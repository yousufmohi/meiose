<h1 align="center">meiose</h1>

<p align="center">
  An MCP proxy that trims oversized tool outputs before they reach your agent.<br>
  One Rust binary. Works with any stdio MCP server. Runs locally, no telemetry.
</p>

<p align="center">
  <a href="https://github.com/yousufmohi/meiose/actions/workflows/ci.yml"><img src="https://github.com/yousufmohi/meiose/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://crates.io/crates/meiose"><img src="https://img.shields.io/crates/v/meiose.svg" alt="crates.io"></a>
  <a href="https://www.npmjs.com/package/meiose"><img src="https://img.shields.io/npm/v/meiose.svg" alt="npm"></a>
  <a href="#license"><img src="https://img.shields.io/crates/l/meiose.svg" alt="License: MIT OR Apache-2.0"></a>
</p>

<p align="center">
  <a href="docs/configuration.md">Docs</a> ·
  <a href="BENCHMARKS.md">Benchmarks</a> ·
  <a href="https://github.com/yousufmohi/meiose/releases">Releases</a> ·
  <a href="CHANGELOG.md">Changelog</a> ·
  <a href="SECURITY.md">Security</a>
</p>

<p align="center">
  A test agent that sends all tool output to the model: 93% lower cost, 1 more wrong answer out of 57.<br>
  Claude Code, which already saves large outputs to a file: 7% lower cost, no more wrong answers.
</p>

![meiose install --dry-run, then meiose log and meiose stats after a real Playwright session: a trimmed page, a query expand and a diff](docs/demo.gif)

## Why meiose?

MCP servers return whatever they have. A Playwright snapshot of one docs page
is 244,000 tokens; a GitHub issue list is 90,000. Many clients send all of it to
the model on every turn (Cline, for one, passes MCP results through uncut:
[cline#15470](https://github.com/cline/cline/issues/15470)). meiose sits between
the agent and the server, cuts what the agent does not need, and keeps the
original one tool call away.

We ran 19 browsing tasks 3 times each (57 runs per setup) with Claude Haiku.
Full results are in [BENCHMARKS.md](BENCHMARKS.md).

| agent | cost per task without meiose | cost per task with meiose | cost saved | correct without meiose | correct with meiose |
|---|---|---|---|---|---|
| test agent that sends all tool output to the model | $0.0463 | $0.0032 | 93% | 55 of 57 | 54 of 57 |
| Claude Code | $0.0075 | $0.0069 | 7% | 57 of 57 | 57 of 57 |

The test agent ran with `--budget 20000`. Claude Code ran with meiose's defaults.

Costs vary between runs, so these savings are estimates. The 95% confidence
range is 44% to 96% for the test agent and 3% to 11% for Claude Code.

Per output:

| | without meiose | with meiose |
|---|---|---|
| Rust `Vec` docs snapshot | 244,156 tokens | 166,563 (32% less) |
| all 40 test pages | 943,239 tokens | 650,465 (31% less), 488,573 (48% less) with `--aggressive` |
| GitHub `list_issues`, 57 issues | 89,946 tokens | 11,864 (87% less) |
| the same page, snapshotted again | 135,571 tokens | 84 (a diff) |
| one method from the `Vec` docs | the whole page | 1,399 tokens, via `query` |
| getting the full output back | re-run the tool | call `meiose_expand(id)` |
| added latency | | 2.2 ms on the largest page |

The test agent is `eval/agent_loop.py`, a minimal script, not a product you can
install. A measurement in Cline is in progress.

## Install

| | |
|---|---|
| Homebrew | `brew install yousufmohi/tap/meiose` |
| npm | `npm install -g meiose` |
| Cargo | `cargo install meiose` |
| macOS / Linux | `curl -LsSf https://github.com/yousufmohi/meiose/releases/latest/download/meiose-installer.sh \| sh` |
| Windows | `irm https://github.com/yousufmohi/meiose/releases/latest/download/meiose-installer.ps1 \| iex` |

### Download

Prebuilt binaries are on the [releases page](https://github.com/yousufmohi/meiose/releases/latest):

| platform | archive |
|---|---|
| macOS, Apple silicon | `meiose-aarch64-apple-darwin.tar.xz` |
| macOS, Intel | `meiose-x86_64-apple-darwin.tar.xz` |
| Linux x64 | `meiose-x86_64-unknown-linux-gnu.tar.xz` |
| Linux arm64 | `meiose-aarch64-unknown-linux-gnu.tar.xz` |
| Windows x64 | `meiose-x86_64-pc-windows-msvc.zip` |

Each has a `.sha256` file. Releases after 0.2.2 also carry a GitHub artifact
attestation: `gh attestation verify <archive> --repo yousufmohi/meiose`.

### Build from source

```sh
git clone https://github.com/yousufmohi/meiose
cd meiose
cargo build --release      # Rust 1.88 or newer; binary in target/release/meiose
```

## Quick start

```sh
meiose install     # find your agent configs, show each change, ask, back up every file
# restart your agent and use it as usual
meiose stats       # what was saved
```

```text
Last 7 days: 26K tokens → 10K (61% saved)
  8 tool calls, 3 trimmed · 0 expands (0 tokens, included above)

  server                     tool               calls    before     after   saved
  npx-playwright-mcp-latest  browser_snapshot       4       26K      9.7K     62%
  npx-playwright-mcp-latest  browser_navigate       2       208       208      0%
  npx-playwright-mcp-latest  browser_click          1       126       126      0%
  npx-playwright-mcp-latest  browser_close          1        29        29      0%
```

`meiose install` knows Claude Code, Claude Desktop and Cursor. `meiose doctor`
checks the setup and prints a fix for each problem; `meiose uninstall` undoes
the install.

## Manual setup

Put `meiose run --` in front of any stdio server's command. Playwright MCP as
the example:

<details>
<summary>Claude Code</summary>

```sh
claude mcp add playwright -- meiose run -- npx -y @playwright/mcp@latest
```

</details>

<details>
<summary>Claude Desktop</summary>

In `claude_desktop_config.json` (`~/Library/Application Support/Claude/` on
macOS, `%APPDATA%\Claude\` on Windows):

```json
{
  "mcpServers": {
    "playwright": {
      "command": "/full/path/to/meiose",
      "args": ["run", "--", "npx", "-y", "@playwright/mcp@latest"]
    }
  }
}
```

Use the full path from `which meiose` (or `where meiose` on Windows). Desktop
apps often start without your shell's PATH.

</details>

<details>
<summary>Cursor</summary>

Add the same `playwright` entry as for Claude Desktop to `~/.cursor/mcp.json`.

</details>

<details>
<summary>Any other agent</summary>

Wherever the agent takes a server command, use
`meiose run -- <command> <args>` instead.

</details>

## Features

### Trimming

| server output | what meiose does |
|---|---|
| Playwright snapshots | drops wrapper nodes, refs of non-interactive elements and repeated URLs; every link, button and input keeps its ref |
| JSON (GitHub, databases, APIs) | long lists keep 10 samples and name the rest; output stays valid JSON, under 45,000 characters |
| other text over 40 KB | keeps the first 16 KB and the last 4 KB |
| anything else | passed through unchanged |

Every trimmed output ends with a note like
`[meiose: … call meiose_expand("k3x9q2abm7tw4") for the full snapshot]`.

### Expand and query

meiose adds one tool, `meiose_expand`, answered locally:

| call | returns |
|---|---|
| `meiose_expand(id)` | the original output, exactly as the server sent it |
| `meiose_expand(id, query="Canada")` | only the matching lines, with their context; `\|` separates alternatives |

### Snapshot diffs

A repeat snapshot of the page you are on is sent as a diff of what changed
(from 135,571 tokens to 84 for an unchanged Wikipedia page). `--no-diff` turns it off.

### Budget

`--budget 20000` caps Playwright snapshots at about that many tokens: larger
pages are trimmed harder, then become a searchable outline. It is off by
default; see [which settings to use](docs/configuration.md#which-settings-to-use).

### Privacy and storage

meiose runs locally and sends no telemetry. Originals are kept so expand works,
for 24 hours (readable by you only on macOS and Linux), then deleted. `--no-store` keeps one
server's outputs in memory only; `meiose purge` deletes everything now. Server
arguments never reach the logs. Details in [SECURITY.md](SECURITY.md).

## CLI reference

| command | |
|---|---|
| `meiose run [flags] -- <server cmd>` | run a server behind the proxy |
| `meiose record [--out DIR] [flags] -- <server cmd>` | like `run`, and log every message to a JSONL file (debugging) |
| `meiose install [--dry-run] [--yes] [--only NAME] [--client APP] [--meiose-path P]` | wrap the servers in your agent configs |
| `meiose uninstall [--yes] [--keep-backups]` | undo `install` |
| `meiose stats [--days 7]` | token savings |
| `meiose log [-n 20] [--show ID]` | recent calls; `--show` prints one call's trimmed and original output |
| `meiose doctor` | check configs, paths and versions |
| `meiose purge [--yes]` | delete stored outputs, logs, recordings and leftover backups |

`run` and `record` flags:

| flag | |
|---|---|
| `--aggressive` | trim harder: hide all non-interactive refs, drop link URLs, shorten more |
| `--budget TOKENS` | cap Playwright snapshots (0 = no limit, the default) |
| `--no-diff` | always send full snapshots |
| `--no-store` | keep this server's originals in memory only |
| `--passthrough` | change nothing (stats are still recorded) |

Environment variables (`MEIOSE_KEEP_HOURS`, `MEIOSE_DATA_DIR`, `MEIOSE_LOG`,
`MEIOSE_EXACT_TOKENS`) are in [docs/configuration.md](docs/configuration.md).

## How it works

```text
 agent ──stdin──▶ meiose ──stdin──▶ MCP server (e.g. npx @playwright/mcp)
 agent ◀─stdout── meiose ◀─stdout── MCP server
                    │
                    ├─ trims tools/call results (Playwright, JSON, text handlers)
                    ├─ sends repeat snapshots of a page as a diff
                    ├─ adds one tool, meiose_expand(id), answered locally
                    └─ stores originals and stats in SQLite (local only)
```

Every message meiose does not intentionally change is forwarded as is. If
anything inside meiose fails, the original result passes through. Rules,
guarantees and protocol support are in [docs/how-it-works.md](docs/how-it-works.md).

## Limitations

- In Claude Code the gains are small (7% of cost in the eval), since Claude
  Code already moves outputs over 50,000 characters to a file.
- `--budget` outlines can leave out links and buttons. They stay reachable
  through `meiose_expand`, but that is less certain than a full snapshot.
- A link whose URL appeared earlier on the page is shown without it; it can
  still be clicked.
- Text outputs over 40 KB are cut to their head and tail. If that output is a
  file you want to edit, the agent has to call `meiose_expand` first.
- Only Playwright gets structure-aware trimming, and remote HTTP/SSE servers
  are not proxied.
- Tested live with Claude Code and Playwright MCP 1.65. Setup for Claude
  Desktop and Cursor is tested on sample config files only.

## Documentation

- [Configuration](docs/configuration.md): flags, environment variables, supported servers, uninstall
- [Troubleshooting](docs/troubleshooting.md)
- [How it works](docs/how-it-works.md): trimming rules, guarantees, storage, protocol support
- [Benchmarks](BENCHMARKS.md) · [Testing](TESTING.md) · [Changelog](CHANGELOG.md) · [Security](SECURITY.md)

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in meiose by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
