//! Persistent store: original outputs (for `meiose_expand` and `meiose log`)
//! and per-call stats (for `meiose stats`).
//!
//! One SQLite file in the data dir, shared by every meiose process (WAL mode,
//! busy timeout). The proxy talks to it through [`StoreHandle`], which sends
//! work to a dedicated thread so SQLite and token counting never sit on the
//! message path. Originals are also kept in memory for the life of the
//! process, so an expand right after a trim never depends on the disk write.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use rusqlite::{Connection, OptionalExtension, params};

/// Default: originals older than this many days are deleted.
pub const DEFAULT_KEEP_DAYS: u64 = 7;

/// How long original outputs are kept on disk, from `MEIOSE_KEEP_DAYS`
/// (default 7). `0` means originals are never written to disk: they live in
/// memory for the session only (expand still works within it; `meiose log
/// --show` does not).
pub fn keep_days() -> u64 {
    std::env::var("MEIOSE_KEEP_DAYS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_KEEP_DAYS)
}

fn output_ttl() -> Duration {
    Duration::from_secs(keep_days() * 24 * 3600)
}
/// Total size cap for stored originals; oldest go first.
pub const OUTPUT_CAP_BYTES: i64 = 512 * 1024 * 1024;
/// Per-process in-memory cap for originals.
const MEMORY_CAP_BYTES: usize = 32 * 1024 * 1024;
/// With `MEIOSE_KEEP_DAYS=0` memory is the only copy, so keep more of it.
const MEMORY_ONLY_CAP_BYTES: usize = 128 * 1024 * 1024;

/// Originals are also on disk (unless `MEIOSE_KEEP_DAYS=0`), so memory only
/// needs the recent ones; an expand of an older id reads it from SQLite.
fn memory_cap() -> usize {
    if keep_days() == 0 {
        MEMORY_ONLY_CAP_BYTES
    } else {
        MEMORY_CAP_BYTES
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn default_path() -> Option<PathBuf> {
    crate::paths::data_dir().map(|d| d.join("meiose.db"))
}

/// What happened to one `tools/call` (or `meiose_expand`) response.
#[derive(Debug, Clone)]
pub struct CallRecord {
    pub server: String,
    pub tool: String,
    /// "call" or "expand"
    pub kind: &'static str,
    /// Handler that trimmed it, if any ("playwright", "playwright-diff", …).
    pub handler: Option<&'static str>,
    /// Text the agent would have received without meiose.
    pub text_before: String,
    /// Text the agent actually received.
    pub text_after: String,
    pub trim_us: u64,
    pub expand_id: Option<String>,
}

enum Job {
    Output {
        id: String,
        server_key: String,
        server: String,
        tool: String,
        result: String,
        trimmed: String,
    },
    Call(CallRecord),
    Get {
        id: String,
        server_key: String,
        reply: std_mpsc::Sender<Option<String>>,
    },
    Flush(std_mpsc::Sender<()>),
    /// Write everything, close the database, then answer.
    Close(std_mpsc::Sender<()>),
    /// Nothing arrived for a while: only a chance to prune.
    Tick,
    /// Tests: hold the store thread until the sender is dropped.
    #[cfg(test)]
    Pause(std_mpsc::Receiver<()>),
}

/// Jobs waiting for the store thread, at most...
const QUEUE_JOBS: usize = 1024;
/// ...and at most this many bytes of outputs and call texts. Past either,
/// call stats are dropped and outputs are not trimmed (their originals
/// could not be stored).
const QUEUE_BYTES: usize = 64 * 1024 * 1024;

/// A stable identity for one MCP server: a hash of its whole command line
/// and working directory. `meiose_expand` only returns outputs stored under
/// the same identity, so one server (or a page it loaded) cannot read
/// another's outputs by id. The stats label is not enough: it is short and
/// two servers can share it.
pub fn server_key(cmd: &[String]) -> String {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut eat = |b: &[u8]| {
        for &x in b.iter().chain(&[0u8]) {
            h = (h ^ u64::from(x)).wrapping_mul(0x0100_0000_01b3);
        }
    };
    if let Ok(cwd) = std::env::current_dir() {
        eat(cwd.to_string_lossy().as_bytes());
    }
    for a in cmd {
        eat(a.as_bytes());
    }
    format!("{h:016x}")
}

#[derive(Default)]
struct Memory {
    /// id → (server key, original result JSON)
    map: HashMap<String, (String, Arc<str>)>,
    order: VecDeque<String>,
    bytes: usize,
}

#[derive(Clone)]
pub struct StoreHandle {
    tx: std_mpsc::SyncSender<Job>,
    memory: Arc<Mutex<Memory>>,
    /// Bytes of queued jobs not yet written.
    queued: Arc<AtomicUsize>,
}

impl StoreHandle {
    /// Open (creating if needed) the database and start the store thread.
    pub fn open(path: &Path) -> anyhow::Result<StoreHandle> {
        let conn = open_db(path)?;
        let (tx, rx) = std_mpsc::sync_channel::<Job>(QUEUE_JOBS);
        let queued = Arc::new(AtomicUsize::new(0));
        let q = queued.clone();
        std::thread::Builder::new()
            .name("meiose-store".into())
            .spawn(move || run(conn, rx, q))
            .context("could not start the store thread")?;
        Ok(StoreHandle {
            tx,
            memory: Arc::default(),
            queued,
        })
    }

    /// Queue a job of `bytes` without blocking the message path. False if
    /// the queue is full.
    fn try_queue(&self, job: Job, bytes: usize) -> bool {
        let before = self.queued.fetch_add(bytes, Ordering::SeqCst);
        if before + bytes > QUEUE_BYTES || self.tx.try_send(job).is_err() {
            self.queued.fetch_sub(bytes, Ordering::SeqCst);
            return false;
        }
        true
    }

    /// Keep an original output so `meiose_expand(id)` can return it to the
    /// server identified by `server_key`. Returns false when it could not be
    /// kept (the store is backed up): then the output must not be trimmed.
    pub fn put_output(
        &self,
        id: &str,
        server_key: &str,
        server: &str,
        tool: &str,
        result: &str,
        trimmed: &str,
    ) -> bool {
        if keep_days() > 0 {
            let job = Job::Output {
                id: id.into(),
                server_key: server_key.into(),
                server: server.into(),
                tool: tool.into(),
                result: result.into(),
                trimmed: trimmed.into(),
            };
            if !self.try_queue(job, result.len() + trimmed.len()) {
                tracing::warn!(
                    "the store is behind; sending this output untrimmed because its original could not be kept"
                );
                return false;
            }
        }
        let mut m = self.memory.lock().unwrap_or_else(|e| e.into_inner());
        m.bytes += result.len();
        m.map
            .insert(id.to_string(), (server_key.to_string(), Arc::from(result)));
        m.order.push_back(id.to_string());
        let cap = memory_cap();
        while m.bytes > cap && m.order.len() > 1 {
            if let Some(old) = m.order.pop_front()
                && let Some((_, v)) = m.map.remove(&old)
            {
                m.bytes -= v.len();
            }
        }
        true
    }

    /// Stats for one call. Dropped (with a warning) if the store is behind.
    pub fn record_call(&self, rec: CallRecord) {
        let bytes = rec.text_before.len() + rec.text_after.len();
        if !self.try_queue(Job::Call(rec), bytes) {
            tracing::warn!("the store is behind; dropped the stats of one call");
        }
    }

    /// The original `result` JSON for an expand id, if it was stored for
    /// this server: memory first, then disk. Blocking; call from
    /// `spawn_blocking` in async code.
    pub fn get_output(&self, id: &str, server_key: &str) -> Option<String> {
        if let Some((key, v)) = self
            .memory
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map
            .get(id)
        {
            return (key == server_key).then(|| v.to_string());
        }
        let (reply, rx) = std_mpsc::channel();
        self.tx
            .send(Job::Get {
                id: id.into(),
                server_key: server_key.into(),
                reply,
            })
            .ok()?;
        rx.recv_timeout(Duration::from_secs(10)).ok().flatten()
    }

    /// Write every job sent so far and close the database, so the WAL is
    /// checkpointed and removed (meiose exits with `process::exit`, which
    /// runs no destructors). Call once, at shutdown.
    pub fn close(&self) {
        let (tx, rx) = std_mpsc::channel();
        if self.tx.send(Job::Close(tx)).is_ok() {
            let _ = rx.recv_timeout(Duration::from_secs(30));
        }
    }

    /// Wait until every job sent so far is written (used at shutdown/tests).
    pub fn flush(&self) {
        let (tx, rx) = std_mpsc::channel();
        if self.tx.send(Job::Flush(tx)).is_ok() {
            let _ = rx.recv_timeout(Duration::from_secs(30));
        }
    }
}

pub fn open_db(path: &Path) -> anyhow::Result<Connection> {
    if let Some(dir) = path.parent() {
        crate::secure::create_dir(dir).with_context(|| {
            format!(
                "could not create {}\n  → set MEIOSE_DATA_DIR to a writable directory",
                dir.display()
            )
        })?;
    }
    // Create the file owner-only before SQLite does: SQLite gives the WAL
    // and SHM files the database file's permissions. A failure here shows
    // up as a clearer error from `Connection::open` just below.
    let _ = crate::secure::touch_private(path);
    let conn = Connection::open(path).with_context(|| {
        format!(
            "could not open the meiose database at {}\n  → check permissions, or set MEIOSE_DATA_DIR to a writable directory",
            path.display()
        )
    })?;
    conn.busy_timeout(Duration::from_secs(5))?;
    // Only takes effect on a new, empty database (older ones are converted
    // by a VACUUM the first time they go over the cap): lets a prune hand
    // freed pages back to the file system.
    conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    // Deleted rows are overwritten with zeros, so a pruned output is gone
    // from the file, not just unlinked from the table.
    conn.pragma_update(None, "secure_delete", "ON")?;
    // After a checkpoint the WAL file is cut back to this size.
    conn.pragma_update(None, "journal_size_limit", 16 * 1024 * 1024)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS outputs(
            id TEXT PRIMARY KEY,
            created_ms INTEGER NOT NULL,
            server TEXT NOT NULL,
            tool TEXT NOT NULL,
            result TEXT NOT NULL,
            trimmed TEXT NOT NULL,
            server_key TEXT NOT NULL DEFAULT ''
         );
         CREATE INDEX IF NOT EXISTS outputs_created ON outputs(created_ms);
         CREATE TABLE IF NOT EXISTS calls(
            ts_ms INTEGER NOT NULL,
            server TEXT NOT NULL,
            tool TEXT NOT NULL,
            kind TEXT NOT NULL,
            handler TEXT,
            bytes_before INTEGER NOT NULL,
            bytes_after INTEGER NOT NULL,
            tokens_before INTEGER NOT NULL,
            tokens_after INTEGER NOT NULL,
            trim_us INTEGER NOT NULL,
            expand_id TEXT
         );
         CREATE INDEX IF NOT EXISTS calls_ts ON calls(ts_ms);",
    )
    .context("could not create the meiose database tables")?;
    migrate(&conn).context("could not upgrade the meiose database")?;
    Ok(conn)
}

/// Databases from before 0.3 have no `server_key`. Their outputs get an
/// empty key, which no server has, so they can no longer be expanded (they
/// expire within days) but `meiose log --show` still finds them. Older
/// meiose versions keep working on the same file: they name their columns.
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let has_key = |conn: &Connection| -> rusqlite::Result<bool> {
        conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('outputs') WHERE name = 'server_key'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
    };
    if has_key(conn)? {
        return Ok(());
    }
    match conn.execute_batch("ALTER TABLE outputs ADD COLUMN server_key TEXT NOT NULL DEFAULT ''") {
        Ok(()) => Ok(()),
        // Another meiose process added it first.
        Err(e) if has_key(conn)? => {
            tracing::debug!(error = %e, "server_key column added concurrently");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// A running proxy prunes after this many stored outputs...
const PRUNE_EVERY_OUTPUTS: u32 = 200;
/// ...or this many bytes of them (so big outputs cannot outrun the cap)...
const PRUNE_EVERY_BYTES: usize = 16 * 1024 * 1024;
/// ...or this long after its last prune, whichever comes first.
const PRUNE_EVERY: Duration = Duration::from_secs(3600);

fn prune_due(since_prune: u32, bytes_since: usize, last_prune: Instant) -> bool {
    since_prune >= PRUNE_EVERY_OUTPUTS
        || bytes_since >= PRUNE_EVERY_BYTES
        || last_prune.elapsed() >= PRUNE_EVERY
}

fn run(conn: Connection, rx: std_mpsc::Receiver<Job>, queued: Arc<AtomicUsize>) {
    if let Err(e) = prune(&conn) {
        tracing::warn!(error = %e, "store prune failed");
    }
    let mut since_prune = 0u32;
    let mut bytes_since = 0usize;
    let mut last_prune = Instant::now();
    let mut closed = None;
    let db_path = conn.path().filter(|p| !p.is_empty()).map(PathBuf::from);
    loop {
        let job = match rx.recv_timeout(PRUNE_EVERY) {
            Ok(job) => job,
            Err(std_mpsc::RecvTimeoutError::Timeout) => Job::Tick,
            Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let res = match job {
            Job::Tick => Ok(()),
            Job::Output {
                id,
                server_key,
                server,
                tool,
                result,
                trimmed,
            } => {
                let bytes = result.len() + trimmed.len();
                since_prune += 1;
                bytes_since += bytes;
                let res = conn
                    .execute(
                        "INSERT OR REPLACE INTO outputs(id, created_ms, server, tool, result, trimmed, server_key)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![id, now_ms(), server, tool, result, trimmed, server_key],
                    )
                    .map(|_| ());
                queued.fetch_sub(bytes, Ordering::SeqCst);
                // Several meiose processes writing at once can keep the WAL
                // from ever resetting; past a limit, wait for a checkpoint.
                if db_path
                    .as_deref()
                    .is_some_and(|p| wal_bytes(p) > 4 * WAL_LIMIT)
                {
                    let _ = checkpoint(&conn);
                }
                res
            }
            Job::Call(rec) => {
                let res = insert_call(&conn, &rec);
                queued.fetch_sub(
                    rec.text_before.len() + rec.text_after.len(),
                    Ordering::SeqCst,
                );
                res
            }
            Job::Get {
                id,
                server_key,
                reply,
            } => {
                let v = conn
                    .query_row(
                        "SELECT result FROM outputs WHERE id = ?1 AND server_key = ?2",
                        [&id, &server_key],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()
                    .unwrap_or(None);
                let _ = reply.send(v);
                Ok(())
            }
            Job::Flush(done) => {
                let _ = done.send(());
                Ok(())
            }
            Job::Close(done) => {
                closed = Some(done);
                break;
            }
            #[cfg(test)]
            Job::Pause(until) => {
                let _ = until.recv();
                Ok(())
            }
        };
        if let Err(e) = res {
            tracing::warn!(error = %e, "store write failed");
        }
        if prune_due(since_prune, bytes_since, last_prune) {
            since_prune = 0;
            bytes_since = 0;
            last_prune = Instant::now();
            if let Err(e) = prune(&conn) {
                tracing::warn!(error = %e, "store prune failed");
            }
        }
    }
    if let Some(done) = closed {
        if let Err((_, e)) = conn.close() {
            tracing::warn!(error = %e, "closing the store failed");
        }
        let _ = done.send(());
    }
}

fn insert_call(conn: &Connection, rec: &CallRecord) -> rusqlite::Result<()> {
    let tokens_before = crate::tokens::for_stats(&rec.text_before) as i64;
    let tokens_after = if rec.text_after == rec.text_before {
        tokens_before
    } else {
        crate::tokens::for_stats(&rec.text_after) as i64
    };
    conn.execute(
        "INSERT INTO calls(ts_ms, server, tool, kind, handler, bytes_before, bytes_after,
                           tokens_before, tokens_after, trim_us, expand_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            now_ms(),
            rec.server,
            rec.tool,
            rec.kind,
            rec.handler,
            rec.text_before.len() as i64,
            rec.text_after.len() as i64,
            tokens_before,
            tokens_after,
            rec.trim_us as i64,
            rec.expand_id,
        ],
    )
    .map(|_| ())
}

/// Delete expired originals, then the oldest until the database files
/// (database + WAL, measured on disk) fit the cap. Call rows are kept for
/// 90 days. With `secure_delete` the freed pages are zeroed; the WAL, which
/// still holds copies of them, is then checkpointed and truncated (fully
/// only if no other meiose process is mid-read), and freed pages are given
/// back to the file system.
pub fn prune(conn: &Connection) -> rusqlite::Result<()> {
    prune_to(conn, OUTPUT_CAP_BYTES as u64)
}

/// [`prune`] with a given cap, in bytes of disk.
pub fn prune_to(conn: &Connection, cap: u64) -> rusqlite::Result<()> {
    let cutoff = now_ms() - output_ttl().as_millis() as i64;
    let mut deleted = conn.execute("DELETE FROM outputs WHERE created_ms < ?1", [cutoff])?;
    conn.execute(
        "DELETE FROM calls WHERE ts_ms < ?1",
        [now_ms() - 90 * 24 * 3600 * 1000],
    )?;
    let Some(path) = conn.path().filter(|p| !p.is_empty()).map(PathBuf::from) else {
        return Ok(());
    };
    // Rows are deleted until the database's own content fits 90% of the cap,
    // less room for the WAL. A WAL that other meiose processes keep busy is
    // truncated by a later prune; deleting rows would not shrink it.
    let target = (cap * 9 / 10).saturating_sub(WAL_LIMIT.min(cap / 8));
    for _ in 0..8 {
        if deleted > 0 || disk_usage(&path) > cap || wal_bytes(&path) > WAL_LIMIT {
            reclaim(conn, &path, cap)?;
        }
        if disk_usage(&path) <= cap {
            break;
        }
        let content = content_bytes(conn)?;
        if content <= target {
            break;
        }
        let mut excess = (content - target) as i64;
        let victims: Vec<String> = conn
            .prepare("SELECT id, LENGTH(CAST(result AS BLOB)) + LENGTH(CAST(trimmed AS BLOB)) FROM outputs ORDER BY created_ms")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .filter_map(Result::ok)
            .take_while(|(_, len)| {
                let take = excess > 0;
                excess -= len;
                take
            })
            .map(|(id, _)| id)
            .collect();
        if victims.is_empty() {
            break; // nothing left to delete: the rest is stats and overhead
        }
        deleted = 0;
        for id in victims {
            deleted += conn.execute("DELETE FROM outputs WHERE id = ?1", [id])?;
        }
    }
    Ok(())
}

/// The WAL may stay this big between checkpoints (`journal_size_limit`).
const WAL_LIMIT: u64 = 16 * 1024 * 1024;

/// Bytes of pages in use in the database (its size less free pages).
fn content_bytes(conn: &Connection) -> rusqlite::Result<u64> {
    let pages: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
    let free: i64 = conn.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
    let size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    Ok(((pages - free).max(0) * size) as u64)
}

fn wal_bytes(db: &Path) -> u64 {
    let mut p = db.as_os_str().to_owned();
    p.push("-wal");
    std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0)
}

/// Checkpoint and truncate the WAL; then, if the files are still over the
/// cap, give free pages back to the file system (converting an older
/// database to incremental auto-vacuum once). Under the cap free pages are
/// left for new outputs to reuse: moving pages costs WAL writes. Freed
/// pages are zeroed either way (`secure_delete`). The checkpoint waits up
/// to the busy timeout for other meiose processes.
fn reclaim(conn: &Connection, path: &Path, cap: u64) -> rusqlite::Result<()> {
    checkpoint(conn)?;
    let free: i64 = conn.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
    if free > 0 && disk_usage(path) > cap {
        let mode: i64 = conn.query_row("PRAGMA auto_vacuum", [], |r| r.get(0))?;
        if mode == 2 {
            // Each step frees one page: step it to the end.
            let mut stmt = conn.prepare("PRAGMA incremental_vacuum")?;
            let mut rows = stmt.query([])?;
            while rows.next()?.is_some() {}
        } else {
            conn.execute_batch("PRAGMA auto_vacuum = INCREMENTAL; VACUUM")?;
        }
        checkpoint(conn)?;
    }
    Ok(())
}

/// `wal_checkpoint(TRUNCATE)`; logs when other processes kept it from
/// finishing (a later prune retries).
fn checkpoint(conn: &Connection) -> rusqlite::Result<()> {
    let busy: i64 = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))?;
    if busy != 0 {
        tracing::debug!(
            "WAL checkpoint could not finish (other meiose processes busy); will retry"
        );
    }
    Ok(())
}

/// Bytes the database takes on disk: the file, its WAL and its SHM.
pub fn disk_usage(db: &Path) -> u64 {
    let mut total = 0;
    for suffix in ["", "-wal", "-shm"] {
        let mut p = db.as_os_str().to_owned();
        p.push(suffix);
        total += std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
    }
    total
}

/// (number of stored outputs, their total bytes).
pub fn output_totals(conn: &Connection) -> rusqlite::Result<(i64, i64)> {
    conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(LENGTH(result) + LENGTH(trimmed)), 0) FROM outputs",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
}

/// Delete every stored output and leave no copy in the database files:
/// zeroed pages (`secure_delete`), a VACUUM, then a truncated WAL.
pub fn purge_outputs(conn: &Connection) -> rusqlite::Result<usize> {
    let n = conn.execute("DELETE FROM outputs", [])?;
    conn.execute_batch("VACUUM")?;
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    Ok(n)
}

// ---------- queries for `meiose stats` / `meiose log` ----------

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Totals {
    pub calls: i64,
    pub trimmed_calls: i64,
    pub tokens_before: i64,
    pub tokens_after: i64,
    pub expands: i64,
    pub expand_tokens: i64,
}

pub fn totals_since(conn: &Connection, since_ms: i64) -> rusqlite::Result<Totals> {
    conn.query_row(
        "SELECT
            COALESCE(SUM(kind = 'call'), 0),
            COALESCE(SUM(kind = 'call' AND handler IS NOT NULL), 0),
            COALESCE(SUM(CASE WHEN kind = 'call' THEN tokens_before END), 0),
            COALESCE(SUM(CASE WHEN kind = 'call' THEN tokens_after END), 0),
            COALESCE(SUM(kind = 'expand'), 0),
            COALESCE(SUM(CASE WHEN kind = 'expand' THEN tokens_after END), 0)
         FROM calls WHERE ts_ms >= ?1",
        [since_ms],
        |r| {
            Ok(Totals {
                calls: r.get(0)?,
                trimmed_calls: r.get(1)?,
                tokens_before: r.get(2)?,
                tokens_after: r.get(3)?,
                expands: r.get(4)?,
                expand_tokens: r.get(5)?,
            })
        },
    )
}

#[derive(Debug, Clone)]
pub struct ToolTotals {
    pub server: String,
    pub tool: String,
    pub calls: i64,
    pub tokens_before: i64,
    pub tokens_after: i64,
}

pub fn per_tool_since(conn: &Connection, since_ms: i64) -> rusqlite::Result<Vec<ToolTotals>> {
    let mut stmt = conn.prepare(
        "SELECT server, tool, COUNT(*), SUM(tokens_before), SUM(tokens_after)
         FROM calls WHERE ts_ms >= ?1 AND kind = 'call'
         GROUP BY server, tool ORDER BY SUM(tokens_before) DESC",
    )?;
    stmt.query_map([since_ms], |r| {
        Ok(ToolTotals {
            server: r.get(0)?,
            tool: r.get(1)?,
            calls: r.get(2)?,
            tokens_before: r.get(3)?,
            tokens_after: r.get(4)?,
        })
    })?
    .collect()
}

#[derive(Debug, Clone)]
pub struct CallRow {
    pub ts_ms: i64,
    pub server: String,
    pub tool: String,
    pub kind: String,
    pub handler: Option<String>,
    pub tokens_before: i64,
    pub tokens_after: i64,
    pub trim_us: i64,
    pub expand_id: Option<String>,
}

pub fn recent_calls(conn: &Connection, limit: usize) -> rusqlite::Result<Vec<CallRow>> {
    let mut stmt = conn.prepare(
        "SELECT ts_ms, server, tool, kind, handler, tokens_before, tokens_after, trim_us, expand_id
         FROM calls ORDER BY ts_ms DESC, rowid DESC LIMIT ?1",
    )?;
    stmt.query_map([limit as i64], |r| {
        Ok(CallRow {
            ts_ms: r.get(0)?,
            server: r.get(1)?,
            tool: r.get(2)?,
            kind: r.get(3)?,
            handler: r.get(4)?,
            tokens_before: r.get(5)?,
            tokens_after: r.get(6)?,
            trim_us: r.get(7)?,
            expand_id: r.get(8)?,
        })
    })?
    .collect()
}

/// (original result JSON, trimmed result JSON) for an expand id.
pub fn output_pair(conn: &Connection, id: &str) -> rusqlite::Result<Option<(String, String)>> {
    conn.query_row(
        "SELECT result, trimmed FROM outputs WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(tool: &str, before: &str, after: &str, handler: Option<&'static str>) -> CallRecord {
        CallRecord {
            server: "srv".into(),
            tool: tool.into(),
            kind: "call",
            handler,
            text_before: before.into(),
            text_after: after.into(),
            trim_us: 10,
            expand_id: handler.map(|_| "abc".into()),
        }
    }

    #[test]
    fn outputs_round_trip_through_memory_and_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db");
        let s = StoreHandle::open(&path).unwrap();
        let original = r#"{"content":[{"type":"text","text":"é  x"}]}"#;
        assert!(s.put_output("id1", "key-a", "srv", "tool", original, "{}"));
        assert_eq!(s.get_output("id1", "key-a").as_deref(), Some(original));
        s.flush();

        // A fresh handle (new process) finds it on disk.
        let s2 = StoreHandle::open(&path).unwrap();
        assert_eq!(s2.get_output("id1", "key-a").as_deref(), Some(original));
        assert_eq!(s2.get_output("missing", "key-a"), None);
    }

    #[test]
    fn outputs_are_only_returned_to_the_server_that_stored_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db");
        let s = StoreHandle::open(&path).unwrap();
        assert!(s.put_output("id1", "key-a", "srv", "tool", "{}", "{}"));
        // In memory (same process)...
        assert_eq!(s.get_output("id1", "key-b"), None);
        s.flush();
        // ...and on disk (another process).
        let s2 = StoreHandle::open(&path).unwrap();
        assert_eq!(s2.get_output("id1", "key-b"), None);
        assert_eq!(s2.get_output("id1", "").as_deref(), None);
        assert!(s2.get_output("id1", "key-a").is_some());
    }

    #[test]
    fn server_keys_tell_apart_commands_that_share_a_label() {
        let cmd = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let work = server_key(&cmd(&["srv", "--profile", "work"]));
        assert_eq!(work, server_key(&cmd(&["srv", "--profile", "work"])));
        assert_ne!(work, server_key(&cmd(&["srv", "--profile", "home"])));
        // Argument boundaries count: ["ab", "c"] is not ["a", "bc"].
        assert_ne!(
            server_key(&cmd(&["ab", "c"])),
            server_key(&cmd(&["a", "bc"]))
        );
    }

    #[test]
    fn a_full_queue_refuses_outputs_and_drops_stats_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db");
        let s = StoreHandle::open(&path).unwrap();
        let (hold, until) = std_mpsc::channel();
        s.tx.send(Job::Pause(until)).unwrap();
        let big = "x".repeat(QUEUE_BYTES / 4);
        let mut stored = 0;
        let started = Instant::now();
        for i in 0..8 {
            if s.put_output(&format!("id{i}"), "k", "srv", "t", &big, "{}") {
                stored += 1;
            }
            s.record_call(rec("t", &big, "x", None));
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the queue blocked"
        );
        assert!(stored < 8, "the queue must refuse once full");
        assert!(stored >= 1);
        // A refused output is not in memory either: nothing to expand.
        assert_eq!(s.get_output("id7", "k"), None);
        drop(hold);
        s.flush();
        assert!(s.queued.load(Ordering::SeqCst) == 0);
        let conn = open_db(&path).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM outputs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, stored);
        // Room again once the store caught up.
        assert!(s.put_output("later", "k", "srv", "t", "{}", "{}"));
    }

    #[test]
    fn prune_keeps_the_files_on_disk_under_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db");
        let conn = open_db(&path).unwrap();
        let blob = "y".repeat(256 * 1024);
        for i in 0..80 {
            conn.execute(
                "INSERT INTO outputs(id, created_ms, server, tool, result, trimmed, server_key)
                 VALUES (?1, ?2, 's', 't', ?3, '{}', 'k')",
                params![format!("id{i}"), now_ms() + i, blob],
            )
            .unwrap();
        }
        assert!(disk_usage(&path) > 20 * 1024 * 1024);
        let cap = 8 * 1024 * 1024;
        prune_to(&conn, cap).unwrap();
        assert!(disk_usage(&path) <= cap, "{} > {cap}", disk_usage(&path));
        // The newest outputs are the ones kept.
        assert!(output_pair(&conn, "id79").unwrap().is_some());
        assert!(output_pair(&conn, "id0").unwrap().is_none());
    }

    #[test]
    fn prune_shrinks_a_database_from_before_auto_vacuum() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db");
        {
            // The pre-0.3 layout: no auto_vacuum, no server_key.
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "PRAGMA journal_mode=WAL;
                 CREATE TABLE outputs(id TEXT PRIMARY KEY, created_ms INTEGER NOT NULL,
                    server TEXT NOT NULL, tool TEXT NOT NULL, result TEXT NOT NULL,
                    trimmed TEXT NOT NULL);",
            )
            .unwrap();
            let blob = "z".repeat(256 * 1024);
            for i in 0..80 {
                c.execute(
                    "INSERT INTO outputs VALUES (?1, ?2, 's', 't', ?3, '{}')",
                    params![format!("id{i}"), now_ms() + i, blob],
                )
                .unwrap();
            }
        }
        let conn = open_db(&path).unwrap();
        // Migrated: old rows have the empty key, which no server has.
        let key: String = conn
            .query_row("SELECT server_key FROM outputs WHERE id = 'id0'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(key, "");
        let cap = 8 * 1024 * 1024;
        prune_to(&conn, cap).unwrap();
        assert!(disk_usage(&path) <= cap, "{} > {cap}", disk_usage(&path));
        let mode: i64 = conn
            .query_row("PRAGMA auto_vacuum", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, 2, "converted to incremental auto-vacuum");
    }

    #[test]
    fn stats_aggregate_calls_and_expands() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db");
        let s = StoreHandle::open(&path).unwrap();
        s.record_call(rec(
            "snap",
            &"word ".repeat(1000),
            "word word",
            Some("playwright"),
        ));
        s.record_call(rec("other", "same", "same", None));
        let mut e = rec("meiose_expand", "", &"word ".repeat(100), None);
        e.kind = "expand";
        s.record_call(e);
        s.flush();

        let conn = open_db(&path).unwrap();
        let t = totals_since(&conn, 0).unwrap();
        assert_eq!(t.calls, 2);
        assert_eq!(t.trimmed_calls, 1);
        assert_eq!(t.expands, 1);
        assert!(t.tokens_before > 1000 && t.tokens_after < 10, "{t:?}");
        assert!(t.expand_tokens >= 100);
        let per = per_tool_since(&conn, 0).unwrap();
        assert_eq!(per[0].tool, "snap");
        let recent = recent_calls(&conn, 10).unwrap();
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].kind, "expand");
    }

    #[test]
    fn prune_runs_by_count_bytes_or_time() {
        let now = Instant::now();
        assert!(!prune_due(0, 0, now));
        assert!(prune_due(PRUNE_EVERY_OUTPUTS, 0, now));
        assert!(prune_due(1, PRUNE_EVERY_BYTES, now));
        if let Some(long_ago) = now.checked_sub(PRUNE_EVERY + Duration::from_secs(1)) {
            assert!(prune_due(1, 0, long_ago));
        }
    }

    #[test]
    fn prune_enforces_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open_db(&dir.path().join("m.db")).unwrap();
        conn.execute(
            "INSERT INTO outputs(id, created_ms, server, tool, result, trimmed)
             VALUES ('old', ?1, 's', 't', 'r', 't')",
            [now_ms() - output_ttl().as_millis() as i64 - 1000],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO outputs(id, created_ms, server, tool, result, trimmed)
             VALUES ('new', ?1, 's', 't', 'r', 't')",
            [now_ms()],
        )
        .unwrap();
        prune(&conn).unwrap();
        assert!(output_pair(&conn, "old").unwrap().is_none());
        assert!(output_pair(&conn, "new").unwrap().is_some());
    }
}
