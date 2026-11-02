//! M5 protocol suite: the proxy must never hang, deadlock, reorder or
//! corrupt a message, whatever the server (or agent) does, and must fail
//! open when its own logic breaks.

mod common;

use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::session::{Session, TIMEOUT, expand_id, python, result_raw_of, root, text_of};
use serde_json::{Value, json};

const HN: &str = "tests/fixtures/playwright/news-ycombinator-com.txt";

#[test]
fn interleaved_out_of_order_responses_are_matched_correctly() {
    let mut s = Session::start(HN, &[]);
    s.init();
    // 30 slow echoes answered in reverse order, with snapshots (answered at
    // once, and trimmed) interleaved between them.
    let mut expected: HashMap<u64, String> = HashMap::new();
    let mut snapshot_ids = Vec::new();
    for i in 0..30u64 {
        let id = s.next_request_id();
        let text = format!("echo-{i}");
        s.send(&json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
            "params":{"name":"echo","arguments":{"text":text,"delay_ms":(30 - i) * 15}}}));
        expected.insert(id, text);
        if i % 5 == 0 {
            let sid = s.next_request_id();
            s.send(&json!({"jsonrpc":"2.0","id":sid,"method":"tools/call",
                "params":{"name":"browser_snapshot","arguments":{}}}));
            snapshot_ids.push(sid);
        }
    }
    let total = expected.len() + snapshot_ids.len();
    let mut order = Vec::new();
    for _ in 0..total {
        let line = s.recv_line();
        let v: Value = serde_json::from_str(&line).expect("valid JSON");
        let id = v["id"].as_u64().expect("response id");
        order.push(id);
        if let Some(text) = expected.remove(&id) {
            assert_eq!(text_of(&v), text, "response {id} carried the wrong payload");
            assert_eq!(
                result_raw_of(&line),
                s.server_result_raw(id),
                "untouched result must be byte-identical"
            );
        } else {
            assert!(snapshot_ids.contains(&id), "unexpected id {id}");
            assert!(
                text_of(&v).contains("meiose_expand(\""),
                "snapshot {id} was not trimmed"
            );
        }
    }
    assert!(expected.is_empty(), "missing responses: {expected:?}");
    let mut sorted = order.clone();
    sorted.sort();
    assert_ne!(order, sorted, "responses should have arrived out of order");
    s.finish();
}

#[test]
fn notifications_mid_stream_pass_through_byte_for_byte() {
    let mut s = Session::start(HN, &[]);
    s.init();
    let id = s.next_request_id();
    s.send(&json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"notify","arguments":{"count":5}}}));
    let mut got = Vec::new();
    loop {
        let line = s.recv_line();
        let done =
            line.contains(&format!("\"id\": {id}")) || line.contains(&format!("\"id\":{id}"));
        got.push(line);
        if done {
            break;
        }
    }
    assert_eq!(got.len(), 6, "{got:?}");
    let server = s.server_lines();
    for line in &got[..5] {
        assert!(line.contains("notifications/progress"));
        assert!(server.contains(line), "notification altered: {line}");
    }
    s.finish();
}

#[test]
fn five_megabyte_snapshot_is_trimmed_and_expands_byte_for_byte() {
    let mut s = Session::start(HN, &[]);
    s.init();
    let id = s.next_id + 1;
    let started = Instant::now();
    let (line, _) = s.request_raw("tools/call", json!({"name":"huge_snapshot","arguments":{}}));
    let resp: Value = serde_json::from_str(&line).unwrap();
    let original_len = s.server_result_raw(id).len();
    assert!(
        original_len > 5_000_000,
        "fixture should be > 5 MB, is {original_len}"
    );
    assert!(line.len() < original_len, "must be trimmed");
    let eid = expand_id(text_of(&resp));
    let (full, _) = s.request_raw(
        "tools/call",
        json!({"name":"meiose_expand","arguments":{"id":eid}}),
    );
    assert_eq!(result_raw_of(&full), s.server_result_raw(id));
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "took {:?}",
        started.elapsed()
    );
    s.finish();
}

#[test]
fn malformed_server_lines_are_forwarded_verbatim() {
    let mut s = Session::start(HN, &[]);
    s.init();
    let (line, others) = s.request_raw("tools/call", json!({"name":"garbage","arguments":{}}));
    assert_eq!(others, [Value::String("{this is not json".into())]);
    assert!(line.contains("after garbage"));
    s.finish();
}

#[test]
fn malformed_and_batch_client_lines_reach_the_server_unchanged() {
    let mut s = Session::start(HN, &[]);
    s.init();
    let junk = b"{oops not json\n";
    let batch = b"[{\"jsonrpc\":\"2.0\",\"id\":999,\"method\":\"tools/list\"}]\n";
    let crlf = b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/x\"}\r\n";
    s.send_raw(junk);
    s.send_raw(batch);
    s.send_raw(crlf);
    let resp = s.call("small", json!({}));
    assert_eq!(text_of(&resp), "ok", "the session must keep working");
    let received = std::fs::read(&s.server_in_log).unwrap();
    for needle in [&junk[..], &batch[..], &crlf[..]] {
        assert!(
            received.windows(needle.len()).any(|w| w == needle),
            "server did not receive {:?} byte-for-byte",
            String::from_utf8_lossy(needle)
        );
    }
    s.finish();
}

#[test]
fn responses_written_in_pieces_are_reassembled() {
    let mut s = Session::start(HN, &[]);
    s.init();
    let resp = s.call("split", json!({}));
    assert_eq!(text_of(&resp), "split ok");
    s.finish();
}

#[test]
fn slow_server_is_waited_for_not_timed_out() {
    let mut s = Session::start(HN, &[]);
    s.init();
    let started = Instant::now();
    let resp = s.call("echo", json!({"text": "patience", "delay_ms": 2500}));
    assert_eq!(text_of(&resp), "patience");
    assert!(started.elapsed() >= Duration::from_millis(2400));
    s.finish();
}

#[test]
fn server_crash_mid_request_ends_the_session_cleanly() {
    let mut s = Session::start(HN, &[]);
    s.init();
    let id = s.next_request_id();
    s.send(&json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"crash","arguments":{}}}));
    // The agent keeps stdin open; meiose must still notice and exit.
    let status = s.wait_exit(TIMEOUT);
    assert_eq!(status.code(), Some(3));
}

#[test]
fn panic_in_server_side_logic_fails_open() {
    let mut s = Session::start_with(HN, &[], &[("MEIOSE_TEST_PANIC", "server")]);
    let names = s.init();
    assert!(
        !names.contains(&"meiose_expand".to_string()),
        "nothing injected when broken"
    );
    let id = s.next_id + 1;
    let (line, _) = s.request_raw(
        "tools/call",
        json!({"name":"browser_snapshot","arguments":{}}),
    );
    assert_eq!(
        result_raw_of(&line),
        s.server_result_raw(id),
        "original forwarded unchanged"
    );
    s.finish();
}

#[test]
fn panic_in_client_side_logic_fails_open() {
    let mut s = Session::start_with(HN, &[], &[("MEIOSE_TEST_PANIC", "client")]);
    s.init();
    let resp = s.call("small", json!({}));
    assert_eq!(text_of(&resp), "ok");
    let sent = std::fs::read_to_string(&s.server_in_log).unwrap();
    assert!(
        sent.contains("\"name\":\"small\""),
        "requests still reach the server"
    );
    s.finish();
}

#[test]
fn unusable_store_means_plain_passthrough() {
    let blocker = tempfile::NamedTempFile::new().unwrap();
    // A regular file where the data directory should be: the store cannot open.
    let mut s = Session::start_with(
        HN,
        &[],
        &[("MEIOSE_DATA_DIR", blocker.path().to_str().unwrap())],
    );
    let names = s.init();
    assert!(!names.contains(&"meiose_expand".to_string()));
    let id = s.next_id + 1;
    let (line, _) = s.request_raw(
        "tools/call",
        json!({"name":"browser_snapshot","arguments":{}}),
    );
    assert_eq!(result_raw_of(&line), s.server_result_raw(id));
    s.finish();
}

#[test]
fn agent_that_stops_reading_does_not_hang_meiose() {
    let data = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_meiose"))
        .args(["run", "--", &python(), "-u"])
        .arg(root().join("tests/fake_server.py"))
        .arg("mcp")
        .arg(root().join(HN))
        .env("MEIOSE_DATA_DIR", data.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    drop(child.stdout.take()); // the agent goes away without closing stdin
    let mut stdin = child.stdin.take().unwrap();
    for i in 0..20 {
        let line = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{i},\"method\":\"tools/call\",\"params\":{{\"name\":\"browser_snapshot\",\"arguments\":{{}}}}}}\n"
        );
        if stdin.write_all(line.as_bytes()).is_err() {
            break;
        }
    }
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        assert!(
            start.elapsed() < TIMEOUT,
            "meiose hung after the agent stopped reading"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn alive(pid: u32) -> bool {
    if cfg!(windows) {
        let out = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).contains(&pid.to_string())
    } else {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    }
}

fn wait_dead(pid: u32) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[test]
fn stuck_server_is_killed_with_its_whole_process_tree() {
    let pid_file = tempfile::NamedTempFile::new().unwrap();
    let mut s = Session::start_with(
        HN,
        &[],
        &[
            ("MEIOSE_SHUTDOWN_GRACE_MS", "500"),
            ("GRANDCHILD_PID", pid_file.path().to_str().unwrap()),
        ],
    );
    s.init();
    s.call("spawn_grandchild", json!({}));
    let gc: u32 = std::fs::read_to_string(pid_file.path())
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(alive(gc));
    // Agent shuts down; the server ignores stdin EOF, so meiose must kill
    // it, and its child with it.
    drop(s.stdin.take());
    s.wait_exit(TIMEOUT);
    assert!(wait_dead(gc), "grandchild {gc} survived");
}

#[cfg(windows)]
#[test]
fn hard_killing_meiose_takes_the_server_tree_with_it() {
    let pid_file = tempfile::NamedTempFile::new().unwrap();
    let mut s = Session::start_with(
        HN,
        &[],
        &[("GRANDCHILD_PID", pid_file.path().to_str().unwrap())],
    );
    s.init();
    s.call("spawn_grandchild", json!({}));
    let gc: u32 = std::fs::read_to_string(pid_file.path())
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    s.child.kill().unwrap(); // TerminateProcess: no cleanup code runs in meiose
    s.child.wait().unwrap();
    assert!(
        wait_dead(gc),
        "grandchild {gc} survived meiose being killed (job object not working)"
    );
}
