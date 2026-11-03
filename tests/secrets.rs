//! Secrets on disk: config modes survive install/uninstall, the data
//! directory is private, backups go away when they are no longer needed,
//! server arguments never reach logs, stats or filenames, and pruned
//! originals are really gone from the database files.
//!
//! The permission tests are Unix-only: on Windows meiose keeps the default
//! ACLs of the user's profile directories.

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use common::session::{Session, expand_id, text_of};
use serde_json::json;

const HN: &str = "tests/fixtures/playwright/news-ycombinator-com.txt";
const FAKE_MEIOSE: &str = "/opt/meiose/bin/meiose";
const CONFIG: &str = r#"{
  "mcpServers": {
    "playwright": {
      "command": "npx",
      "args": ["-y", "@playwright/mcp@latest"],
      "env": { "API_TOKEN": "s3cret-do-not-print" }
    }
  }
}
"#;

struct Env {
    home: tempfile::TempDir,
    data: tempfile::TempDir,
}

impl Env {
    fn new() -> Env {
        let e = Env {
            home: tempfile::tempdir().unwrap(),
            data: tempfile::tempdir().unwrap(),
        };
        std::fs::write(e.config(), CONFIG).unwrap();
        e
    }

    fn config(&self) -> PathBuf {
        self.home.path().join(".claude.json")
    }

    fn install_dir(&self) -> PathBuf {
        self.data.path().join("install")
    }

    fn meiose(&self, args: &[&str], stdin: Option<&str>) -> (Output, String) {
        meiose_in(self.home.path(), self.data.path(), args, stdin)
    }

    fn install(&self) {
        let (out, text) = self.meiose(&["install", "--yes", "--meiose-path", FAKE_MEIOSE], None);
        assert!(out.status.success(), "{text}");
    }

    fn backups(&self) -> Vec<PathBuf> {
        files_in(&self.install_dir())
            .into_iter()
            .filter(|p| p.file_name().unwrap().to_string_lossy() != "manifest.json")
            .collect()
    }
}

fn meiose_in(home: &Path, data: &Path, args: &[&str], stdin: Option<&str>) -> (Output, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_meiose"))
        .args(args)
        .env("MEIOSE_FAKE_HOME", home)
        .env("MEIOSE_DATA_DIR", data)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    if let Some(s) = stdin {
        input.write_all(s.as_bytes()).unwrap();
    }
    drop(input);
    let out = child.wait_with_output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out, text)
}

fn files_in(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(Result::ok).map(|e| e.path()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// Every file under `dir`, recursively.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in files_in(dir) {
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[cfg(unix)]
fn mode(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[cfg(unix)]
fn set_mode(p: &Path, m: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap();
}

// ---------------- 1. config modes ----------------

#[cfg(unix)]
#[test]
fn install_and_uninstall_keep_the_config_mode() {
    for m in [0o600, 0o640] {
        let env = Env::new();
        set_mode(&env.config(), m);
        env.install();
        assert!(
            std::fs::read_to_string(env.config())
                .unwrap()
                .contains(FAKE_MEIOSE)
        );
        assert_eq!(mode(&env.config()), m, "install changed the config mode");
        let (out, text) = env.meiose(&["uninstall", "--yes"], None);
        assert!(out.status.success(), "{text}");
        assert_eq!(std::fs::read_to_string(env.config()).unwrap(), CONFIG);
        assert_eq!(mode(&env.config()), m, "uninstall changed the config mode");
        // No temp file left behind.
        assert_eq!(files_in(env.home.path()), [env.config()]);
    }
}

#[cfg(unix)]
#[test]
fn backups_and_the_install_dir_are_private() {
    let env = Env::new();
    env.install();
    assert_eq!(mode(&env.install_dir()), 0o700);
    let files = files_in(&env.install_dir());
    assert!(files.len() >= 3, "{files:?}"); // backup, written, manifest
    for f in files {
        assert_eq!(mode(&f), 0o600, "{}", f.display());
    }
}

#[cfg(unix)]
#[test]
fn a_failed_config_write_leaves_no_temp_file() {
    let env = Env::new();
    // A read-only home: the temp file next to the config cannot be created.
    set_mode(env.home.path(), 0o500);
    let (out, text) = env.meiose(&["install", "--yes", "--meiose-path", FAKE_MEIOSE], None);
    set_mode(env.home.path(), 0o700);
    assert!(!out.status.success(), "{text}");
    assert_eq!(files_in(env.home.path()), [env.config()]);
    assert_eq!(std::fs::read_to_string(env.config()).unwrap(), CONFIG);
}

// ---------------- 2. private data directory ----------------

#[cfg(unix)]
#[test]
fn the_data_directory_is_private() {
    let mut s = Session::start(HN, &[]);
    s.init();
    let snap = s.call("browser_snapshot", json!({}));
    let eid = expand_id(text_of(&snap));
    s.call("meiose_expand", json!({"id": eid}));
    // Check while the proxy still runs (the WAL and SHM files exist), then after.
    let data = s.data.path().to_path_buf();
    let check = |data: &Path| {
        assert_eq!(mode(data), 0o700, "data dir");
        assert_eq!(mode(&data.join("logs")), 0o700, "logs dir");
        let mut seen = 0;
        for f in walk(data) {
            let name = f.file_name().unwrap().to_string_lossy().into_owned();
            if name.starts_with("server-") {
                continue; // the fake server's own logs
            }
            seen += 1;
            assert_eq!(mode(&f), 0o600, "{}", f.display());
        }
        assert!(seen >= 2, "expected the database and a log file");
    };
    check(&data);
    let data = s.finish();
    check(data.path());
}

#[cfg(unix)]
#[test]
fn an_existing_data_directory_is_tightened_on_startup() {
    let data = tempfile::tempdir().unwrap();
    let d = data.path();
    for sub in ["logs", "recordings", "install"] {
        std::fs::create_dir_all(d.join(sub)).unwrap();
        set_mode(&d.join(sub), 0o755);
    }
    let files = [
        d.join("meiose.db"),
        d.join("logs/meiose.2026-01-01.log"),
        d.join("recordings/1-npx.jsonl"),
        d.join("install/claude-code-1.backup.json"),
    ];
    {
        let conn = rusqlite::Connection::open(&files[0]).unwrap();
        conn.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (1);")
            .unwrap();
    }
    for f in &files[1..] {
        std::fs::write(f, "x").unwrap();
    }
    for f in &files {
        set_mode(f, 0o644);
    }
    set_mode(d, 0o755);

    let home = tempfile::tempdir().unwrap();
    let (out, text) = meiose_in(home.path(), d, &["stats"], None);
    assert!(out.status.success(), "{text}");
    assert_eq!(mode(d), 0o700);
    for sub in ["logs", "recordings", "install"] {
        assert_eq!(mode(&d.join(sub)), 0o700, "{sub}");
    }
    for f in walk(d) {
        assert_eq!(mode(&f), 0o600, "{}", f.display());
    }
}

#[cfg(unix)]
#[test]
fn recordings_are_private_even_in_a_custom_directory() {
    let out_dir = tempfile::tempdir().unwrap();
    let out = out_dir.path().join("new-subdir");
    let s = Session::start_full("record", HN, &["--out", out.to_str().unwrap()], &[], &[]);
    let data = s.finish();
    let files = files_in(&out);
    assert_eq!(files.len(), 1);
    assert_eq!(mode(&files[0]), 0o600);
    assert_eq!(mode(&out), 0o700, "a directory meiose creates is private");
    drop(data);
}

// ---------------- 3. backups go away ----------------

#[test]
fn uninstall_deletes_the_backups_once_the_config_is_restored() {
    let env = Env::new();
    env.install();
    assert_eq!(env.backups().len(), 2, "a backup and a written copy");
    let (out, text) = env.meiose(&["uninstall", "--yes"], None);
    assert!(out.status.success(), "{text}");
    assert_eq!(std::fs::read_to_string(env.config()).unwrap(), CONFIG);
    assert!(env.backups().is_empty(), "{:?}", env.backups());
    assert!(!text.contains("Backups are kept"), "{text}");
}

#[test]
fn uninstall_deletes_the_backups_once_servers_are_unwrapped() {
    let env = Env::new();
    env.install();
    // The agent rewrote the file since: uninstall unwraps instead.
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(env.config()).unwrap()).unwrap();
    v["numStartups"] = json!(3);
    std::fs::write(env.config(), serde_json::to_string_pretty(&v).unwrap()).unwrap();
    let (out, text) = env.meiose(&["uninstall", "--yes"], None);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("✓ unwrapped 1 server(s)"), "{text}");
    assert!(env.backups().is_empty(), "{:?}", env.backups());
}

#[test]
fn uninstall_keep_backups_keeps_them() {
    let env = Env::new();
    env.install();
    let (out, text) = env.meiose(&["uninstall", "--yes", "--keep-backups"], None);
    assert!(out.status.success(), "{text}");
    assert_eq!(env.backups().len(), 2);
    assert!(text.contains("Backups are kept"), "{text}");
}

#[test]
fn backups_stay_when_undo_fails() {
    let env = Env::new();
    env.install();
    std::fs::write(env.config(), "{ not json").unwrap();
    let (out, text) = env.meiose(&["uninstall", "--yes"], None);
    assert!(!out.status.success(), "{text}");
    assert_eq!(env.backups().len(), 2, "{text}");
    let backup = env
        .backups()
        .into_iter()
        .find(|p| p.to_string_lossy().ends_with(".backup.json"))
        .unwrap();
    assert_eq!(std::fs::read_to_string(backup).unwrap(), CONFIG);
}

// ---------------- 3. purge ----------------

/// A data dir with a stored output, a log, a recording and a leftover backup.
fn populated() -> (Env, String) {
    let env = Env::new();
    // The env var given here wins over the session's own data dir.
    let mut s = Session::start_full(
        "record",
        HN,
        &[],
        &[("MEIOSE_DATA_DIR", env.data.path().to_str().unwrap())],
        &[],
    );
    s.init();
    let snap = s.call("browser_snapshot", json!({}));
    let eid = expand_id(text_of(&snap));
    s.finish();
    std::fs::create_dir_all(env.install_dir()).unwrap();
    std::fs::write(
        env.install_dir().join("cursor-0123.backup.json"),
        "leftover",
    )
    .unwrap();
    (env, eid)
}

fn stored_outputs(data: &Path) -> i64 {
    let conn = rusqlite::Connection::open(data.join("meiose.db")).unwrap();
    conn.query_row("SELECT COUNT(*) FROM outputs", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn purge_lists_what_it_deletes_and_asks_first() {
    let (env, _) = populated();
    let (out, text) = env.meiose(&["purge"], Some("n\n"));
    assert!(!out.status.success(), "{text}");
    for what in ["stored output", "log file", "recording", "leftover backup"] {
        assert!(text.contains(what), "purge must list {what}: {text}");
    }
    assert!(text.contains("Nothing deleted"), "{text}");
    assert_eq!(stored_outputs(env.data.path()), 1);
    assert_eq!(files_in(&env.data.path().join("recordings")).len(), 1);
}

#[test]
fn purge_yes_deletes_outputs_logs_recordings_and_leftover_backups() {
    let (env, eid) = populated();
    let (out, text) = env.meiose(&["purge", "--yes"], None);
    assert!(out.status.success(), "{text}");
    assert_eq!(stored_outputs(env.data.path()), 0);
    assert!(walk(&env.data.path().join("logs")).is_empty());
    assert!(walk(&env.data.path().join("recordings")).is_empty());
    assert!(env.backups().is_empty());
    let (_, show) = env.meiose(&["log", "--show", &eid], None);
    assert!(show.contains("no stored output"), "{show}");
}

#[test]
fn purge_keeps_the_backups_of_configs_that_are_still_wrapped() {
    let env = Env::new();
    env.install();
    let (out, text) = env.meiose(&["purge", "--yes"], None);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("meiose uninstall"), "{text}");
    assert_eq!(env.backups().len(), 2, "{text}");
    let (out, text) = env.meiose(&["uninstall", "--yes"], None);
    assert!(out.status.success(), "{text}");
    assert_eq!(std::fs::read_to_string(env.config()).unwrap(), CONFIG);
}

// ---------------- 4. server arguments ----------------

const API_KEY: &str = "sk-live-0123456789abcdef";
const DB_PASSWORD: &str = "hunter2pass";

#[test]
fn server_arguments_never_reach_logs_stats_or_filenames() {
    let db_url = format!("postgres://admin:{DB_PASSWORD}@db.internal/shop");
    for (sub, flags) in [("run", vec![]), ("record", vec![])] {
        // The secret is the first non-flag argument, and a flag value.
        let mut s = Session::start_full(
            sub,
            HN,
            &flags,
            &[("MEIOSE_LOG", "debug")],
            &[&db_url, "--api-key", API_KEY, &format!("--token={API_KEY}")],
        );
        s.init();
        s.call("browser_snapshot", json!({}));
        let data = s.finish();
        let mut checked = 0;
        for f in walk(data.path()) {
            let name = f.file_name().unwrap().to_string_lossy().into_owned();
            if name.starts_with("server-") {
                continue; // the fake server's own logs
            }
            assert!(
                !name.contains(DB_PASSWORD) && !name.contains("sk-live"),
                "{name}"
            );
            let bytes = std::fs::read(&f).unwrap();
            let text = String::from_utf8_lossy(&bytes);
            if name.ends_with(".jsonl") {
                // A recording keeps the traffic itself; only its header names the command.
                let header = text.lines().next().unwrap();
                assert!(!header.contains(API_KEY), "{sub}: {header}");
                assert!(!header.contains(DB_PASSWORD), "{sub}: {header}");
            } else if !name.starts_with("meiose.db") {
                assert!(!text.contains(API_KEY), "{sub}: {name}: {text}");
                assert!(!text.contains(DB_PASSWORD), "{sub}: {name}: {text}");
            }
            checked += 1;
        }
        assert!(checked >= 2, "{sub}");
        let (out, stats) = meiose_in(data.path(), data.path(), &["stats"], None);
        assert!(out.status.success(), "{stats}");
        assert!(stats.contains("browser_snapshot"), "{stats}");
        assert!(
            !stats.contains(DB_PASSWORD) && !stats.contains("sk-live"),
            "{stats}"
        );
        let (_, log) = meiose_in(data.path(), data.path(), &["log"], None);
        assert!(
            !log.contains(DB_PASSWORD) && !log.contains("sk-live"),
            "{log}"
        );
        let conn = rusqlite::Connection::open(data.path().join("meiose.db")).unwrap();
        let servers: Vec<String> = conn
            .prepare("SELECT DISTINCT server FROM calls")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        for srv in servers {
            assert!(
                !srv.contains(DB_PASSWORD) && !srv.contains("sk-live"),
                "{srv}"
            );
        }
    }
}

#[test]
fn install_diff_hides_secret_arguments() {
    let env = Env::new();
    let config = json!({"mcpServers": {"db": {
        "command": "uvx",
        "args": ["postgres-mcp", "--dsn", format!("postgres://admin:{DB_PASSWORD}@db/shop"),
                 "--api-key", API_KEY]
    }}});
    std::fs::write(env.config(), serde_json::to_string_pretty(&config).unwrap()).unwrap();
    let (out, text) = env.meiose(
        &["install", "--dry-run", "--meiose-path", FAKE_MEIOSE],
        None,
    );
    assert!(out.status.success(), "{text}");
    assert!(text.contains("~ db"), "{text}");
    assert!(text.contains("postgres-mcp --dsn"), "{text}");
    assert!(
        !text.contains(DB_PASSWORD) && !text.contains(API_KEY),
        "{text}"
    );
}

// ---------------- 5. pruned data is gone ----------------

#[test]
fn pruned_outputs_leave_no_trace_in_the_database_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("meiose.db");
    let conn = meiose::store::open_db(&path).unwrap();
    let small = format!("{{\"text\":\"{API_KEY}\"}}");
    let big = format!("{{\"text\":\"{}{API_KEY}\"}}", "x".repeat(200_000));
    for (id, result) in [("small", &small), ("big", &big)] {
        conn.execute(
            "INSERT INTO outputs VALUES (?1, 0, 's', 't', ?2, ?2)",
            [id, result.as_str()],
        )
        .unwrap();
    }
    // Make sure the rows reached the database file, not just the WAL.
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    meiose::store::prune(&conn).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM outputs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
    // The connection is still open, as in a running proxy.
    for f in files_in(dir.path()) {
        let bytes = std::fs::read(&f).unwrap();
        let found = bytes
            .windows(API_KEY.len())
            .any(|w| w == API_KEY.as_bytes());
        assert!(!found, "the pruned secret is still in {}", f.display());
    }
}
