//! Logging goes to a daily-rotated file, never to stdout (stdout belongs to
//! the protocol). If the log directory is unusable we fall back to stderr,
//! which MCP clients treat as free-form server logs.
//!
//! Level is controlled by `MEIOSE_LOG` (e.g. `MEIOSE_LOG=debug`); default `info`.

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

use crate::paths;

/// Install the global subscriber. Keep the returned guard alive until exit,
/// then drop it to flush buffered log lines.
pub fn init() -> Option<WorkerGuard> {
    let filter = EnvFilter::try_from_env("MEIOSE_LOG").unwrap_or_else(|_| EnvFilter::new("info"));

    let dir = paths::logs_dir().filter(|d| std::fs::create_dir_all(d).is_ok());
    match dir {
        Some(dir) => {
            let appender = tracing_appender::rolling::daily(dir, "meiose.log");
            let (writer, guard) = tracing_appender::non_blocking(appender);
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(writer)
                .with_ansi(false)
                .init();
            Some(guard)
        }
        None => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(std::io::stderr)
                .with_ansi(false)
                .init();
            tracing::warn!(
                "could not create the meiose log directory; logging to stderr instead \
                 (set MEIOSE_DATA_DIR to a writable directory to fix this)"
            );
            None
        }
    }
}
