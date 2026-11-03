//! The proxy: spawn the real MCP server and pump frames in both directions.
//!
//! ```text
//!  agent stdin ──c2s──▶ child stdin
//!  agent stdout ◀─writer◀─s2c── child stdout
//!  agent stderr ◀──────────── child stderr
//! ```
//!
//! M1 is a pure passthrough: frames are forwarded as opaque bytes. The single
//! stdout writer task exists so later milestones can inject locally produced
//! responses (e.g. `meiose_expand`) without two writers interleaving.
//!
//! Lifecycle (MCP stdio "Shutdown" section):
//! - agent closes our stdin  → close the child's stdin, give it `grace` to
//!   exit, then kill it. Remaining child output is still delivered.
//! - child exits on its own  → deliver its remaining output, then exit with
//!   the child's exit code.
//! - agent stops reading     → kill the child and exit.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::frame::{READ_BUF, read_frame, write_frames};
use crate::intercept::{ClientAction, Interceptor};
use crate::procgroup;
use crate::record::{Dir, Recorder};

pub struct Options {
    pub cmd: Vec<String>,
    pub recorder: Option<Recorder>,
    /// Trimming/expand logic; `None` is a pure byte passthrough.
    pub intercept: Option<Arc<Interceptor>>,
    /// How long the child gets to exit after its stdin is closed.
    pub grace: Duration,
}

/// Default shutdown grace; override with `MEIOSE_SHUTDOWN_GRACE_MS`.
pub fn default_grace() -> Duration {
    let ms = std::env::var("MEIOSE_SHUTDOWN_GRACE_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000);
    Duration::from_millis(ms)
}

/// How long we wait for buffered output after the child is gone. A grandchild
/// (e.g. node under `npx.cmd`) may hold the pipe open after its parent exits.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error(
        "could not find `{program}` on PATH\n  \
         → check it is installed, or pass its full path: meiose run -- /full/path/to/{program} ..."
    )]
    NotFound { program: String },
    #[error(
        "could not start `{program}` ({path}): {source}\n  \
         → try running the command without meiose to see whether it starts on its own"
    )]
    Io {
        program: String,
        path: PathBuf,
        source: std::io::Error,
    },
}

fn spawn(cmd: &[String]) -> Result<Child, SpawnError> {
    let (program, args) = cmd.split_first().expect("clap guarantees a command");
    // `which` applies PATHEXT on Windows, so `npx` resolves to `npx.cmd`.
    let path = which::which(program).map_err(|_| SpawnError::NotFound {
        program: program.clone(),
    })?;
    let mut command = Command::new(&path);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    procgroup::configure(&mut command);
    command.spawn().map_err(|source| SpawnError::Io {
        program: program.clone(),
        path,
        source,
    })
}

/// Run the proxy until the session ends. Returns the exit code to use.
pub async fn run(opts: Options) -> anyhow::Result<i32> {
    let mut child = spawn(&opts.cmd)?;
    // Kept alive until we exit: on Windows, closing it kills the server tree.
    let tree = procgroup::Tree::attach(&child);
    // The program only: arguments often carry credentials (`--api-key …`).
    tracing::info!(
        program = %crate::install::program_name(&opts.cmd[0]),
        pid = child.id(),
        "started MCP server"
    );

    let child_stdin = child.stdin.take().expect("piped");
    let child_stdout = child.stdout.take().expect("piped");
    let child_stderr = child.stderr.take().expect("piped");

    let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>(64);
    let mut writer = tokio::spawn(write_frames(tokio::io::stdout(), out_rx));
    let mut c2s = tokio::spawn(client_to_server(
        BufReader::with_capacity(READ_BUF, tokio::io::stdin()),
        child_stdin,
        opts.recorder.clone(),
        opts.intercept.clone(),
        // Weak: the writer must still finish once the server's output ends,
        // even while the agent keeps our stdin open.
        out_tx.downgrade(),
    ));
    let s2c = tokio::spawn(server_to_client(
        BufReader::with_capacity(READ_BUF, child_stdout),
        out_tx,
        opts.recorder.clone(),
        opts.intercept.clone(),
    ));
    let errp = tokio::spawn(copy_stderr(
        BufReader::with_capacity(READ_BUF, child_stderr),
        tokio::io::stderr(),
        opts.recorder,
    ));

    let status = tokio::select! {
        status = child.wait() => {
            tracing::info!(?status, "MCP server exited");
            status
        }
        _ = &mut c2s => {
            tracing::info!("agent closed stdin; closed server stdin, waiting for it to exit");
            wait_or_kill(&mut child, &tree, opts.grace).await
        }
        _ = shutdown_signal() => {
            tracing::info!("meiose was asked to stop; stopping the MCP server");
            tree.kill(&mut child).await;
            child.wait().await
        }
        res = &mut writer => match res {
            // The server closed its stdout and everything was delivered:
            // server-initiated shutdown. It should exit on its own.
            Ok(Ok(())) => {
                tracing::info!("MCP server closed stdout; waiting for it to exit");
                wait_or_kill(&mut child, &tree, opts.grace).await
            }
            res => {
                tracing::warn!(?res, "agent stopped reading stdout; killing MCP server");
                tree.kill(&mut child).await;
                child.wait().await
            }
        },
    };

    // Deliver whatever the child wrote before exiting.
    drain(s2c, "server stdout").await;
    drain(errp, "server stderr").await;
    if !writer.is_finished() {
        drain(writer, "agent stdout").await;
    }

    let code = match status {
        Ok(s) => exit_code(s),
        Err(e) => {
            tracing::error!(error = %e, "could not get MCP server exit status");
            1
        }
    };
    Ok(code)
}

async fn wait_or_kill(
    child: &mut Child,
    tree: &procgroup::Tree,
    grace: Duration,
) -> std::io::Result<std::process::ExitStatus> {
    match tokio::time::timeout(grace, child.wait()).await {
        Ok(status) => status,
        Err(_) => {
            tracing::warn!(
                ?grace,
                "MCP server did not exit in time; killing it and its children"
            );
            tree.kill(child).await;
            child.wait().await
        }
    }
}

/// Ctrl-C (and SIGTERM on Unix, Ctrl-Break on Windows). If a handler cannot
/// be installed (no console, etc.), that source simply never fires.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let term = async {
            match signal(SignalKind::terminate()) {
                Ok(mut s) => {
                    s.recv().await;
                }
                Err(_) => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            _ = ctrl_c() => {}
            _ = term => {}
        }
    }
    #[cfg(windows)]
    {
        let brk = async {
            match tokio::signal::windows::ctrl_break() {
                Ok(mut s) => {
                    s.recv().await;
                }
                Err(_) => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            _ = ctrl_c() => {}
            _ = brk => {}
        }
    }
    #[cfg(not(any(unix, windows)))]
    ctrl_c().await
}

async fn ctrl_c() {
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
}

async fn drain<T: std::fmt::Debug>(task: JoinHandle<T>, what: &str) {
    match tokio::time::timeout(DRAIN_TIMEOUT, task).await {
        Ok(Ok(res)) => tracing::debug!(?res, "{what} finished"),
        Ok(Err(e)) => tracing::error!(error = %e, "{what} task failed"),
        Err(_) => tracing::warn!("{what} still open {DRAIN_TIMEOUT:?} after exit; giving up on it"),
    }
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    1
}

/// agent → server. Returning (for any reason) drops the child's stdin,
/// which is the spec's graceful shutdown signal.
async fn client_to_server<R, W>(
    mut from: R,
    mut to: W,
    rec: Option<Recorder>,
    intercept: Option<Arc<Interceptor>>,
    out: mpsc::WeakSender<Vec<u8>>,
) where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    loop {
        let frame = match read_frame(&mut from).await {
            Ok(Some(f)) => f,
            Ok(None) => return,
            Err(e) => {
                tracing::error!(error = %e, "reading agent stdin failed");
                return;
            }
        };
        if let Some(r) = &rec {
            r.log(Dir::C2s, &frame).await;
        }
        if let Some(ic) = &intercept
            && let ClientAction::Expand {
                id_raw,
                expand_id,
                query,
                terminator,
            } = ic.on_client_frame(&frame)
        {
            // Answered locally; never forwarded to the server.
            let ic = ic.clone();
            let reply = tokio::task::spawn_blocking(move || {
                ic.expand_reply(&id_raw, expand_id.as_deref(), query.as_deref(), terminator)
            })
            .await;
            match (reply, out.upgrade()) {
                (Ok(reply), Some(tx)) => {
                    let _ = tx.send(reply).await;
                }
                (Err(e), _) => tracing::error!(error = %e, "meiose_expand failed"),
                (_, None) => tracing::warn!("agent stdout closed; dropping meiose_expand reply"),
            }
            continue;
        }
        if let Err(e) = async {
            to.write_all(&frame).await?;
            to.flush().await
        }
        .await
        {
            tracing::warn!(error = %e, "writing to MCP server stdin failed (server gone?)");
            return;
        }
    }
}

/// server → agent, via the single stdout writer.
async fn server_to_client<R>(
    mut from: R,
    out: mpsc::Sender<Vec<u8>>,
    rec: Option<Recorder>,
    intercept: Option<Arc<Interceptor>>,
) where
    R: AsyncBufRead + Unpin,
{
    loop {
        let frame = match read_frame(&mut from).await {
            Ok(Some(f)) => f,
            Ok(None) => return,
            Err(e) => {
                tracing::error!(error = %e, "reading MCP server stdout failed");
                return;
            }
        };
        if let Some(r) = &rec {
            r.log(Dir::S2c, &frame).await;
        }
        let frame = match &intercept {
            Some(ic) => ic.on_server_frame(frame),
            None => frame,
        };
        if out.send(frame).await.is_err() {
            return; // agent stdout writer is gone
        }
    }
}

/// Server stderr is forwarded verbatim; MCP clients show it as server logs.
async fn copy_stderr<R, W>(mut from: R, mut to: W, rec: Option<Recorder>)
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    while let Ok(Some(line)) = read_frame(&mut from).await {
        if let Some(r) = &rec {
            r.log(Dir::Stderr, &line).await;
        }
        if to.write_all(&line).await.is_err() || to.flush().await.is_err() {
            // Our stderr is gone; keep draining so the child never blocks on it.
            while let Ok(Some(_)) = read_frame(&mut from).await {}
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_program_error_says_what_to_do() {
        let err = spawn(&["meiose-definitely-not-a-real-program".into()]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("could not find `meiose-definitely-not-a-real-program`"));
        assert!(
            msg.contains("→"),
            "error must tell the user what to do: {msg}"
        );
    }

    #[tokio::test]
    async fn c2s_forwards_exact_bytes_and_stops_at_eof() {
        let input: &[u8] = b"{\"id\":1}\n{\"id\":2}\r\nno-newline";
        let mut sink = Vec::new();
        let (tx, _rx) = mpsc::channel(1);
        client_to_server(BufReader::new(input), &mut sink, None, None, tx.downgrade()).await;
        assert_eq!(sink, input);
    }

    #[tokio::test]
    async fn s2c_forwards_exact_frames_in_order() {
        let input: &[u8] = b"{\"a\":1}\n{\"b\":2}\n";
        let (tx, mut rx) = mpsc::channel(8);
        server_to_client(BufReader::new(input), tx, None, None).await;
        assert_eq!(rx.recv().await.unwrap(), b"{\"a\":1}\n");
        assert_eq!(rx.recv().await.unwrap(), b"{\"b\":2}\n");
        assert!(rx.recv().await.is_none());
    }
}
