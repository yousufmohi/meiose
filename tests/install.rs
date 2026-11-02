//! `meiose install` / `uninstall` / `doctor` against fake home directories
//! (MEIOSE_FAKE_HOME). The core guarantee: uninstall restores every file
//! byte-for-byte when nothing else touched it, and otherwise unwraps only
//! what meiose wrapped while keeping everyone else's changes.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use meiose::install::Roots;
use serde_json::{Value, json};

struct Env {
    home: tempfile::TempDir,
    data: tempfile::TempDir,
}

const CLAUDE_CODE: &str = r#"{
  "numStartups": 12,
  "mcpServers": {
    "playwright": {
      "type": "stdio",
      "command": "npx",
      "args": [
        "-y",
        "@playwright/mcp@latest"
      ],
      "env": {
        "API_TOKEN": "s3cret-do-not-print"
      }
    },
    "remote-docs": {
      "type": "http",
      "url": "https://docs.example/mcp"
    },
    "pw-meiose": {
      "type": "stdio",
      "command": "C:\\Users\\me\\.cargo\\bin\\meiose.exe",
      "args": [
        "record",
        "--",
        "npx",
        "@playwright/mcp"
      ]
    }
  },
  "projects": {
    "C:\\work\\shop": {
      "allowedTools": [],
      "mcpServers": {
        "db": {
          "command": "uvx",
          "args": [
            "postgres-mcp",
            "--dsn",
            "postgres://localhost/shop"
          ]
        }
      },
      "lastCost": 0.000123456789,
      "big": 123456789012345678901234567890
    }
  },
  "名前": "ユーザー"
}
"#;

const DESKTOP: &str = "{\r\n    \"mcpServers\": {\r\n        \"filesystem\": {\r\n            \"command\": \"npx\",\r\n            \"args\": [\"-y\", \"@modelcontextprotocol/server-filesystem\", \"C:\\\\Users\\\\me\\\\Desktop\"]\r\n        }\r\n    }\r\n}\r\n";

const CURSOR: &str = "{\n\t\"mcpServers\": {\n\t\t\"github\": {\n\t\t\t\"command\": \"docker\",\n\t\t\t\"args\": [\n\t\t\t\t\"run\",\n\t\t\t\t\"-i\",\n\t\t\t\t\"ghcr.io/github/github-mcp-server\"\n\t\t\t]\n\t\t}\n\t}\n}";

impl Env {
    fn new() -> Env {
        let e = Env {
            home: tempfile::tempdir().unwrap(),
            data: tempfile::tempdir().unwrap(),
        };
        write(&e.claude_code(), CLAUDE_CODE);
        write(&e.desktop(), DESKTOP);
        write(&e.cursor(), CURSOR);
        e
    }

    fn roots(&self) -> Roots {
        Roots::fake(self.home.path())
    }

    fn claude_code(&self) -> PathBuf {
        self.home.path().join(".claude.json")
    }

    fn desktop(&self) -> PathBuf {
        self.roots()
            .config
            .join("Claude/claude_desktop_config.json")
    }

    fn cursor(&self) -> PathBuf {
        self.home.path().join(".cursor/mcp.json")
    }

    fn meiose(&self, args: &[&str], stdin: Option<&str>) -> (Output, String) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_meiose"))
            .args(args)
            .env("MEIOSE_FAKE_HOME", self.home.path())
            .env("MEIOSE_DATA_DIR", self.data.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        if let Some(s) = stdin {
            input.write_all(s.as_bytes()).unwrap();
        }
        drop(input);
        let out = child.wait_with_output().unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out, text)
    }

    fn install(&self, extra: &[&str], stdin: Option<&str>) -> (Output, String) {
        let mut args = vec!["install", "--meiose-path", FAKE_MEIOSE];
        args.extend_from_slice(extra);
        self.meiose(&args, stdin)
    }
}

const FAKE_MEIOSE: &str = "/opt/meiose/bin/meiose";

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn read(path: &Path) -> String {
    String::from_utf8(std::fs::read(path).unwrap()).unwrap()
}

fn json(path: &Path) -> Value {
    serde_json::from_str(&read(path)).unwrap()
}

#[test]
fn install_then_uninstall_restores_every_file_byte_for_byte() {
    let env = Env::new();
    let (out, text) = env.install(&["--yes"], None);
    assert!(out.status.success(), "{text}");

    // The diff shows what changes, and never prints env values.
    assert!(text.contains("  ~ playwright\n    - npx -y @playwright/mcp@latest\n    + /opt/meiose/bin/meiose run -- npx -y @playwright/mcp@latest"), "{text}");
    assert!(text.contains(r"~ db (project C:\work\shop)"), "{text}");
    assert!(text.contains("remote-docs (remote server)"), "{text}");
    assert!(
        text.contains("pw-meiose (already through meiose)"),
        "{text}"
    );
    assert!(!text.contains("s3cret"), "env values must never be printed");

    let cc = json(&env.claude_code());
    assert_eq!(cc["mcpServers"]["playwright"]["command"], FAKE_MEIOSE);
    assert_eq!(
        cc["mcpServers"]["playwright"]["args"],
        json!(["run", "--", "npx", "-y", "@playwright/mcp@latest"])
    );
    assert_eq!(
        cc["mcpServers"]["playwright"]["env"]["API_TOKEN"],
        "s3cret-do-not-print"
    );
    assert_eq!(
        cc["mcpServers"]["remote-docs"],
        json!({"type":"http","url":"https://docs.example/mcp"})
    );
    assert_eq!(
        cc["projects"]["C:\\work\\shop"]["mcpServers"]["db"]["command"],
        FAKE_MEIOSE
    );
    assert!(read(&env.claude_code()).contains("123456789012345678901234567890"));
    assert_eq!(
        json(&env.desktop())["mcpServers"]["filesystem"]["args"][0],
        "run"
    );
    assert_eq!(
        json(&env.cursor())["mcpServers"]["github"]["command"],
        FAKE_MEIOSE
    );
    // Formatting is kept: CRLF and 4 spaces, tabs.
    assert!(read(&env.desktop()).contains("\r\n        \"filesystem\""));
    assert!(read(&env.cursor()).starts_with("{\n\t\"mcpServers\""));

    let (out, text) = env.meiose(&["uninstall", "--yes"], None);
    assert!(out.status.success(), "{text}");
    assert_eq!(text.matches("✓ restored").count(), 3, "{text}");
    assert_eq!(read(&env.claude_code()), CLAUDE_CODE);
    assert_eq!(read(&env.desktop()), DESKTOP);
    assert_eq!(read(&env.cursor()), CURSOR);

    let (_, text) = env.meiose(&["uninstall", "--yes"], None);
    assert!(text.contains("Nothing to uninstall"), "{text}");
}

#[test]
fn only_the_command_and_args_lines_change() {
    let env = Env::new();
    env.install(
        &["--yes", "--client", "claude-code", "--only", "playwright"],
        None,
    );
    let before: Vec<&str> = CLAUDE_CODE.lines().collect();
    let after_text = read(&env.claude_code());
    let after: Vec<&str> = after_text.lines().collect();
    let changed: Vec<_> = similar::TextDiff::from_slices(&before, &after)
        .ops()
        .iter()
        .flat_map(|op| op.iter_changes(&before, &after).collect::<Vec<_>>())
        .filter(|c| c.tag() != similar::ChangeTag::Equal)
        .map(|c| c.value().trim().to_string())
        .collect();
    for line in &changed {
        assert!(
            line.starts_with("\"command\"") || line.starts_with('"') && !line.contains(':'),
            "unexpected changed line: {line}"
        );
    }
    assert!(!changed.is_empty());
}

#[test]
fn installing_twice_is_a_no_op() {
    let env = Env::new();
    env.install(&["--yes"], None);
    let snapshot = read(&env.claude_code());
    let (out, text) = env.install(&["--yes"], None);
    assert!(out.status.success());
    assert!(text.contains("Nothing to do"), "{text}");
    assert_eq!(read(&env.claude_code()), snapshot);
}

#[test]
fn asks_before_changing_anything() {
    let env = Env::new();
    for answer in [Some("n\n"), Some(""), None] {
        let (out, text) = env.install(&[], answer);
        assert!(!out.status.success());
        assert!(text.contains("[y/N]"), "{text}");
        assert!(text.contains("Nothing changed"), "{text}");
        assert_eq!(read(&env.claude_code()), CLAUDE_CODE);
    }
    let (out, text) = env.install(&[], Some("y\n"));
    assert!(out.status.success(), "{text}");
    assert_ne!(read(&env.claude_code()), CLAUDE_CODE);
}

#[test]
fn dry_run_changes_nothing() {
    let env = Env::new();
    let (out, text) = env.install(&["--dry-run"], None);
    assert!(out.status.success());
    assert!(text.contains("Dry run"), "{text}");
    assert!(text.contains("~ playwright"));
    assert_eq!(read(&env.claude_code()), CLAUDE_CODE);
    assert_eq!(read(&env.desktop()), DESKTOP);
}

#[test]
fn uninstall_after_the_app_rewrote_the_file_unwraps_and_keeps_its_changes() {
    let env = Env::new();
    env.install(&["--yes"], None);

    // Claude Code rewrites its config all the time: simulate that, plus a
    // server the user added after installing.
    let mut cc = json(&env.claude_code());
    cc["numStartups"] = 13.into();
    cc["mcpServers"]["added-later"] = json!({"command": "node", "args": ["srv.js"]});
    write(
        &env.claude_code(),
        &serde_json::to_string_pretty(&cc).unwrap(),
    );

    let (out, text) = env.meiose(&["uninstall", "--yes"], None);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("the file changed since install"), "{text}");
    assert!(text.contains("✓ unwrapped 2 server(s)"), "{text}");
    let cc = json(&env.claude_code());
    assert_eq!(cc["numStartups"], 13);
    assert_eq!(cc["mcpServers"]["added-later"]["command"], "node");
    let original: Value = serde_json::from_str(CLAUDE_CODE).unwrap();
    assert_eq!(
        cc["mcpServers"]["playwright"],
        original["mcpServers"]["playwright"]
    );
    assert_eq!(
        cc["projects"]["C:\\work\\shop"]["mcpServers"]["db"],
        original["projects"]["C:\\work\\shop"]["mcpServers"]["db"]
    );
    assert_eq!(
        cc["mcpServers"]["pw-meiose"], original["mcpServers"]["pw-meiose"],
        "never touched"
    );
    // Untouched files still come back byte-for-byte.
    assert_eq!(read(&env.desktop()), DESKTOP);
}

#[test]
fn reinstall_after_outside_edits_stays_reversible() {
    let env = Env::new();
    env.install(&["--yes", "--only", "playwright"], None);
    // The user edits the file, then installs again for another server.
    let mut cc = json(&env.claude_code());
    cc["theme"] = "dark".into();
    write(
        &env.claude_code(),
        &serde_json::to_string_pretty(&cc).unwrap(),
    );
    env.install(&["--yes", "--only", "db"], None);

    let (out, text) = env.meiose(&["uninstall", "--yes"], None);
    assert!(out.status.success(), "{text}");
    let cc = json(&env.claude_code());
    assert_eq!(cc["theme"], "dark", "the outside edit must survive");
    assert_eq!(cc["mcpServers"]["playwright"]["command"], "npx");
    assert_eq!(
        cc["projects"]["C:\\work\\shop"]["mcpServers"]["db"]["command"],
        "uvx"
    );
}

#[test]
fn doctor_reports_state_and_fixes() {
    let env = Env::new();
    let (out, text) = env.meiose(&["doctor"], None);
    assert!(text.contains("not going through meiose"), "{text}");
    assert!(text.contains("→ run `meiose install` to wrap it"), "{text}");
    let _ = out;

    // Wrapped with a path that does not exist → a failure with a fix.
    env.install(&["--yes"], None);
    let (out, text) = env.meiose(&["doctor"], None);
    assert!(!out.status.success(), "{text}");
    assert!(
        text.contains("/opt/meiose/bin/meiose, which does not exist"),
        "{text}"
    );
    assert!(
        text.contains("→ run `meiose install` again to update the path"),
        "{text}"
    );

    // Wrapped with a real binary → that server is fine.
    let env = Env::new();
    let real = env!("CARGO_BIN_EXE_meiose");
    env.meiose(
        &[
            "install",
            "--yes",
            "--client",
            "cursor",
            "--meiose-path",
            real,
        ],
        None,
    );
    let (_, text) = env.meiose(&["doctor"], None);
    assert!(
        text.contains("1 stdio server(s), 1 through meiose"),
        "{text}"
    );
    assert!(!text.contains("github: points to"), "{text}");
}

#[test]
fn no_configs_found_says_what_to_do() {
    let env = Env::new();
    std::fs::remove_file(env.claude_code()).unwrap();
    std::fs::remove_file(env.desktop()).unwrap();
    std::fs::remove_file(env.cursor()).unwrap();
    let (out, text) = env.install(&["--yes"], None);
    assert!(out.status.success());
    assert!(text.contains("No agent configs found"), "{text}");
    assert!(text.contains("→"), "{text}");
}

#[test]
fn broken_json_is_reported_and_left_alone() {
    let env = Env::new();
    write(&env.cursor(), "{ \"mcpServers\": { oops");
    let (out, text) = env.install(&["--yes"], None);
    assert!(!out.status.success());
    assert!(text.contains("could not understand"), "{text}");
    assert!(text.contains("→ fix the JSON"), "{text}");
    assert_eq!(read(&env.cursor()), "{ \"mcpServers\": { oops");
    // The other files were still installed.
    assert_eq!(
        json(&env.desktop())["mcpServers"]["filesystem"]["command"],
        FAKE_MEIOSE
    );
}
