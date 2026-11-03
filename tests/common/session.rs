//! Drive the real `meiose run` binary in front of `tests/fake_server.py mcp`
//! like an agent would: line-delimited JSON-RPC over pipes, with timeouts so
//! a hang fails the test instead of stalling it.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::value::RawValue;
use serde_json::{Value, json};

pub const TIMEOUT: Duration = Duration::from_secs(20);

pub fn python() -> String {
    std::env::var("MEIOSE_TEST_PYTHON")
        .unwrap_or_else(|_| if cfg!(windows) { "python" } else { "python3" }.to_string())
}

pub fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

pub struct Session {
    pub child: Child,
    pub stdin: Option<ChildStdin>,
    pub lines: mpsc::Receiver<String>,
    pub data: tempfile::TempDir,
    /// Every line the fake server wrote.
    pub server_log: PathBuf,
    /// Every line the fake server received.
    pub server_in_log: PathBuf,
    pub next_id: u64,
}

impl Session {
    pub fn start(fixture: &str, flags: &[&str]) -> Session {
        Session::start_with(fixture, flags, &[])
    }

    pub fn start_with(fixture: &str, flags: &[&str], env: &[(&str, &str)]) -> Session {
        Session::start_full("run", fixture, flags, env, &[])
    }

    /// `meiose <sub> <flags> -- python fake_server.py mcp <fixture> <server_args>`.
    pub fn start_full(
        sub: &str,
        fixture: &str,
        flags: &[&str],
        env: &[(&str, &str)],
        server_args: &[&str],
    ) -> Session {
        let data = tempfile::tempdir().unwrap();
        let server_log = data.path().join("server-output.jsonl");
        let server_in_log = data.path().join("server-input.jsonl");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_meiose"));
        cmd.arg(sub)
            .args(flags)
            .arg("--")
            .arg(python())
            .arg("-u")
            .arg(root().join("tests/fake_server.py"))
            .arg("mcp")
            .arg(root().join(fixture))
            .args(server_args)
            .env("MEIOSE_DATA_DIR", data.path())
            .env("FAKE_LOG", &server_log)
            .env("FAKE_IN_LOG", &server_in_log)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("spawn meiose");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Session {
            child,
            stdin,
            lines,
            data,
            server_log,
            server_in_log,
            next_id: 100,
        }
    }

    pub fn send_raw(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        stdin.write_all(bytes).unwrap();
        stdin.flush().unwrap();
    }

    pub fn send(&mut self, v: &Value) {
        let mut line = serde_json::to_vec(v).unwrap();
        line.push(b'\n');
        self.send_raw(&line);
    }

    pub fn recv_line(&self) -> String {
        self.lines
            .recv_timeout(TIMEOUT)
            .expect("meiose did not answer in time (hang?)")
    }

    pub fn next_request_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Send a request; return the raw response line with that id plus any
    /// other messages that arrived first.
    pub fn request_raw(&mut self, method: &str, params: Value) -> (String, Vec<Value>) {
        let id = self.next_request_id();
        self.send(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        let mut others = Vec::new();
        loop {
            let line = self.recv_line();
            let v: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => {
                    others.push(Value::String(line));
                    continue;
                }
            };
            if v.get("id") == Some(&json!(id)) && v.get("method").is_none() {
                return (line, others);
            }
            others.push(v);
        }
    }

    pub fn request(&mut self, method: &str, params: Value) -> Value {
        serde_json::from_str(&self.request_raw(method, params).0).unwrap()
    }

    pub fn call(&mut self, tool: &str, args: Value) -> Value {
        self.request("tools/call", json!({"name": tool, "arguments": args}))
    }

    pub fn init(&mut self) -> Vec<String> {
        self.request(
            "initialize",
            json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"0"}}),
        );
        self.send(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        let tools = self.request("tools/list", json!({}));
        tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    /// Raw `result` bytes of the server's response with this id.
    pub fn server_result_raw(&self, id: u64) -> String {
        let log = std::fs::read_to_string(&self.server_log).unwrap();
        for line in log.lines() {
            #[derive(serde::Deserialize)]
            struct R<'a> {
                #[serde(borrow)]
                id: Option<&'a RawValue>,
                #[serde(borrow)]
                result: Option<&'a RawValue>,
            }
            let Ok(r) = serde_json::from_str::<R>(line) else {
                continue;
            };
            if r.id.map(|i| i.get()) == Some(&id.to_string())
                && let Some(res) = r.result
            {
                return res.get().to_string();
            }
        }
        panic!("server never answered id {id}");
    }

    pub fn server_lines(&self) -> Vec<String> {
        std::fs::read_to_string(&self.server_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Wait for meiose to exit on its own.
    pub fn wait_exit(&mut self, limit: Duration) -> ExitStatus {
        let start = Instant::now();
        loop {
            if let Some(s) = self.child.try_wait().unwrap() {
                return s;
            }
            if start.elapsed() > limit {
                let _ = self.child.kill();
                panic!("meiose did not exit within {limit:?} (hang)");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Close stdin like an agent shutting down; wait for exit.
    pub fn finish(mut self) -> tempfile::TempDir {
        drop(self.stdin.take());
        self.wait_exit(TIMEOUT);
        self.data
    }
}

pub fn text_of(resp: &Value) -> &str {
    resp["result"]["content"][0]["text"]
        .as_str()
        .expect("text content")
}

pub fn expand_id(text: &str) -> String {
    let s = text.rfind("meiose_expand(\"").expect("marker") + "meiose_expand(\"".len();
    text[s..s + text[s..].find('"').unwrap()].to_string()
}

/// `result` bytes exactly as they appear in a response line.
pub fn result_raw_of(line: &str) -> String {
    #[derive(serde::Deserialize)]
    struct R<'a> {
        #[serde(borrow)]
        result: &'a RawValue,
    }
    let r: R = serde_json::from_str(line).unwrap();
    r.result.get().to_string()
}
