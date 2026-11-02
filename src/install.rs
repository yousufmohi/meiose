//! `meiose install` / `meiose uninstall`: wrap every stdio MCP server in the
//! agent configs we know (Claude Code, Claude Desktop, Cursor) with
//! `meiose run --`, and undo it.
//!
//! Safety:
//! - Each file is backed up before its first change, and a copy of exactly
//!   what we wrote is kept next to it (in `<data>/install/`).
//! - `uninstall` restores the backup byte-for-byte when the file is still
//!   exactly what we wrote. If something else changed it since (Claude Code
//!   rewrites ~/.claude.json constantly), it instead unwraps only the
//!   entries we wrapped, restoring their original command/args and keeping
//!   every other change.
//! - Writes are atomic (temp file + rename).
//! - Remote (url/http/sse) servers and already-wrapped servers are skipped.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Client {
    ClaudeCode,
    ClaudeDesktop,
    Cursor,
}

impl Client {
    pub const ALL: [Client; 3] = [Client::ClaudeCode, Client::ClaudeDesktop, Client::Cursor];

    pub fn label(self) -> &'static str {
        match self {
            Client::ClaudeCode => "Claude Code",
            Client::ClaudeDesktop => "Claude Desktop",
            Client::Cursor => "Cursor",
        }
    }

    pub fn restart_hint(self) -> &'static str {
        match self {
            Client::ClaudeCode => "new Claude Code sessions pick this up; restart running ones",
            Client::ClaudeDesktop => "quit and reopen Claude Desktop",
            Client::Cursor => "restart Cursor (or reload its MCP servers)",
        }
    }
}

/// Where per-user config files live. `MEIOSE_FAKE_HOME` roots everything in
/// one directory (tests, and trying install safely).
#[derive(Debug, Clone)]
pub struct Roots {
    pub home: PathBuf,
    /// %APPDATA% on Windows, ~/Library/Application Support on macOS, ~/.config on Linux.
    pub config: PathBuf,
    /// %LOCALAPPDATA% on Windows (Microsoft Store installs live under it).
    pub local: PathBuf,
    pub claude_config_dir: Option<PathBuf>,
}

impl Roots {
    pub fn from_env() -> Option<Roots> {
        if let Some(fake) = std::env::var_os("MEIOSE_FAKE_HOME").map(PathBuf::from) {
            return Some(Roots::fake(&fake));
        }
        Some(Roots {
            home: dirs::home_dir()?,
            config: dirs::config_dir()?,
            local: dirs::data_local_dir()?,
            claude_config_dir: std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from),
        })
    }

    pub fn fake(root: &Path) -> Roots {
        let config = if cfg!(windows) {
            root.join("AppData/Roaming")
        } else if cfg!(target_os = "macos") {
            root.join("Library/Application Support")
        } else {
            root.join(".config")
        };
        Roots {
            home: root.to_path_buf(),
            config,
            local: root.join("AppData/Local"),
            claude_config_dir: None,
        }
    }

    /// Candidate config files for a client, most likely first.
    pub fn candidates(&self, client: Client) -> Vec<PathBuf> {
        match client {
            Client::ClaudeCode => vec![
                self.claude_config_dir
                    .as_ref()
                    .map(|d| d.join(".claude.json"))
                    .unwrap_or_else(|| self.home.join(".claude.json")),
            ],
            Client::ClaudeDesktop => {
                let mut v = vec![self.config.join("Claude/claude_desktop_config.json")];
                // Microsoft Store (MSIX) installs keep a virtualized copy.
                if cfg!(windows)
                    && let Ok(rd) = std::fs::read_dir(self.local.join("Packages"))
                {
                    for e in rd.flatten() {
                        if e.file_name().to_string_lossy().starts_with("Claude_") {
                            v.push(
                                e.path()
                                    .join("LocalCache/Roaming/Claude/claude_desktop_config.json"),
                            );
                        }
                    }
                }
                v
            }
            Client::Cursor => vec![self.home.join(".cursor/mcp.json")],
        }
    }

    /// Existing config files.
    pub fn found(&self) -> Vec<(Client, PathBuf)> {
        Client::ALL
            .iter()
            .flat_map(|&c| {
                self.candidates(c)
                    .into_iter()
                    .filter(|p| p.is_file())
                    .map(move |p| (c, p))
            })
            .collect()
    }
}

// ---------------- entries ----------------

/// JSON path to one server entry, e.g. ["mcpServers", "playwright"] or
/// ["projects", "C:\\work", "mcpServers", "db"].
pub type Location = Vec<String>;

pub fn location_label(loc: &Location) -> String {
    match loc.as_slice() {
        [m, name] if m == "mcpServers" => name.clone(),
        [p, proj, _, name] if p == "projects" => format!("{name} (project {proj})"),
        other => other.join("."),
    }
}

/// The parts of an entry install changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launch {
    pub command: String,
    pub args: Vec<String>,
}

impl Launch {
    fn of(entry: &Value) -> Option<Launch> {
        let command = entry.get("command")?.as_str()?.to_string();
        let args = match entry.get("args") {
            None => vec![],
            Some(Value::Array(a)) => a
                .iter()
                .map(|x| x.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()?,
            Some(_) => return None,
        };
        Some(Launch { command, args })
    }

    pub fn display(&self) -> String {
        let mut s = shell_word(&self.command);
        for a in &self.args {
            s.push(' ');
            s.push_str(&shell_word(a));
        }
        s
    }
}

fn shell_word(s: &str) -> String {
    if !s.is_empty() && !s.contains([' ', '"', '\'']) {
        s.to_string()
    } else {
        format!("{s:?}")
    }
}

/// Is this launch already going through meiose?
pub fn is_wrapped(l: &Launch) -> bool {
    let stem = Path::new(&l.command)
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase());
    stem.as_deref() == Some("meiose")
        && matches!(l.args.first().map(String::as_str), Some("run" | "record"))
}

pub fn wrap(l: &Launch, meiose: &str) -> Launch {
    let mut args = vec!["run".to_string(), "--".to_string(), l.command.clone()];
    args.extend(l.args.iter().cloned());
    Launch {
        command: meiose.to_string(),
        args,
    }
}

/// The server a wrapped launch runs (everything after `--`).
pub fn unwrap_launch(l: &Launch) -> Option<Launch> {
    if !is_wrapped(l) {
        return None;
    }
    let dd = l.args.iter().position(|a| a == "--")?;
    let (cmd, rest) = l.args[dd + 1..].split_first()?;
    Some(Launch {
        command: cmd.clone(),
        args: rest.to_vec(),
    })
}

fn is_remote(entry: &Value) -> bool {
    entry.get("url").is_some()
        || matches!(
            entry.get("type").and_then(Value::as_str),
            Some("http" | "sse" | "streamable-http" | "streamableHttp" | "ws")
        )
}

/// Every server entry in a config document, with its location.
pub fn entries(doc: &Value) -> Vec<(Location, &Value)> {
    let mut out = Vec::new();
    if let Some(Value::Object(m)) = doc.get("mcpServers") {
        for (name, e) in m {
            out.push((vec!["mcpServers".into(), name.clone()], e));
        }
    }
    // Claude Code "local" scope: per-project servers inside ~/.claude.json.
    if let Some(Value::Object(projects)) = doc.get("projects") {
        for (proj, p) in projects {
            if let Some(Value::Object(m)) = p.get("mcpServers") {
                for (name, e) in m {
                    out.push((
                        vec![
                            "projects".into(),
                            proj.clone(),
                            "mcpServers".into(),
                            name.clone(),
                        ],
                        e,
                    ));
                }
            }
        }
    }
    out
}

fn entry_mut<'a>(doc: &'a mut Value, loc: &Location) -> Option<&'a mut Map<String, Value>> {
    let mut v = doc;
    for k in loc {
        v = v.get_mut(k)?;
    }
    v.as_object_mut()
}

fn set_launch(entry: &mut Map<String, Value>, l: &Launch) {
    entry.insert("command".into(), Value::String(l.command.clone()));
    entry.insert(
        "args".into(),
        Value::Array(l.args.iter().cloned().map(Value::String).collect()),
    );
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skip {
    Remote,
    AlreadyWrapped,
    NoCommand,
    Filtered,
}

#[derive(Debug, Clone)]
pub struct Change {
    pub location: Location,
    pub before: Launch,
    pub after: Launch,
}

#[derive(Debug, Default)]
pub struct Plan {
    pub changes: Vec<Change>,
    pub skipped: Vec<(Location, Skip)>,
    pub new_text: Option<String>,
}

/// Plan wrapping every eligible stdio server in `text`.
pub fn plan_install(text: &str, meiose: &str, only: &[String]) -> anyhow::Result<Plan> {
    let mut doc: Value = serde_json::from_str(text).context("the file is not valid JSON")?;
    let mut plan = Plan::default();
    let found: Vec<(Location, Value)> = entries(&doc)
        .into_iter()
        .map(|(l, e)| (l, e.clone()))
        .collect();
    for (loc, entry) in found {
        let name = loc.last().cloned().unwrap_or_default();
        if !only.is_empty() && !only.contains(&name) {
            plan.skipped.push((loc, Skip::Filtered));
            continue;
        }
        if is_remote(&entry) {
            plan.skipped.push((loc, Skip::Remote));
            continue;
        }
        let Some(before) = Launch::of(&entry) else {
            plan.skipped.push((loc, Skip::NoCommand));
            continue;
        };
        if is_wrapped(&before) {
            plan.skipped.push((loc, Skip::AlreadyWrapped));
            continue;
        }
        let after = wrap(&before, meiose);
        set_launch(entry_mut(&mut doc, &loc).expect("found above"), &after);
        plan.changes.push(Change {
            location: loc,
            before,
            after,
        });
    }
    if !plan.changes.is_empty() {
        plan.new_text = Some(Style::detect(text).render(&doc)?);
    }
    Ok(plan)
}

/// Undo our wraps in `text` (semantic path), restoring recorded originals.
/// Only entries whose current launch is exactly what we wrote are touched.
pub fn unwrap_recorded(text: &str, recorded: &[Change]) -> anyhow::Result<(Option<String>, usize)> {
    let mut doc: Value = serde_json::from_str(text).context("the file is not valid JSON")?;
    let mut n = 0;
    for c in recorded {
        if let Some(entry) = entry_mut(&mut doc, &c.location)
            && Launch::of(&Value::Object(entry.clone())).as_ref() == Some(&c.after)
        {
            set_launch(entry, &c.before);
            n += 1;
        }
    }
    if n == 0 {
        return Ok((None, 0));
    }
    Ok((Some(Style::detect(text).render(&doc)?), n))
}

// ---------------- formatting ----------------

/// How a JSON file was formatted, so rewriting it changes as little as possible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Style {
    indent: Vec<u8>,
    crlf: bool,
    trailing_newline: bool,
}

impl Style {
    pub fn detect(text: &str) -> Style {
        let crlf = text.contains("\r\n");
        let indent = text
            .lines()
            .skip(1)
            .map(|l| {
                l.bytes()
                    .take_while(|b| *b == b' ' || *b == b'\t')
                    .collect::<Vec<u8>>()
            })
            .find(|w| !w.is_empty())
            .unwrap_or_else(|| b"  ".to_vec());
        Style {
            indent,
            crlf,
            trailing_newline: text.ends_with('\n'),
        }
    }

    pub fn render(&self, doc: &Value) -> anyhow::Result<String> {
        let mut buf = Vec::new();
        let fmt = serde_json::ser::PrettyFormatter::with_indent(&self.indent);
        let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
        doc.serialize(&mut ser)?;
        let mut s = String::from_utf8(buf)?;
        if self.crlf {
            s = s.replace('\n', "\r\n");
        }
        if self.trailing_newline {
            s.push_str(if self.crlf { "\r\n" } else { "\n" });
        }
        Ok(s)
    }
}

// ---------------- manifest & files ----------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRecord {
    pub client: Client,
    pub path: PathBuf,
    /// The file exactly as it was before meiose first touched it.
    pub backup: PathBuf,
    /// Exactly what meiose last wrote.
    pub written: PathBuf,
    /// Did anything else change the file between two installs? Then the
    /// backup no longer represents "the file minus meiose".
    pub byte_restore_ok: bool,
    pub changes: Vec<RecordedChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedChange {
    pub location: Location,
    pub before: Launch,
    pub after: Launch,
}

impl From<&Change> for RecordedChange {
    fn from(c: &Change) -> Self {
        RecordedChange {
            location: c.location.clone(),
            before: c.before.clone(),
            after: c.after.clone(),
        }
    }
}

impl From<&RecordedChange> for Change {
    fn from(c: &RecordedChange) -> Self {
        Change {
            location: c.location.clone(),
            before: c.before.clone(),
            after: c.after.clone(),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub files: Vec<FileRecord>,
}

pub struct InstallDir {
    pub dir: PathBuf,
}

impl InstallDir {
    pub fn new(data_dir: &Path) -> InstallDir {
        InstallDir {
            dir: data_dir.join("install"),
        }
    }

    fn manifest_path(&self) -> PathBuf {
        self.dir.join("manifest.json")
    }

    pub fn load(&self) -> anyhow::Result<Manifest> {
        match std::fs::read(self.manifest_path()) {
            Ok(b) => serde_json::from_slice(&b).with_context(|| {
                format!(
                    "{} is corrupt\n  → restore your configs from the backups in {} by hand, then delete it",
                    self.manifest_path().display(),
                    self.dir.display()
                )
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Manifest {
                version: 1,
                files: vec![],
            }),
            Err(e) => Err(e).context("could not read the install manifest"),
        }
    }

    pub fn save(&self, m: &Manifest) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        write_atomic(&self.manifest_path(), &serde_json::to_vec_pretty(m)?)
    }

    fn slot(&self, client: Client, path: &Path, kind: &str) -> PathBuf {
        let tag = format!(
            "{:016x}",
            crate::store::now_ms() as u64 ^ fnv(path.to_string_lossy().as_bytes())
        );
        let client = serde_json::to_value(client)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        self.dir.join(format!("{client}-{tag}.{kind}.json"))
    }
}

fn fnv(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &x| {
        (h ^ u64::from(x)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Write via a temp file in the same directory, then rename over the target.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let dir = path
        .parent()
        .context("config path has no parent directory")?;
    let tmp = dir.join(format!(
        ".{}.meiose-tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    std::fs::write(&tmp, bytes).with_context(|| format!("could not write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| {
        let _ = std::fs::remove_file(&tmp);
        format!(
            "could not replace {}\n  → close the app that has it open and try again",
            path.display()
        )
    })
}

// ---------------- high-level operations ----------------

pub struct FilePlan {
    pub client: Client,
    pub path: PathBuf,
    pub original: Vec<u8>,
    pub plan: Plan,
}

/// Plan install for every config file found.
pub fn plan_all(
    roots: &Roots,
    meiose: &str,
    only: &[String],
    clients: &[Client],
) -> Vec<(Client, PathBuf, anyhow::Result<FilePlan>)> {
    roots
        .found()
        .into_iter()
        .filter(|(c, _)| clients.is_empty() || clients.contains(c))
        .map(|(client, path)| {
            let r = (|| {
                let original = std::fs::read(&path)
                    .with_context(|| format!("could not read {}", path.display()))?;
                let text = std::str::from_utf8(&original).context("the file is not UTF-8")?;
                let plan = plan_install(text, meiose, only).with_context(|| {
                    format!(
                        "could not understand {}\n  → fix the JSON (or move the file aside) and run install again",
                        path.display()
                    )
                })?;
                Ok(FilePlan {
                    client,
                    path: path.clone(),
                    original,
                    plan,
                })
            })();
            (client, path, r)
        })
        .collect()
}

/// Apply one file's plan: backup (first time only), write, record.
pub fn apply(dir: &InstallDir, manifest: &mut Manifest, fp: &FilePlan) -> anyhow::Result<()> {
    let Some(new_text) = &fp.plan.new_text else {
        return Ok(());
    };
    std::fs::create_dir_all(&dir.dir)
        .with_context(|| format!("could not create {}", dir.dir.display()))?;
    let existing = manifest.files.iter().position(|f| f.path == fp.path);
    let record = match existing {
        Some(i) => {
            let r = &mut manifest.files[i];
            // Was the file changed by someone else since our last write?
            let ours = std::fs::read(&r.written).ok();
            if ours.as_deref() != Some(fp.original.as_slice()) {
                r.byte_restore_ok = false;
            }
            r
        }
        None => {
            let backup = dir.slot(fp.client, &fp.path, "backup");
            std::fs::write(&backup, &fp.original)
                .with_context(|| format!("could not back up {}", fp.path.display()))?;
            manifest.files.push(FileRecord {
                client: fp.client,
                path: fp.path.clone(),
                backup: backup.clone(),
                written: dir.slot(fp.client, &fp.path, "written"),
                byte_restore_ok: true,
                changes: vec![],
            });
            manifest.files.last_mut().expect("just pushed")
        }
    };
    record
        .changes
        .extend(fp.plan.changes.iter().map(RecordedChange::from));
    std::fs::write(&record.written, new_text.as_bytes())?;
    // Manifest first: if writing the config fails, uninstall still knows.
    dir.save(manifest)?;
    write_atomic(&fp.path, new_text.as_bytes())
}

#[derive(Debug, PartialEq, Eq)]
pub enum Undo {
    /// The file was exactly what we wrote: original restored byte-for-byte.
    Restored,
    /// The file changed since; N wrapped entries unwrapped, other edits kept.
    Unwrapped(usize),
    /// None of our entries are present any more; nothing to do.
    NothingToUndo,
    /// The config file is gone.
    Missing,
}

pub fn undo_file(rec: &FileRecord) -> anyhow::Result<Undo> {
    let current = match std::fs::read(&rec.path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Undo::Missing),
        Err(e) => return Err(e).with_context(|| format!("could not read {}", rec.path.display())),
    };
    let written = std::fs::read(&rec.written).ok();
    if rec.byte_restore_ok && written.as_deref() == Some(current.as_slice()) {
        let backup = std::fs::read(&rec.backup)
            .with_context(|| format!("the backup {} is missing", rec.backup.display()))?;
        write_atomic(&rec.path, &backup)?;
        return Ok(Undo::Restored);
    }
    let text = std::str::from_utf8(&current).context("the file is not UTF-8")?;
    let changes: Vec<Change> = rec.changes.iter().map(Change::from).collect();
    match unwrap_recorded(text, &changes).with_context(|| {
        format!(
            "could not understand {}\n  → your original is saved at {}",
            rec.path.display(),
            rec.backup.display()
        )
    })? {
        (Some(new_text), n) => {
            write_atomic(&rec.path, new_text.as_bytes())?;
            Ok(Undo::Unwrapped(n))
        }
        (None, _) => Ok(Undo::NothingToUndo),
    }
}

/// The meiose binary path to put into configs. Refuses throwaway locations.
pub fn meiose_path(explicit: Option<&Path>) -> anyhow::Result<String> {
    let p = match explicit {
        Some(p) => p.to_path_buf(),
        None => std::env::current_exe().context("could not find the meiose executable")?,
    };
    let s = p.to_string_lossy().to_string();
    if explicit.is_none() && is_throwaway(&s, &std::env::temp_dir().to_string_lossy()) {
        bail!(
            "meiose is running from a temporary location ({s}), which may disappear\n  \
             → install it permanently first (`cargo install meiose`, `brew install meiose`, or \
             `npm i -g meiose`), then run `meiose install` from there, or pass --meiose-path"
        );
    }
    Ok(s)
}

/// npx cache, temp dirs and debug builds disappear or change under you.
fn is_throwaway(path: &str, temp_dir: &str) -> bool {
    let norm = |p: &str| p.to_ascii_lowercase().replace('\\', "/");
    let lower = norm(path);
    let temp = norm(temp_dir);
    let temp = temp.trim_end_matches('/');
    lower.contains("/_npx/")
        || lower.contains("/target/debug/")
        || (!temp.is_empty() && lower.starts_with(temp))
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: &str = "/opt/bin/meiose";

    fn l(cmd: &str, args: &[&str]) -> Launch {
        Launch {
            command: cmd.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn wrap_and_unwrap_round_trip() {
        let orig = l("npx", &["-y", "@playwright/mcp@latest", "--", "x"]);
        let w = wrap(&orig, M);
        assert_eq!(w.command, M);
        assert_eq!(w.args[..3], ["run", "--", "npx"]);
        assert!(is_wrapped(&w));
        assert_eq!(unwrap_launch(&w), Some(orig));
    }

    #[test]
    fn recognises_existing_wraps_including_record_and_exe() {
        assert!(is_wrapped(&l(
            r"C:\Users\me\.cargo\bin\meiose.exe",
            &["record", "--", "npx"]
        )));
        assert!(is_wrapped(&l(
            "meiose",
            &["run", "--aggressive", "--", "x"]
        )));
        assert!(!is_wrapped(&l("npx", &["meiose", "run"])));
        assert!(!is_wrapped(&l("meiose", &["stats"])));
    }

    #[test]
    fn plan_skips_remote_wrapped_and_commandless_entries() {
        let text = r#"{
  "mcpServers": {
    "pw": { "command": "npx", "args": ["@playwright/mcp"] },
    "remote": { "type": "http", "url": "https://x" },
    "already": { "command": "meiose", "args": ["run", "--", "npx", "a"] },
    "weird": { "foo": 1 }
  },
  "projects": {
    "C:\\work": { "mcpServers": { "db": { "command": "uvx", "args": ["db-mcp"], "env": { "TOKEN": "s3cret" } } } }
  }
}
"#;
        let plan = plan_install(text, M, &[]).unwrap();
        let names: Vec<String> = plan
            .changes
            .iter()
            .map(|c| location_label(&c.location))
            .collect();
        assert_eq!(names, ["pw", r"db (project C:\work)"]);
        let skipped: Vec<_> = plan
            .skipped
            .iter()
            .map(|(l, s)| (l.last().unwrap().as_str(), s.clone()))
            .collect();
        assert_eq!(
            skipped,
            [
                ("remote", Skip::Remote),
                ("already", Skip::AlreadyWrapped),
                ("weird", Skip::NoCommand)
            ]
        );
        let new: Value = serde_json::from_str(plan.new_text.as_deref().unwrap()).unwrap();
        assert_eq!(
            new["projects"]["C:\\work"]["mcpServers"]["db"]["env"]["TOKEN"],
            "s3cret"
        );
        assert_eq!(
            new["projects"]["C:\\work"]["mcpServers"]["db"]["command"],
            M
        );
    }

    #[test]
    fn only_filter_limits_changes() {
        let text = r#"{"mcpServers":{"a":{"command":"x"},"b":{"command":"y"}}}"#;
        let plan = plan_install(text, M, &["b".into()]).unwrap();
        assert_eq!(plan.changes.len(), 1);
        assert_eq!(plan.changes[0].location, ["mcpServers", "b"]);
    }

    #[test]
    fn style_round_trips_common_formats_exactly() {
        for text in [
            "{\n  \"a\": 1,\n  \"b\": [\n    \"x\"\n  ],\n  \"c\": {}\n}\n",
            "{\r\n    \"a\": 1.50,\r\n    \"n\": 123456789012345678901234567890\r\n}",
            "{\n\t\"é\": \"日本\",\n\t\"s\": \"a/b\"\n}\n",
        ] {
            let doc: Value = serde_json::from_str(text).unwrap();
            assert_eq!(Style::detect(text).render(&doc).unwrap(), text);
        }
    }

    #[test]
    fn semantic_unwrap_keeps_other_changes() {
        let text = r#"{"mcpServers":{"pw":{"command":"npx","args":["a"]}}}"#;
        let plan = plan_install(text, M, &[]).unwrap();
        // Someone else edits the file afterwards.
        let mut doc: Value = serde_json::from_str(plan.new_text.as_deref().unwrap()).unwrap();
        doc["numStartups"] = 42.into();
        doc["mcpServers"]["new"] = serde_json::json!({"command": "z"});
        let edited = serde_json::to_string(&doc).unwrap();
        let (out, n) = unwrap_recorded(&edited, &plan.changes).unwrap();
        assert_eq!(n, 1);
        let out: Value = serde_json::from_str(&out.unwrap()).unwrap();
        assert_eq!(
            out["mcpServers"]["pw"],
            serde_json::json!({"command": "npx", "args": ["a"]})
        );
        assert_eq!(out["numStartups"], 42);
        assert_eq!(out["mcpServers"]["new"]["command"], "z");
    }

    #[test]
    fn semantic_unwrap_leaves_entries_the_user_changed_since() {
        let text = r#"{"mcpServers":{"pw":{"command":"npx","args":["a"]}}}"#;
        let plan = plan_install(text, M, &[]).unwrap();
        let edited = r#"{"mcpServers":{"pw":{"command":"node","args":["mine.js"]}}}"#;
        assert_eq!(unwrap_recorded(edited, &plan.changes).unwrap().1, 0);
    }

    #[test]
    fn throwaway_locations_are_detected() {
        let tmp = r"C:\Users\me\AppData\Local\Temp";
        assert!(is_throwaway(
            r"C:\Users\me\AppData\Local\npm-cache\_npx\ab\meiose.exe",
            tmp
        ));
        assert!(is_throwaway(
            r"C:\Users\me\AppData\Local\Temp\x\meiose.exe",
            tmp
        ));
        assert!(is_throwaway("/home/me/meiose/target/debug/meiose", "/tmp"));
        assert!(!is_throwaway(r"C:\Users\me\.cargo\bin\meiose.exe", tmp));
        assert!(!is_throwaway("/opt/homebrew/bin/meiose", "/tmp"));
        // An explicit path is always trusted.
        let p = std::env::temp_dir().join("x/meiose.exe");
        assert!(meiose_path(Some(&p)).is_ok());
    }
}
