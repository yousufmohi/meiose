//! Hostile pages and servers: terminal escapes in stored text, expand ids
//! used across servers, and frames too big to process.

mod common;

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

use common::session::{Session, expand_id, result_raw_of, text_of};
use serde_json::json;

const HN: &str = "tests/fixtures/playwright/news-ycombinator-com.txt";

fn meiose(data: &Path, args: &[&str]) -> (bool, Vec<u8>, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_meiose"))
        .args(args)
        .env("MEIOSE_DATA_DIR", data)
        .output()
        .unwrap();
    (
        out.status.success(),
        out.stdout,
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// No terminal control bytes except newline and tab, in UTF-8 or as C1.
fn assert_no_controls(what: &str, out: &[u8]) {
    let text = String::from_utf8_lossy(out);
    let bad: Vec<char> = text
        .chars()
        .filter(|c| c.is_control() && *c != '\n' && *c != '\t')
        .collect();
    assert!(bad.is_empty(), "{what} printed control characters {bad:?}");
}

// ---------------- 6. terminal escapes ----------------

#[test]
fn log_and_stats_print_no_terminal_escapes() {
    let evil = "\u{1b}]52;c;ZXZpbA==\u{7}\u{1b}[31mred\u{1b}[0m\r\u{9b}2J";
    let mut s = Session::start(HN, &[]);
    s.init();
    let snap = s.call("hostile", json!({}));
    let eid = expand_id(text_of(&snap));
    let data = s.finish();
    // A tool and server name with escapes too (a hostile agent or config).
    let conn = meiose::store::open_db(&data.path().join("meiose.db")).unwrap();
    conn.execute(
        "UPDATE calls SET tool = ?1, server = ?1 WHERE expand_id = ?2",
        [&format!("tool{evil}"), &eid],
    )
    .unwrap();
    drop(conn);

    for args in [vec!["log"], vec!["log", "--show", &eid], vec!["stats"]] {
        let (ok, out, err) = meiose(data.path(), &args);
        assert!(ok, "{args:?}: {err}");
        assert_no_controls(&format!("{args:?}"), &out);
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("tool"), "{args:?}: {text}");
    }
    let (_, out, _) = meiose(data.path(), &["log", "--show", &eid]);
    assert!(String::from_utf8_lossy(&out).contains("]52;c;ZXZpbA=="));
}

#[test]
fn expand_still_returns_escapes_byte_for_byte() {
    let mut s = Session::start(HN, &[]);
    s.init();
    let id = s.next_id + 1;
    let snap = s.call("hostile", json!({}));
    let eid = expand_id(text_of(&snap));
    let (line, _) = s.request_raw(
        "tools/call",
        json!({"name":"meiose_expand","arguments":{"id": eid}}),
    );
    assert_eq!(result_raw_of(&line), s.server_result_raw(id));
    assert!(text_of(&serde_json::from_str(&line).unwrap()).contains('\u{1b}'));
    let raw = s.server_result_raw(id);
    let data = s.finish();
    // The stored original is untouched as well.
    let conn = rusqlite::Connection::open(data.path().join("meiose.db")).unwrap();
    let stored: String = conn
        .query_row("SELECT result FROM outputs WHERE id = ?1", [&eid], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(stored, raw);
}

#[test]
fn log_show_into_a_closed_pipe_does_not_panic() {
    let mut s = Session::start(HN, &[]);
    s.init();
    let snap = s.call("big_text", json!({"kb": 4096}));
    let eid = expand_id(text_of(&snap));
    let data = s.finish();
    for args in [
        vec!["log", "--show", eid.as_str()],
        vec!["log", "-n", "500"],
        vec!["stats"],
    ] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_meiose"))
            .args(&args)
            .env("MEIOSE_DATA_DIR", data.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // Like `| head -c 10`: read a little, then close the pipe.
        let mut first = [0u8; 10];
        child
            .stdout
            .as_mut()
            .unwrap()
            .read_exact(&mut first)
            .unwrap();
        drop(child.stdout.take());
        let out = child.wait_with_output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!err.contains("panicked"), "{args:?}: {err}");
        assert!(out.status.success(), "{args:?}: {:?} {err}", out.status);
    }
}

// ---------------- 7. expand is scoped to its server ----------------

fn session_in(data: &Path, server_args: &[&str]) -> Session {
    let dir = data.to_str().unwrap().to_string();
    Session::start_full(
        "run",
        HN,
        &[],
        &[("MEIOSE_DATA_DIR", dir.as_str())],
        server_args,
    )
}

#[test]
fn expand_only_returns_outputs_of_the_same_server() {
    let data = tempfile::tempdir().unwrap();
    // Same program and first argument: the stats label is the same for both.
    let work = ["--profile", "work"];
    let home = ["--profile", "home"];

    let mut a = session_in(data.path(), &work);
    a.init();
    let snap = a.call("browser_snapshot", json!({}));
    let eid = expand_id(text_of(&snap));
    let full = a.call("meiose_expand", json!({"id": eid}));
    assert_eq!(full["result"]["isError"], serde_json::Value::Null);
    a.finish();

    let mut b = session_in(data.path(), &home);
    b.init();
    let other = b.call("meiose_expand", json!({"id": eid}));
    assert_eq!(other["result"]["isError"], true, "{other}");
    assert!(
        text_of(&other).contains(&format!("no stored output for id \"{eid}\"")),
        "{other}"
    );
    b.finish();

    // The same server in a new process still finds it on disk.
    let mut c = session_in(data.path(), &work);
    c.init();
    let again = c.call("meiose_expand", json!({"id": eid}));
    assert!(text_of(&again).contains("[cursor=pointer]"), "{again}");
    c.finish();
}

#[test]
fn expand_ids_carry_at_least_64_random_bits() {
    let mut s = Session::start(HN, &[]);
    s.init();
    let mut ids = Vec::new();
    for _ in 0..3 {
        let snap = s.call("browser_snapshot", json!({"checked": ids.len() % 2 == 1}));
        ids.push(expand_id(text_of(&snap)));
    }
    s.finish();
    for id in &ids {
        // 32 symbols carry 5 bits each.
        assert!(id.len() * 5 >= 64, "{id} is too short");
        assert!(
            id.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
            "{id}"
        );
    }
    ids.dedup();
    assert_eq!(ids.len(), 3);
}

// ---------------- 8. size limits ----------------

/// Debug builds read `MEIOSE_TEST_MAX_FRAME_KB` so this runs in a moment.
const LIMIT: (&str, &str) = ("MEIOSE_TEST_MAX_FRAME_KB", "1024");

#[test]
fn an_oversized_response_passes_through_untouched_and_unstored() {
    let mut s = Session::start_with(HN, &[], &[LIMIT]);
    s.init();
    let id = s.next_id + 1;
    let (line, _) = s.request_raw(
        "tools/call",
        json!({"name":"big_text","arguments":{"kb": 3000}}),
    );
    assert_eq!(
        result_raw_of(&line),
        s.server_result_raw(id),
        "an oversized frame must reach the agent byte-for-byte"
    );
    assert!(!line.contains("meiose_expand(\""));
    // The stream is intact: a normal call afterwards is still trimmed.
    let snap = s.call("browser_snapshot", json!({}));
    assert!(text_of(&snap).contains("meiose_expand(\""));
    let data = s.finish();
    let conn = rusqlite::Connection::open(data.path().join("meiose.db")).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM outputs WHERE LENGTH(result) > 1000000",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0, "an oversized output must not be stored");
}

#[test]
fn an_oversized_request_reaches_the_server_untouched() {
    let mut s = Session::start_with(HN, &[], &[LIMIT]);
    s.init();
    let text = "x".repeat(3 * 1024 * 1024);
    let id = s.next_id + 1;
    let (line, _) = s.request_raw(
        "tools/call",
        json!({"name":"echo","arguments":{"text": text}}),
    );
    // The server got it (it echoed it back) and the reply is the server's.
    assert_eq!(result_raw_of(&line), s.server_result_raw(id));
    let received = std::fs::read_to_string(&s.server_in_log).unwrap();
    assert!(received.contains(&text));
    s.finish();
}
