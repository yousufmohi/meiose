//! Where meiose keeps its files. `MEIOSE_DATA_DIR` overrides the platform
//! default (used by tests and by people who want everything in one place).

use std::path::PathBuf;

pub fn data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("MEIOSE_DATA_DIR") {
        return Some(PathBuf::from(dir));
    }
    dirs::data_local_dir().map(|d| d.join("meiose"))
}

pub fn logs_dir() -> Option<PathBuf> {
    data_dir().map(|d| d.join("logs"))
}

pub fn recordings_dir() -> Option<PathBuf> {
    data_dir().map(|d| d.join("recordings"))
}
