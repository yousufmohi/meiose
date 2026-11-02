//! `meiose record`: append every frame that crosses the proxy to a JSONL file.
//!
//! Line 1 is a header: `{"meiose_recording":1,"cmd":[...],"started_unix_ms":N}`.
//! Every other line is `{"t_ms":N,"dir":"c2s"|"s2c"|"stderr","raw":"..."}` where
//! `raw` is the exact frame including its line terminator. MCP mandates UTF-8;
//! if a frame is not valid UTF-8 it is stored lossily and marked `"lossy":true`.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde_json::json;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// agent → server
    C2s,
    /// server → agent
    S2c,
    /// server stderr
    Stderr,
}

impl Dir {
    fn as_str(self) -> &'static str {
        match self {
            Dir::C2s => "c2s",
            Dir::S2c => "s2c",
            Dir::Stderr => "stderr",
        }
    }
}

struct Entry {
    t_ms: u64,
    dir: Dir,
    raw: Vec<u8>,
}

/// Cheap to clone; every pump task holds one. The file is flushed and closed
/// once every clone is dropped and the writer task is awaited.
#[derive(Clone)]
pub struct Recorder {
    tx: mpsc::Sender<Entry>,
    start: Instant,
}

impl Recorder {
    /// Create `<dir>/<unix_ms>-<name>.jsonl` and start the writer task.
    pub async fn create(
        dir: &Path,
        cmd: &[String],
    ) -> anyhow::Result<(Recorder, JoinHandle<io::Result<()>>, PathBuf)> {
        tokio::fs::create_dir_all(dir).await.with_context(|| {
            format!(
                "could not create the recording directory {}\n  \
                 → pass a writable directory with --out <DIR>",
                dir.display()
            )
        })?;
        let now_ms = unix_ms();
        let path = dir.join(format!("{now_ms}-{}.jsonl", file_stem(cmd)));
        let file = tokio::fs::File::create(&path).await.with_context(|| {
            format!(
                "could not create the recording file {}\n  \
                 → pass a writable directory with --out <DIR>",
                path.display()
            )
        })?;

        let header = json!({ "meiose_recording": 1, "cmd": cmd, "started_unix_ms": now_ms });
        let (tx, rx) = mpsc::channel(256);
        let task = tokio::spawn(write_entries(BufWriter::new(file), header, rx));
        Ok((
            Recorder {
                tx,
                start: Instant::now(),
            },
            task,
            path,
        ))
    }

    pub async fn log(&self, dir: Dir, raw: &[u8]) {
        let entry = Entry {
            t_ms: self.start.elapsed().as_millis() as u64,
            dir,
            raw: raw.to_vec(),
        };
        // If the writer died (disk full, etc.) recording stops but proxying
        // continues; the error surfaces when the task is awaited.
        let _ = self.tx.send(entry).await;
    }
}

async fn write_entries<W: tokio::io::AsyncWrite + Unpin>(
    mut w: W,
    header: serde_json::Value,
    mut rx: mpsc::Receiver<Entry>,
) -> io::Result<()> {
    write_json_line(&mut w, &header).await?;
    while let Some(e) = rx.recv().await {
        let line = match std::str::from_utf8(&e.raw) {
            Ok(s) => json!({ "t_ms": e.t_ms, "dir": e.dir.as_str(), "raw": s }),
            Err(_) => json!({
                "t_ms": e.t_ms,
                "dir": e.dir.as_str(),
                "raw": String::from_utf8_lossy(&e.raw),
                "lossy": true,
            }),
        };
        write_json_line(&mut w, &line).await?;
        if rx.is_empty() {
            w.flush().await?;
        }
    }
    w.flush().await
}

async fn write_json_line<W: tokio::io::AsyncWrite + Unpin>(
    w: &mut W,
    v: &serde_json::Value,
) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(v)?;
    bytes.push(b'\n');
    w.write_all(&bytes).await
}

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// `["npx", "@playwright/mcp@latest"]` → `npx-playwright-mcp-latest`.
pub fn file_stem(cmd: &[String]) -> String {
    let program = cmd
        .first()
        .and_then(|p| Path::new(p).file_stem())
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    // First non-flag argument: `npx -y @playwright/mcp` names the package, not `-y`.
    let first_arg = cmd
        .iter()
        .skip(1)
        .find(|a| !a.starts_with('-'))
        .map(String::as_str)
        .unwrap_or_default();
    let joined = format!("{program}-{first_arg}");
    let mut out = String::new();
    for c in joined.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out: String = out.trim_end_matches('-').chars().take(48).collect();
    if out.is_empty() {
        "session".into()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stem_is_filesystem_safe() {
        let cmd = ["npx".to_string(), "@playwright/mcp@latest".to_string()];
        assert_eq!(file_stem(&cmd), "npx-playwright-mcp-latest");
        let cmd = ["npx", "-y", "@playwright/mcp", "--headless"].map(String::from);
        assert_eq!(file_stem(&cmd), "npx-playwright-mcp");
        let cmd = ["/opt/tools/node.exe".to_string(), "server.js".to_string()];
        assert_eq!(file_stem(&cmd), "node-server-js");
        assert_eq!(file_stem(&["???".to_string()]), "session");
    }

    #[tokio::test]
    async fn records_header_then_exact_frames() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = vec!["fake".to_string()];
        let (rec, task, path) = Recorder::create(dir.path(), &cmd).await.unwrap();
        rec.log(Dir::C2s, b"{\"id\":1}\r\n").await;
        rec.log(Dir::S2c, b"{\"id\":1,\"result\":{}}\n").await;
        rec.log(Dir::Stderr, b"\xff not utf8\n").await;
        drop(rec);
        task.await.unwrap().unwrap();

        let text = std::fs::read_to_string(path).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0]["meiose_recording"], 1);
        assert_eq!(lines[0]["cmd"], json!(["fake"]));
        assert_eq!(lines[1]["dir"], "c2s");
        assert_eq!(lines[1]["raw"], "{\"id\":1}\r\n");
        assert_eq!(lines[2]["dir"], "s2c");
        assert_eq!(lines[3]["dir"], "stderr");
        assert_eq!(lines[3]["lossy"], true);
        assert!(lines[1].get("lossy").is_none());
    }
}
