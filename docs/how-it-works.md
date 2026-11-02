# How meiose works

```text
 agent ──stdin──▶ meiose ──stdin──▶ MCP server (e.g. npx @playwright/mcp)
 agent ◀─stdout── meiose ◀─stdout── MCP server
                    │
                    ├─ trims tools/call results (Playwright, JSON, text handlers)
                    ├─ sends repeat snapshots of a page as a diff
                    ├─ adds one tool, meiose_expand(id), answered locally
                    └─ stores originals and stats in SQLite (local only)
```

`meiose install` changes each server's command from `npx @playwright/mcp` to
`meiose run -- npx @playwright/mcp`. Nothing else in your config changes.

Messages meiose does not intentionally change are forwarded exactly as
received: requests, notifications, errors and server stderr.

A trimmed result ends with a note like this:

```text
[meiose: 312 nodes trimmed — call meiose_expand("k3x9q2ab") for the full snapshot]
```

When the agent needs something that was left out, it calls `meiose_expand`
with that id and gets the server's original result back byte-for-byte.

## Handlers

Handlers are tried in order. The first one that makes an output smaller wins.

Playwright snapshots, default mode:

- collapse empty wrapper elements and drop empty nodes;
- hide refs on purely structural elements (rows, cells, paragraphs, …);
  links, buttons, inputs and anything clickable keep theirs;
- drop `[cursor=pointer]` where the role already implies it;
- collapse runs of identical siblings;
- shorten very long text.

`--aggressive` also hides every non-interactive ref, drops link URLs, collapses
runs of structurally similar siblings and shortens text sooner.

A repeat snapshot of the same page, within 10 minutes of the previous one, is
sent as a diff of what changed. Playwright refs stay stable across snapshots of
a page, so unchanged elements remain valid in the agent's context.
`--no-diff` turns this off.

JSON: arrays longer than 40 items keep their first 10 plus the item keys.
Numbers are preserved exactly.

Text: outputs longer than 40 KB keep the head and the tail.

## Guarantees

These properties are tested on every commit on macOS, Linux and Windows.

- Interactive elements are never removed. Every link, button, input,
  checkbox, option or element with `[cursor=pointer]` keeps its `[ref=…]`.
  This is checked on 40 real pages, on generated trees and on random page
  fragments, with a classifier written independently of the trimmer.
- A trimmed output is always smaller than the original and says how to get
  the original back.
- meiose fails open. If trimming fails or panics, if the store cannot be
  opened, or if the agent never saw `meiose_expand`, the original passes
  through unchanged.
- The proxy does not hang on slow servers, crashing servers, 5 MB messages,
  malformed JSON or out-of-order responses.
- meiose makes no network calls and sends no telemetry. It never writes logs
  to stdout.

## Process handling

The server and everything it started stop with meiose. On Windows the server
runs in a Job Object that is killed when meiose exits, even if meiose itself is
killed. On Unix the server leads its own process group, which is signalled on a
forced stop. Ctrl-C, SIGTERM and Ctrl-Break stop the server and exit.

## Storage and retention

Original tool outputs (what pages or tools returned) are kept in SQLite in your
data directory for 7 days, so `meiose_expand` and `meiose log` work. Set
`MEIOSE_KEEP_DAYS` to change that; `MEIOSE_KEEP_DAYS=0` keeps originals in
memory for the session only. Stored originals are capped at 512 MB, oldest
first.

`meiose install` keeps backups of the config files it changes in the same
directory. Those are full copies, including any `env` values (API keys).

`meiose record` writes every message to a JSONL file, including anything typed
into forms. Use it for debugging only.

Data directory: `%LOCALAPPDATA%\meiose` on Windows,
`~/Library/Application Support/meiose` on macOS, `~/.local/share/meiose` on
Linux, or `MEIOSE_DATA_DIR` if set.

## Protocol

meiose speaks raw newline-delimited JSON-RPC over stdio and does not use an MCP
SDK. It works with both MCP protocol eras: the 2026-07-28 spec (no handshake,
`server/discover`) and `initialize`-based servers up to 2025-11-25. Request ids
are tracked per direction, because servers in the older era send their own
requests to the client and ids can collide.
