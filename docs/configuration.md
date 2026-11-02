# Configuration

There is no config file. Everything below is optional.

## Flags

| | |
|---|---|
| `meiose run --aggressive -- …` | trim harder: hide all non-interactive refs, drop link URLs, shorten more |
| `meiose run --no-diff -- …` | always send full snapshots |
| `meiose run --passthrough -- …` | change nothing (still records stats) |
| `meiose record -- …` | passthrough plus a JSONL log of all traffic, for debugging and fixtures |
| `meiose install --only playwright` | wrap only some servers |
| `meiose install --client cursor` | touch only some apps |
| `meiose install --dry-run` | show what would change |
| `meiose log [-n 20]` | recent calls with tokens before and after |
| `meiose log --show <id>` | trimmed and original output for one call |

## Environment variables

| | |
|---|---|
| `MEIOSE_KEEP_DAYS` | days to keep originals on disk (default 7, `0` = memory only) |
| `MEIOSE_DATA_DIR` | where the database, logs and backups live |
| `MEIOSE_LOG=debug` | more detail in `<data dir>/logs/` |

## Supported servers

| server | handling |
|---|---|
| Playwright MCP (`@playwright/mcp`) | snapshot trimming and diffs (tested with 1.65) |
| servers returning JSON (databases, APIs, GitHub, …) | JSON handler |
| any stdio MCP server | text handler for outputs over 40 KB, otherwise passthrough |
| remote (HTTP/SSE) servers | not proxied; `install` leaves them alone |

## Supported agents

`meiose install` knows the config files of Claude Code (user and per-project
servers), Claude Desktop (including the Microsoft Store install) and Cursor.
Anything that starts a stdio MCP server can use
`meiose run -- <server command>` directly.

## Uninstall

`meiose uninstall` restores each config file exactly as it was if nothing else
changed it since install. If the file changed (Claude Code rewrites its config
often), it puts back the original command of every server meiose wrapped and
keeps your other changes. Backups stay in the data directory.

Then remove the binary with the tool you installed it with:
`brew uninstall meiose`, `npm uninstall -g meiose` or `cargo uninstall meiose`.
