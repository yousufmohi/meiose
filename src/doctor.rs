//! `meiose doctor`: check the install end to end and say how to fix
//! anything that is off.

use std::fmt::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;

use crate::install::{self, Client, Launch, Roots};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Fail,
    Info,
}

#[derive(Debug, Clone)]
pub struct Check {
    pub status: Status,
    pub what: String,
    /// What to do about it (for Warn/Fail).
    pub fix: Option<String>,
}

fn check(status: Status, what: impl Into<String>, fix: Option<&str>) -> Check {
    Check {
        status,
        what: what.into(),
        fix: fix.map(str::to_string),
    }
}

pub fn run(roots: &Roots) -> Vec<Check> {
    let mut out = Vec::new();
    let version = env!("CARGO_PKG_VERSION");
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "<unknown>".into());
    out.push(check(
        Status::Ok,
        format!("meiose {version} at {exe}"),
        None,
    ));

    // Data directory and database.
    match crate::store::default_path() {
        Some(db) => match crate::store::open_db(&db) {
            Ok(conn) => {
                let calls: i64 = conn
                    .query_row("SELECT COUNT(*) FROM calls", [], |r| r.get(0))
                    .unwrap_or(0);
                out.push(check(
                    Status::Ok,
                    format!("database {} ({calls} calls recorded)", db.display()),
                    None,
                ));
            }
            Err(e) => out.push(check(
                Status::Fail,
                format!("cannot open the database: {e:#}"),
                Some("set MEIOSE_DATA_DIR to a writable directory; without it meiose forwards traffic untrimmed"),
            )),
        },
        None => out.push(check(
            Status::Fail,
            "no data directory found",
            Some("set MEIOSE_DATA_DIR to a writable directory"),
        )),
    }

    // Agent configs.
    let mut any_npx = false;
    for client in Client::ALL {
        let paths: Vec<_> = roots
            .candidates(client)
            .into_iter()
            .filter(|p| p.is_file())
            .collect();
        if paths.is_empty() {
            out.push(check(
                Status::Info,
                format!("{}: no config found (not installed?)", client.label()),
                None,
            ));
            continue;
        }
        for path in paths {
            any_npx |= check_config(client, &path, version, &mut out);
        }
    }

    // Install manifest.
    if let Some(data) = crate::paths::data_dir() {
        let dir = install::InstallDir::new(&data);
        match dir.load() {
            Ok(m) if m.files.is_empty() => {}
            Ok(m) => {
                for f in &m.files {
                    if !f.backup.is_file() {
                        out.push(check(
                            Status::Warn,
                            format!("backup for {} is missing ({})", f.path.display(), f.backup.display()),
                            Some("`meiose uninstall` can still unwrap servers, but cannot restore the file byte-for-byte"),
                        ));
                    }
                }
                out.push(check(
                    Status::Ok,
                    format!(
                        "{} config file(s) installed, backups in {}",
                        m.files.len(),
                        dir.dir.display()
                    ),
                    None,
                ));
            }
            Err(e) => out.push(check(Status::Fail, format!("{e:#}"), None)),
        }
    }

    if any_npx {
        match which::which("npx") {
            Ok(p) => out.push(check(
                Status::Ok,
                format!("npx found at {}", p.display()),
                None,
            )),
            Err(_) => out.push(check(
                Status::Fail,
                "a server is started with npx, but npx is not on PATH",
                Some("install Node.js (https://nodejs.org), then restart your agent"),
            )),
        }
    }
    out
}

/// Returns whether any server in the file is launched via npx.
fn check_config(client: Client, path: &Path, our_version: &str, out: &mut Vec<Check>) -> bool {
    let label = format!("{} ({})", client.label(), path.display());
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            out.push(check(
                Status::Fail,
                format!("{label}: cannot read: {e}"),
                None,
            ));
            return false;
        }
    };
    let doc: Value = match serde_json::from_str(&text) {
        Ok(d) => d,
        Err(e) => {
            out.push(check(
                Status::Fail,
                format!("{label}: not valid JSON ({e})"),
                Some("fix the file; the agent cannot load it either"),
            ));
            return false;
        }
    };
    let entries = install::entries(&doc);
    let mut wrapped = 0;
    let mut stdio = 0;
    let mut problems = Vec::new();
    let mut npx = false;
    for (loc, entry) in &entries {
        let name = install::location_label(loc);
        let Some(launch) = launch_of(entry) else {
            continue; // remote or unusual: not ours to judge
        };
        stdio += 1;
        let inner = install::unwrap_launch(&launch);
        let target = inner.as_ref().unwrap_or(&launch);
        npx |= install::program_name(&target.command) == "npx";
        match &inner {
            None => problems.push(check(
                Status::Warn,
                format!("  {name}: not going through meiose"),
                Some("run `meiose install` to wrap it"),
            )),
            Some(inner) => {
                wrapped += 1;
                if !command_exists(&launch.command) {
                    problems.push(check(
                        Status::Fail,
                        format!(
                            "  {name}: points to {}, which does not exist",
                            launch.command
                        ),
                        Some(
                            "run `meiose install` again to update the path (or `meiose uninstall`)",
                        ),
                    ));
                } else if let Some(v) = version_of(&launch.command)
                    && v != our_version
                {
                    problems.push(check(
                        Status::Warn,
                        format!("  {name}: uses meiose {v}, this is {our_version}"),
                        Some("fine if intended; otherwise reinstall so both are the same version"),
                    ));
                }
                if !command_exists(&inner.command) {
                    problems.push(check(
                        Status::Warn,
                        format!("  {name}: cannot find `{}` on PATH here", inner.command),
                        Some("the agent may have a different PATH; if the server fails to start, use the full path to the command"),
                    ));
                }
            }
        }
    }
    let status = if problems.iter().any(|p| p.status == Status::Fail) {
        Status::Fail
    } else if problems.is_empty() {
        Status::Ok
    } else {
        Status::Warn
    };
    out.push(check(
        status,
        format!("{label}: {stdio} stdio server(s), {wrapped} through meiose"),
        None,
    ));
    out.extend(problems);
    npx
}

/// A command with a path separator must exist as a file (Windows may omit
/// `.exe`); a bare name must resolve on PATH.
fn command_exists(cmd: &str) -> bool {
    if cmd.contains(['/', '\\']) {
        let p = Path::new(cmd);
        p.is_file() || (cfg!(windows) && p.with_extension("exe").is_file())
    } else {
        which::which(cmd).is_ok()
    }
}

fn launch_of(entry: &Value) -> Option<Launch> {
    if entry.get("url").is_some() {
        return None;
    }
    let command = entry.get("command")?.as_str()?.to_string();
    let args = entry
        .get("args")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Some(Launch { command, args })
}

/// `<exe> --version` → "0.1.0", with a timeout.
fn version_of(exe: &str) -> Option<String> {
    let mut child = Command::new(exe)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let start = std::time::Instant::now();
    loop {
        if child.try_wait().ok()?.is_some() {
            break;
        }
        if start.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut s = String::new();
    use std::io::Read;
    child.stdout.take()?.read_to_string(&mut s).ok()?;
    s.split_whitespace().nth(1).map(str::to_string)
}

pub fn render(checks: &[Check]) -> String {
    let mut s = String::from("meiose doctor\n");
    for c in checks {
        let mark = match c.status {
            Status::Ok => "✓",
            Status::Warn => "!",
            Status::Fail => "✗",
            Status::Info => "·",
        };
        let _ = writeln!(s, "  {mark} {}", c.what);
        if let Some(fix) = &c.fix {
            let _ = writeln!(s, "      → {fix}");
        }
    }
    let fails = checks.iter().filter(|c| c.status == Status::Fail).count();
    let warns = checks.iter().filter(|c| c.status == Status::Warn).count();
    let _ = writeln!(
        s,
        "\n{}",
        match (fails, warns) {
            (0, 0) => "All good.".to_string(),
            (0, w) => format!("{w} warning(s); meiose works, see the notes above."),
            (f, _) => format!("{f} problem(s) need fixing; see the → lines above."),
        }
    );
    s
}
