//! Logging goes to a daily file, never to stdout (stdout belongs to the
//! protocol). If the log directory is unusable we fall back to stderr,
//! which MCP clients treat as free-form server logs.
//!
//! Level is controlled by `MEIOSE_LOG` (e.g. `MEIOSE_LOG=debug`); default `info`.
//!
//! Files are `logs/meiose.YYYY-MM-DD.log` (UTC), owner-only on Unix, and
//! the newest [`MAX_LOG_FILES`] are kept. Server arguments are never logged:
//! they often carry credentials.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

use crate::{paths, secure};

/// Daily log files kept; older ones are deleted when a new day starts.
pub const MAX_LOG_FILES: usize = 7;

/// Install the global subscriber. Keep the returned guard alive until exit,
/// then drop it to flush buffered log lines.
pub fn init() -> Option<WorkerGuard> {
    let filter = EnvFilter::try_from_env("MEIOSE_LOG").unwrap_or_else(|_| EnvFilter::new("info"));

    match paths::logs_dir().and_then(|dir| DailyFile::open(dir).ok()) {
        Some(file) => {
            let (writer, guard) = tracing_appender::non_blocking(file);
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

/// Days since the Unix epoch, UTC.
fn today() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0)
}

/// `2026-11-03` for a day number (Howard Hinnant's civil-from-days).
fn ymd(days: u64) -> String {
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

pub fn is_log_file(name: &str) -> bool {
    name.starts_with("meiose.") && name.ends_with(".log")
}

/// An append-only log file that moves to a new file when the UTC day changes.
struct DailyFile {
    dir: PathBuf,
    day: u64,
    file: File,
}

impl DailyFile {
    fn open(dir: PathBuf) -> io::Result<DailyFile> {
        secure::create_private_dir(&dir)?;
        let day = today();
        let file = open_day(&dir, day)?;
        prune_old(&dir);
        Ok(DailyFile { dir, day, file })
    }
}

fn open_day(dir: &Path, day: u64) -> io::Result<File> {
    let path = dir.join(format!("meiose.{}.log", ymd(day)));
    let f = secure::private_options()
        .create(true)
        .append(true)
        .open(&path)?;
    secure::tighten(&path, false);
    Ok(f)
}

/// Keep the newest [`MAX_LOG_FILES`] (names sort by date).
fn prune_old(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut logs: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| is_log_file(&e.file_name().to_string_lossy()))
        .map(|e| e.path())
        .collect();
    logs.sort();
    let extra = logs.len().saturating_sub(MAX_LOG_FILES);
    for old in &logs[..extra] {
        let _ = std::fs::remove_file(old);
    }
}

impl Write for DailyFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let day = today();
        if day != self.day
            && let Ok(f) = open_day(&self.dir, day)
        {
            let _ = self.file.flush();
            self.file = f;
            self.day = day;
            prune_old(&self.dir);
        }
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_numbers_become_dates() {
        assert_eq!(ymd(0), "1970-01-01");
        assert_eq!(ymd(20_395), "2025-11-03");
        assert_eq!(ymd(19_782), "2024-02-29");
    }

    #[test]
    fn keeps_the_newest_log_files_only() {
        let t = tempfile::tempdir().unwrap();
        for d in 1..=9 {
            std::fs::write(t.path().join(format!("meiose.2026-01-0{d}.log")), "x").unwrap();
        }
        std::fs::write(t.path().join("other.txt"), "x").unwrap();
        let mut f = DailyFile::open(t.path().to_path_buf()).unwrap();
        f.write_all(b"hello\n").unwrap();
        let mut names: Vec<String> = std::fs::read_dir(t.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names.len(), MAX_LOG_FILES + 1, "{names:?}");
        assert!(!names.contains(&"meiose.2026-01-03.log".to_string()));
        assert!(names.contains(&"meiose.2026-01-09.log".to_string()));
        assert!(names.contains(&"other.txt".to_string()));
        assert!(names.contains(&format!("meiose.{}.log", ymd(today()))));
    }
}
