# meiose

An MCP proxy that trims oversized tool outputs before they reach your agent, written in Rust.

[![CI](https://github.com/yousufmohi/meiose/actions/workflows/ci.yml/badge.svg)](https://github.com/yousufmohi/meiose/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/meiose.svg)](https://crates.io/crates/meiose)
[![npm](https://img.shields.io/npm/v/meiose.svg)](https://www.npmjs.com/package/meiose)
[![License: MIT OR Apache-2.0](https://img.shields.io/crates/l/meiose.svg)](#license)

![meiose install --dry-run, meiose log and meiose stats after a real Playwright session](docs/demo.gif)

Playwright MCP snapshots of real pages, in tokens:

| page | before | default | `--aggressive` |
|---|---:|---:|---:|
| Rust `Vec` docs | 244,156 | 166,563 (−32%) | 133,879 (−45%) |
| MDN `<input>` reference | 166,768 | 112,891 (−32%) | 93,199 (−44%) |
| Wikipedia "Rust" | 133,768 | 105,700 (−21%) | 81,203 (−39%) |
| Hacker News | 13,713 | 8,262 (−40%) | 5,696 (−58%) |
| all 40 test pages | 943,239 | 650,329 (−31%) | 488,437 (−48%) |
| repeat snapshot of an unchanged page | 133,862 | 79 | — |
| one method from the `Vec` docs, via `query` | 244,156 | 1,399 | — |

Trimming the largest page takes 2.4 ms. In Claude Code the end-to-end saving is
7% of cost at the same accuracy; agents without an output limit save far more
with `--budget`. [BENCHMARKS.md](BENCHMARKS.md) has the evals.

## Install

```sh
brew install yousufmohi/tap/meiose
npm install -g meiose
cargo install meiose
curl -LsSf https://github.com/yousufmohi/meiose/releases/latest/download/meiose-installer.sh | sh
```

```powershell
irm https://github.com/yousufmohi/meiose/releases/latest/download/meiose-installer.ps1 | iex
```

## Quick start

1. Run `meiose install`. It finds your Claude Code, Claude Desktop and Cursor
   configs, shows each change, asks, and backs up every file it edits.
2. Restart your agent and use it as usual.
3. Run `meiose stats`:

```text
Last 7 days: 26K tokens → 10K (61% saved)
  8 tool calls, 3 trimmed · 0 expands (0 tokens, included above)

  server                     tool               calls    before     after   saved
  npx-playwright-mcp-latest  browser_snapshot       4       26K      9.7K     62%
  npx-playwright-mcp-latest  browser_navigate       2       208       208      0%
  npx-playwright-mcp-latest  browser_click          1       126       126      0%
  npx-playwright-mcp-latest  browser_close          1        29        29      0%
```

`meiose doctor` checks the setup and prints a fix for each problem.
`meiose uninstall` undoes the install.

## Manual setup

Put `meiose run --` in front of the server's command. `meiose install` writes
the same entries, with the full path to meiose. Playwright MCP as the example:

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

Any stdio MCP server works the same way: `meiose run -- <command> <args>`.

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

meiose passes through every message it does not intentionally change. When a
tool returns a large result, meiose trims its text and adds a note with an id.
The agent can call `meiose_expand(id)` to get the original byte-for-byte, or
`meiose_expand(id, query="…")` to get only the matching lines with their
context. A repeat snapshot of the same page is sent as a diff of what changed.
In the default mode, links, buttons, inputs and anything clickable always keep
their refs. With `--budget`, a page over the budget becomes an outline that can
leave some of them out; they stay reachable through `meiose_expand`. If anything
inside meiose fails, the original result passes through; it runs locally and
sends no telemetry. Details in [docs/how-it-works.md](docs/how-it-works.md).

## Why shouldn't I use meiose?

- In Claude Code the gains are small. Claude Code already saves MCP text
  results over 50,000 characters to a file instead of showing them to the
  model, so meiose only saves on mid-size outputs and repeat snapshots. In the
  end-to-end eval that was 7% lower cost and no change in tokens.
- `--budget` cuts cost sharply for agents without that limit, but its outlines
  can leave out links and buttons. In testing, agents found what they needed
  with `query` on large pages, but this is less certain than a full snapshot.
- A link whose URL already appeared earlier on the page is shown without it.
  The link can still be clicked; `meiose_expand` shows its URL.
- It keeps copies of tool outputs on disk for 7 days by default.
  `MEIOSE_KEEP_DAYS=0` keeps them in memory only.
- Text outputs over 40 KB are cut to their head and tail. If that output is a
  file you want to edit, the agent has to call `meiose_expand` first.
- Only Playwright snapshots get structure-aware trimming. Other servers get the
  generic JSON and text handlers, and remote HTTP/SSE servers are not proxied.
- It has been tested live with Claude Code and Playwright MCP 1.65. Setup for
  Claude Desktop and Cursor is tested on sample config files only.

## Documentation

- [Configuration](docs/configuration.md): flags, environment variables, supported servers, uninstall
- [Troubleshooting](docs/troubleshooting.md)
- [How it works](docs/how-it-works.md): trimming rules, guarantees, storage, protocol support
- [Benchmarks](BENCHMARKS.md)
- [Testing](TESTING.md)
- [Changelog](CHANGELOG.md)

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in meiose by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
