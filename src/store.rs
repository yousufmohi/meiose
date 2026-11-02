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
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
        server: String,
        tool: String,
        result: String,
        trimmed: String,
    },
    Call(CallRecord),
    Get {
        id: String,
        reply: std_mpsc::Sender<Option<String>>,
    },
    Flush(std_mpsc::Sender<()>),
}

#[derive(Default)]
struct Memory {
    map: HashMap<String, Arc<str>>,
    order: VecDeque<String>,
    bytes: usize,
}

#[derive(Clone)]
pub struct StoreHandle {
    tx: std_mpsc::Sender<Job>,
    memory: Arc<Mutex<Memory>>,
}

impl StoreHandle {
    /// Open (creating if needed) the database and start the store thread.
    pub fn open(path: &Path) -> anyhow::Result<StoreHandle> {
        let conn = open_db(path)?;
        let (tx, rx) = std_mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("meiose-store".into())
            .spawn(move || run(conn, rx))
            .context("could not start the store thread")?;
        Ok(StoreHandle {
            tx,
            memory: Arc::default(),
        })
    }

    /// Keep an original output so `meiose_expand(id)` can return it.
    pub fn put_output(&self, id: &str, server: &str, tool: &str, result: &str, trimmed: &str) {
        {
            let mut m = self.memory.lock().unwrap_or_else(|e| e.into_inner());
            m.bytes += result.len();
            m.map.insert(id.to_string(), Arc::from(result));
            m.order.push_back(id.to_string());
            let cap = memory_cap();
            while m.bytes > cap && m.order.len() > 1 {
                if let Some(old) = m.order.pop_front()
                    && let Some(v) = m.map.remove(&old)
                {
                    m.bytes -= v.len();
                }
            }
        }
        if keep_days() == 0 {
            return; // memory only, by request
        }
        let _ = self.tx.send(Job::Output {
            id: id.into(),
            server: server.into(),
            tool: tool.into(),
            result: result.into(),
            trimmed: trimmed.into(),
        });
    }

    pub fn record_call(&self, rec: CallRecord) {
        let _ = self.tx.send(Job::Call(rec));
    }

    /// The original `result` JSON for an expand id: memory first, then disk.
    /// Blocking; call from `spawn_blocking` in async code.
    pub fn get_output(&self, id: &str) -> Option<String> {
        if let Some(v) = self
            .memory
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map
            .get(id)
        {
            return Some(v.to_string());
        }
        let (reply, rx) = std_mpsc::channel();
        self.tx
            .send(Job::Get {
                id: id.into(),
                reply,
            })
            .ok()?;
        rx.recv_timeout(Duration::from_secs(10)).ok().flatten()
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
        std::fs::create_dir_all(dir).with_context(|| {
            format!(
                "could not create {}\n  → set MEIOSE_DATA_DIR to a writable directory",
                dir.display()
            )
        })?;
    }
    let conn = Connection::open(path).with_context(|| {
        format!(
            "could not open the meiose database at {}\n  → check permissions, or set MEIOSE_DATA_DIR to a writable directory",
            path.display()
        )
    })?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS outputs(
            id TEXT PRIMARY KEY,
            created_ms INTEGER NOT NULL,
            server TEXT NOT NULL,
            tool TEXT NOT NULL,
            result TEXT NOT NULL,
            trimmed TEXT NOT NULL
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
    Ok(conn)
}

fn run(conn: Connection, rx: std_mpsc::Receiver<Job>) {
    if let Err(e) = prune(&conn) {
        tracing::warn!(error = %e, "store prune failed");
    }
    let mut since_prune = 0u32;
    while let Ok(job) = rx.recv() {
        let res = match job {
            Job::Output {
                id,
                server,
                tool,
                result,
                trimmed,
            } => {
                since_prune += 1;
                conn.execute(
                    "INSERT OR REPLACE INTO outputs(id, created_ms, server, tool, result, trimmed)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![id, now_ms(), server, tool, result, trimmed],
                )
                .map(|_| ())
            }
            Job::Call(rec) => insert_call(&conn, &rec),
            Job::Get { id, reply } => {
                let v = conn
                    .query_row("SELECT result FROM outputs WHERE id = ?1", [&id], |r| {
                        r.get::<_, String>(0)
                    })
                    .optional()
                    .unwrap_or(None);
                let _ = reply.send(v);
                Ok(())
            }
            Job::Flush(done) => {
                let _ = done.send(());
                Ok(())
            }
        };
        if let Err(e) = res {
            tracing::warn!(error = %e, "store write failed");
        }
        if since_prune >= 200 {
            since_prune = 0;
            if let Err(e) = prune(&conn) {
                tracing::warn!(error = %e, "store prune failed");
            }
        }
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

/// Delete expired originals, then the oldest until under the size cap.
/// Call rows are kept for 90 days.
pub fn prune(conn: &Connection) -> rusqlite::Result<()> {
    let cutoff = now_ms() - output_ttl().as_millis() as i64;
    conn.execute("DELETE FROM outputs WHERE created_ms < ?1", [cutoff])?;
    conn.execute(
        "DELETE FROM calls WHERE ts_ms < ?1",
        [now_ms() - 90 * 24 * 3600 * 1000],
    )?;
    let total: i64 = conn.query_row(
        "SELECT COALESCE(SUM(LENGTH(result) + LENGTH(trimmed)), 0) FROM outputs",
        [],
        |r| r.get(0),
    )?;
    if total > OUTPUT_CAP_BYTES {
        // Delete oldest rows until we are under 90% of the cap.
        let mut excess = total - OUTPUT_CAP_BYTES * 9 / 10;
        let mut stmt = conn.prepare(
            "SELECT id, LENGTH(result) + LENGTH(trimmed) FROM outputs ORDER BY created_ms",
        )?;
        let victims: Vec<String> = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .filter_map(Result::ok)
            .take_while(|(_, len)| {
                let take = excess > 0;
                excess -= len;
                take
            })
            .map(|(id, _)| id)
            .collect();
        for id in victims {
            conn.execute("DELETE FROM outputs WHERE id = ?1", [id])?;
        }
    }
    Ok(())
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
        s.put_output("id1", "srv", "tool", original, "{}");
        assert_eq!(s.get_output("id1").as_deref(), Some(original));
        s.flush();

        // A fresh handle (new process) finds it on disk.
        let s2 = StoreHandle::open(&path).unwrap();
        assert_eq!(s2.get_output("id1").as_deref(), Some(original));
        assert_eq!(s2.get_output("missing"), None);
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
    fn prune_enforces_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open_db(&dir.path().join("m.db")).unwrap();
        conn.execute(
            "INSERT INTO outputs VALUES ('old', ?1, 's', 't', 'r', 't')",
            [now_ms() - output_ttl().as_millis() as i64 - 1000],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO outputs VALUES ('new', ?1, 's', 't', 'r', 't')",
            [now_ms()],
        )
        .unwrap();
        prune(&conn).unwrap();
        assert!(output_pair(&conn, "old").unwrap().is_none());
        assert!(output_pair(&conn, "new").unwrap().is_some());
    }
}
