//! Private files and directories. meiose stores tool outputs, logs, config
//! backups and recordings, any of which can hold secrets, so on Unix its
//! directories are `700` and its files `600`, set when they are created so
//! nothing is ever briefly readable by other users. Installs from before
//! this rule are tightened on startup.
//!
//! On Windows everything is left to the default ACLs of the user's profile.
//! Failing to tighten an existing path is a warning, never an error.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

/// Create `dir` and any missing parents. Directories created here are
/// private; existing ones are left as they are.
pub fn create_dir(dir: &Path) -> io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)
}

/// Create (or tighten) one of meiose's own directories.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    create_dir(dir)?;
    tighten(dir, true);
    Ok(())
}

/// Open options that create a file readable by its owner only.
pub fn private_options() -> OpenOptions {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut o = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o
}

/// Create (or truncate) `path` for writing, owner-only.
pub fn create_private(path: &Path) -> io::Result<File> {
    let f = private_options()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    // `mode` applies only to new files: tighten one that already existed.
    tighten_file(&f, path);
    Ok(f)
}

/// Create (or truncate) `path` with owner-only permissions and write `bytes`.
pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut f = create_private(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// Create a new file with owner-only permissions; fails if it exists.
pub fn create_new_private(path: &Path) -> io::Result<File> {
    private_options().write(true).create_new(true).open(path)
}

/// Make sure an existing file exists with owner-only permissions, without
/// changing its contents (used for the database before SQLite opens it).
pub fn touch_private(path: &Path) -> io::Result<()> {
    let f = private_options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    tighten_file(&f, path);
    Ok(())
}

#[cfg(unix)]
fn tighten_file(f: &File, path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(m) = f.metadata() {
        let mode = m.permissions().mode();
        if mode & 0o077 != 0
            && let Err(e) = f.set_permissions(std::fs::Permissions::from_mode(mode & 0o700))
        {
            warn(path, &e);
        }
    }
}

#[cfg(not(unix))]
fn tighten_file(_: &File, _: &Path) {}

/// Remove group and other permissions from `path` (a `755` directory
/// becomes `700`, a `644` file `600`). Symlinks are left alone. Never fails; logs a warning.
pub fn tighten(path: &Path, is_dir: bool) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(m) = std::fs::symlink_metadata(path) else {
            return;
        };
        if m.file_type().is_symlink() || m.is_dir() != is_dir {
            return;
        }
        let mode = m.permissions().mode();
        let want = mode & 0o700;
        if mode & 0o077 != 0
            && let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(want))
        {
            warn(path, &e);
        }
    }
    #[cfg(not(unix))]
    let _ = (path, is_dir);
}

#[cfg(unix)]
fn warn(path: &Path, e: &io::Error) {
    tracing::warn!(
        path = %path.display(),
        error = %e,
        "could not make this private (owner-only); other users may be able to read it. \
         → run `chmod go-rwx` on it"
    );
}

/// Tighten an existing data directory: the directory itself, the database
/// files, logs, recordings and install backups (one level deep each).
pub fn tighten_data_dir(data: &Path) {
    if !cfg!(unix) || !data.is_dir() {
        return;
    }
    tighten(data, true);
    tighten_children(data);
    for sub in ["logs", "recordings", "install"] {
        let d = data.join(sub);
        if d.is_dir() {
            tighten(&d, true);
            tighten_children(&d);
        }
    }
}

fn tighten_children(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        if e.file_type().is_ok_and(|t| t.is_file()) {
            tighten(&e.path(), false);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn new_files_and_dirs_are_private() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("a/b");
        create_dir(&d).unwrap();
        assert_eq!(mode(&t.path().join("a")), 0o700);
        assert_eq!(mode(&d), 0o700);
        write_private(&d.join("f"), b"x").unwrap();
        assert_eq!(mode(&d.join("f")), 0o600);
    }

    #[test]
    fn tighten_removes_group_and_other_bits_only() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("f");
        std::fs::write(&f, "x").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
        tighten(&f, false);
        assert_eq!(mode(&f), 0o600);
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o444)).unwrap();
        tighten(&f, false);
        assert_eq!(mode(&f), 0o400, "owner bits are kept");
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&f, b"y").unwrap();
        assert_eq!(mode(&f), 0o600);
    }
}
