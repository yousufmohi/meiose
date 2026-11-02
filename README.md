# meiose

**A context-trimming proxy for MCP servers.** It sits between your AI agent and
its MCP servers, shrinks bulky tool outputs (browser snapshots, big JSON, huge
text) before they reach the model, and keeps every original one tool call away.

[![CI](https://github.com/yousufmohi/meiose/actions/workflows/ci.yml/badge.svg)](https://github.com/yousufmohi/meiose/actions/workflows/ci.yml)

## What it saves

Measured, not estimated. Claude Code headless (`claude -p`, Claude Haiku) running
15 browser tasks through Playwright MCP, 2 runs each, with and without meiose.
Every task has an answer that only the page contains, checked automatically.

| | without meiose | with meiose |
|---|---:|---:|
| tasks answered correctly | 30/30 | **30/30** |
| billed tokens per task | 199,103 | **187,312 (−6%)** |
| cost per task | $0.0077 | **$0.0070 (−9%)** |
| tool-output tokens | 293K | **219K (−23%)** |
| repeat snapshot of an unchanged page | 134K tokens | **81 tokens** |
| added latency per tool call | — | **~2 ms** for a 500 KB snapshot |

On 40 real pages (news, docs, shops, forms, Wikipedia), snapshots shrink by
**23%** in the default mode and **48%** with `--aggressive`; trimming a
490K-token snapshot takes **3.9 ms**.

**Why the end-to-end number is smaller than the trimming number:** Claude Code
already keeps tool results above ~25K tokens away from the model (it saves them
to a file), and its own system prompt is ~15–20K tokens per turn. So in Claude
Code, meiose helps on mid-size pages and repeated snapshots. Agents without
that cap (Claude Desktop, Cursor, your own) get the full trimming effect.
Reproduce everything with [`eval/run_eval.py`](eval/run_eval.py); details in
[TESTING.md](TESTING.md).

<!-- TODO demo GIF (docs/demo.gif): `meiose install`, a browsing session, then `meiose stats` -->

## Install (30 seconds)

```bash
brew install yousufmohi/tap/meiose        # macOS / Linux
npm install -g meiose                      # anywhere with Node
cargo install meiose                       # from source
curl -LsSf https://github.com/yousufmohi/meiose/releases/latest/download/meiose-installer.sh | sh
```

Windows (PowerShell):

```powershell
irm https://github.com/yousufmohi/meiose/releases/latest/download/meiose-installer.ps1 | iex
```

Then wrap your agents' MCP servers and keep working as before:

```bash
meiose install      # finds Claude Code, Claude Desktop and Cursor configs,
                    # shows exactly what changes, asks, backs everything up
# restart your agent, use it normally, then:
meiose stats        # Last 7 days: 2.1M tokens → 1.7M (19% saved)
```

## How it works

```
 agent ──stdin──▶ meiose ──stdin──▶ MCP server (e.g. npx @playwright/mcp)
 agent ◀─stdout── meiose ◀─stdout── MCP server
                    │
                    ├─ trims tools/call results (Playwright, JSON, text handlers)
                    ├─ sends repeat snapshots of a page as a diff
                    ├─ adds one tool, meiose_expand(id), answered locally
                    └─ stores originals + stats in SQLite (local only)
```

- `meiose install` changes each server's command from `npx @playwright/mcp` to
  `meiose run -- npx @playwright/mcp`. Nothing else in your config changes.
- Every message meiose does not intentionally change is forwarded
  **byte-for-byte**: requests, notifications, errors, server stderr.
- A trimmed result ends with a note such as
  `[meiose: 312 nodes trimmed — call meiose_expand("k3x9q2ab") for the full snapshot]`.
  The agent calls `meiose_expand` when it needs what was left out and gets the
  server's original result back, byte-for-byte.
- **Playwright snapshots** (default mode): collapse empty wrapper elements,
  drop empty nodes, hide refs on purely structural elements (rows, cells,
  paragraphs), drop redundant `[cursor=pointer]`, collapse runs of identical
  siblings, shorten very long text. A repeat snapshot of the same page within
  10 minutes is sent as a diff of what changed.
- **JSON**: long arrays keep their first 10 items plus the item keys; numbers
  are preserved exactly. **Text**: very long outputs (>40 KB) keep head and tail.

Works with both MCP protocol eras: the 2026-07-28 spec (no handshake,
`server/discover`) and `initialize`-based servers up to 2025-11-25.

## Safety guarantees

These are tested on every commit (macOS, Linux, Windows):

- **Interactive elements are never removed.** Every link, button, input,
  checkbox, option or element with `[cursor=pointer]` keeps its `[ref=…]`.
  Checked on 40 real pages, generated trees and random page fragments.
- **Never larger, always marked.** A trimmed output is smaller than the
  original and says how to get the original back.
- **Expand is exact.** `meiose_expand` returns the server's result byte-for-byte.
- **Fail open.** If trimming fails or panics, if the store cannot be opened,
  or if the agent never saw `meiose_expand`, the original passes through
  unchanged. The proxy never hangs on slow servers, crashing servers, 5 MB
  messages, malformed JSON or out-of-order responses.
- **No orphans.** The server and everything it spawned are stopped with meiose
  (Windows Job Object; Unix process group).
- **Local only.** No network calls, no telemetry. Logs never go to stdout.

**What is stored:** original tool outputs (what pages or tools returned) are
kept in SQLite in your data directory for 7 days so `meiose_expand` and
`meiose log` work. Set `MEIOSE_KEEP_DAYS=0` to keep them in memory only, or
another number of days. `meiose install` backups (copies of your config files,
including any `env` values) live in the same directory.

## Supported servers

| server | handling |
|---|---|
| Playwright MCP (`@playwright/mcp`) | snapshot trimming + diffs (tested with 1.65) |
| servers returning JSON (databases, APIs, GitHub, …) | JSON handler |
| any stdio MCP server | text handler for outputs > 40 KB, otherwise passthrough |
| remote (HTTP/SSE) servers | not proxied; `install` leaves them alone |

Agents: Claude Code (user and project scopes), Claude Desktop (incl. Microsoft
Store install), Cursor. Anything that launches a stdio MCP server can use
`meiose run -- <server command>` directly.

## Configuration

No config file. Everything is optional:

| | |
|---|---|
| `meiose run --aggressive -- …` | trim harder: hide all non-interactive refs, drop link URLs, shorten more |
| `meiose run --no-diff -- …` | always send full snapshots |
| `meiose run --passthrough -- …` | change nothing (still records stats) |
| `meiose record -- …` | passthrough plus a JSONL log of all traffic, for debugging and fixtures |
| `meiose install --only playwright` | wrap only some servers (`--client cursor` for some apps, `--dry-run` to preview) |
| `meiose log [-n 20]` / `meiose log --show <id>` | recent calls; trimmed vs original for one |
| `MEIOSE_KEEP_DAYS` | days to keep originals on disk (default 7, `0` = memory only) |
| `MEIOSE_DATA_DIR` | where the database, logs and backups live |
| `MEIOSE_LOG=debug` | more detail in `<data dir>/logs/` |

## Troubleshooting

Start with `meiose doctor`: it checks the binary, the database and every server
entry in every config, and prints a `→` fix for each problem.

- **A server stopped starting after install.** Run `meiose doctor`. If it says
  the server's command is not on PATH, use its full path in the config: your
  agent may start servers with a different PATH than your terminal.
- **The agent misses something on a page.** It should call `meiose_expand`;
  smaller models sometimes don't. Try `--no-diff`, or `--passthrough` to
  compare, and open an issue with `meiose log --show <id>`.
- **`meiose install` refuses: "running from a temporary location".** You ran it
  through `npx` or a build folder. Install meiose permanently first (see above),
  or pass `--meiose-path`.
- **Windows: "Access is denied" when updating meiose.** A running agent is using
  it. Close those sessions and try again.

## Uninstall

```bash
meiose uninstall
```

Restores each config file byte-for-byte if nothing else changed it since
install; otherwise puts back the original command of every server meiose
wrapped and keeps your other changes. Backups stay in the data directory.
Then remove the binary with the tool you installed it with
(`brew uninstall meiose`, `npm uninstall -g meiose`, `cargo uninstall meiose`).

## License

MIT or Apache-2.0, at your option.
