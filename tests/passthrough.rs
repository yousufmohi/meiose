//! End-to-end passthrough tests: the real `meiose` binary in front of
//! `tests/fake_server.py`. Every test runs under a watchdog: a hang is a failure.
//!
//! Needs Python 3 on PATH (`python` on Windows, `python3` elsewhere), or set
//! `MEIOSE_TEST_PYTHON` to an interpreter path.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const WATCHDOG: Duration = Duration::from_secs(20);

fn python() -> String {
    std::env::var("MEIOSE_TEST_PYTHON")
        .unwrap_or_else(|_| if cfg!(windows) { "python" } else { "python3" }.to_string())
}

fn fake_server() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake_server.py")
}

struct Proxy {
    child: Child,
    stdout: JoinHandle<Vec<u8>>,
    stderr: JoinHandle<Vec<u8>>,
    _data: tempfile::TempDir,
}

struct Finished {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: String,
}

/// Start `meiose <sub> [extra] -- python fake_server.py <mode...>`.
fn start(sub: &str, extra: &[&str], mode: &[&str], env: &[(&str, &str)]) -> Proxy {
    let data = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_meiose"));
    cmd.arg(sub)
        .args(extra)
        .arg("--")
        .arg(python())
        .arg("-u")
        .arg(fake_server())
        .args(mode)
        .env("MEIOSE_DATA_DIR", data.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn meiose");
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let stdout = std::thread::spawn(move || {
        let mut v = Vec::new();
        out.read_to_end(&mut v).unwrap();
        v
    });
    let stderr = std::thread::spawn(move || {
        let mut v = Vec::new();
        err.read_to_end(&mut v).unwrap();
        v
    });
    Proxy {
        child,
        stdout,
        stderr,
        _data: data,
    }
}

impl Proxy {
    fn send(&mut self, bytes: &[u8]) {
        let stdin = self.child.stdin.as_mut().unwrap();
        stdin.write_all(bytes).unwrap();
        stdin.flush().unwrap();
    }

    fn close_stdin(&mut self) {
        drop(self.child.stdin.take());
    }

    /// Wait for exit; panic (after killing) if it takes longer than `limit`.
    fn finish_within(mut self, limit: Duration) -> Finished {
        let start = Instant::now();
        let status = loop {
            if let Some(s) = self.child.try_wait().unwrap() {
                break s;
            }
            if start.elapsed() > limit {
                self.child.kill().ok();
                panic!("meiose did not exit within {limit:?} (hang)");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        Finished {
            status,
            stdout: self.stdout.join().unwrap(),
            stderr: String::from_utf8_lossy(&self.stderr.join().unwrap()).into_owned(),
            // keep data dir alive until here
        }
    }

    fn finish(self) -> Finished {
        self.finish_within(WATCHDOG)
    }
}

fn mixed_frames() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#);
    v.push(b'\n');
    v.extend_from_slice(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    v.extend_from_slice(b"\r\n"); // CRLF must survive untouched
    v.extend_from_slice("{\"jsonrpc\":\"2.0\",\"id\":\"é-ü-😀\",\"method\":\"x/y\"}\n".as_bytes());
    v.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"s\":\"a\\nb \\u2028\"}}\n");
    v.extend_from_slice(b"this is not json at all\n");
    v
}

#[test]
fn mirror_roundtrip_is_byte_exact() {
    let input = mixed_frames();
    let mut p = start("run", &[], &["mirror"], &[]);
    p.send(&input);
    p.close_stdin();
    let f = p.finish();
    assert!(
        f.status.success(),
        "exit: {:?}\nstderr:\n{}",
        f.status,
        f.stderr
    );
    assert!(f.stdout == input, "stdout differs from input");
}

#[test]
fn five_megabyte_message_roundtrips() {
    let mut big =
        br#"{"jsonrpc":"2.0","id":9,"result":{"content":[{"type":"text","text":""#.to_vec();
    big.extend(std::iter::repeat_n(b'x', 5 * 1024 * 1024));
    big.extend_from_slice(b"\"}]}}\n");
    let mut input = big.clone();
    input.extend_from_slice(b"{\"after\":true}\n");

    let mut p = start("run", &[], &["mirror"], &[]);
    // Write from a thread: a 5 MB write can fill the pipe before we read.
    let mut stdin = p.child.stdin.take().unwrap();
    let send = input.clone();
    let writer = std::thread::spawn(move || stdin.write_all(&send).unwrap());
    writer.join().unwrap();
    let f = p.finish();
    assert!(f.status.success(), "stderr:\n{}", f.stderr);
    assert_eq!(f.stdout.len(), input.len());
    assert!(f.stdout == input);
}

#[test]
fn server_stderr_goes_to_stderr_never_stdout() {
    let mut p = start("run", &[], &["mirror"], &[]);
    p.send(b"{\"id\":1}\n");
    p.close_stdin();
    let f = p.finish();
    assert_eq!(f.stdout, b"{\"id\":1}\n");
    assert!(
        f.stderr.contains("fake: mirrored 9 bytes"),
        "stderr:\n{}",
        f.stderr
    );
    assert!(f.stderr.contains("fake: stdin closed after 1 lines"));
}

#[test]
fn server_exit_code_is_propagated() {
    let mut p = start("run", &[], &["exit", "7"], &[]);
    p.send(b"{\"id\":1}\n");
    let f = p.finish(); // stdin left open: the server, not the agent, ends it
    assert_eq!(f.status.code(), Some(7), "stderr:\n{}", f.stderr);
}

#[test]
fn server_initiated_shutdown_delivers_all_output() {
    let p = start("run", &[], &["burst", "500"], &[]);
    // We never close stdin; meiose must still exit once the server does.
    let f = p.finish();
    assert!(f.status.success(), "stderr:\n{}", f.stderr);
    let lines: Vec<_> = f
        .stdout
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(lines.len(), 500);
    assert!(lines[499].ends_with(br#""n":499}}"#));
}

#[test]
fn server_ignoring_stdin_eof_is_killed_after_grace() {
    let mut p = start(
        "run",
        &[],
        &["hang"],
        &[("MEIOSE_SHUTDOWN_GRACE_MS", "300")],
    );
    p.send(b"{\"id\":1}\n");
    p.close_stdin();
    let started = Instant::now();
    let f = p.finish_within(Duration::from_secs(10));
    assert!(!f.status.success());
    assert!(started.elapsed() < Duration::from_secs(8));
}

#[test]
fn missing_command_fails_with_actionable_error() {
    let out = Command::new(env!("CARGO_BIN_EXE_meiose"))
        .args(["run", "--", "meiose-no-such-server-xyz"])
        .env("MEIOSE_DATA_DIR", tempfile::tempdir().unwrap().path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "nothing but protocol on stdout");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("could not find `meiose-no-such-server-xyz`"),
        "{err}"
    );
    assert!(err.contains("→"), "error must say what to do next: {err}");
}

#[test]
fn record_logs_every_direction_and_stays_transparent() {
    let rec_dir = tempfile::tempdir().unwrap();
    let rec = rec_dir.path().to_str().unwrap();
    let input = mixed_frames();
    let mut p = start("record", &["--out", rec], &["mirror"], &[]);
    p.send(&input);
    p.close_stdin();
    let f = p.finish();
    assert!(f.status.success(), "stderr:\n{}", f.stderr);
    assert!(f.stdout == input, "record must not change the stream");
    assert!(f.stderr.contains("meiose: recording to"));

    let files: Vec<_> = std::fs::read_dir(rec_dir.path()).unwrap().collect();
    assert_eq!(files.len(), 1);
    let text = std::fs::read_to_string(files[0].as_ref().unwrap().path()).unwrap();
    let entries: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    assert_eq!(entries[0]["meiose_recording"], 1);
    let raw_of = |dir: &str| -> Vec<u8> {
        entries[1..]
            .iter()
            .filter(|e| e["dir"] == dir)
            .flat_map(|e| e["raw"].as_str().unwrap().as_bytes().to_vec())
            .collect()
    };
    assert!(raw_of("c2s") == input, "c2s recording must be exact");
    assert!(raw_of("s2c") == input, "s2c recording must be exact");
    assert!(
        String::from_utf8(raw_of("stderr"))
            .unwrap()
            .contains("fake: mirrored")
    );
}
