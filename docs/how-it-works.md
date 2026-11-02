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

A trimmed result ends with a note like this one, from the Hacker News front
page:

```text
[meiose: 137 nodes trimmed, 319 non-interactive refs hidden, 35 repeated URLs hidden — call meiose_expand("k3x9q2ab") for the full snapshot, or add query="…" for only the lines you need]
```

When the agent needs something that was left out, it calls `meiose_expand`
with that id and gets the server's original result back byte-for-byte.

## Partial expand

`meiose_expand(id, query="…")` searches the stored original instead of
returning all of it. The search is case-insensitive, and `|` separates
alternatives (`query="Canada|Mexico"`). Each matching line comes back with its
ancestors and its largest enclosing element that is at most 40 lines long, so
a match in a table cell brings its whole row and a method name in API docs
brings its whole header. Up to 30 matches and about 24 KB are returned; the
reply says how many matched in total. Refs in the result can be used directly,
for example to click a link the trimmed page did not show.

## Handlers

Handlers are tried in order. The first one that makes an output smaller wins.

Playwright snapshots, default mode:

- collapse empty wrapper elements and drop empty nodes;
- hide refs on purely structural elements (rows, cells, paragraphs, …);
  links, buttons, inputs and anything clickable keep theirs;
- drop `[cursor=pointer]` where the role already implies it;
- collapse runs of identical siblings;
- shorten very long text;
- hide a link's URL when the same URL already appeared on the page;
- put a link whose only child is its URL on one line:
  `- link "Docs" [ref=e12]: /docs`.

`--aggressive` also hides every non-interactive ref, drops link URLs, collapses
runs of structurally similar siblings and shortens text sooner.

A repeat snapshot of the same page, within 10 minutes of the previous one, is
sent as a diff of what changed. Playwright refs are stable within one loaded
page, so unchanged elements remain valid in the agent's context. They are not
stable across navigations: reloading a page renames every ref, so a snapshot
after a reload differs almost everywhere and is sent in full.
`--no-diff` turns this off.

## Budget

`--budget <TOKENS>` (off by default) caps the size of a Playwright snapshot.
A snapshot over the budget is first trimmed as in `--aggressive`. If it is
still too large, it becomes an outline: the page's structure (headings,
landmarks, forms) first, then form controls, then the rest of the page in
order until the budget is used. The outline's note says it is an outline and
how to search the full page with `query`. The budget is converted at 3 bytes
per token, which avoids counting tokens on the message path, so it is
approximate.

An outline can leave out links and buttons. They are still reachable through
`meiose_expand`, with `query` or in full, and refs in the results work as
usual. In Claude Code, keep the budget under about 16,000 tokens if you use
it, so that outlines stay under Claude Code's 50,000-character limit and the
note reaches the agent.

## Other handlers

JSON: arrays longer than 40 items keep their first 10 plus the item keys.
Numbers are preserved exactly.

Text: outputs longer than 40 KB keep the head and the tail.

## Guarantees

These properties are tested on every commit on macOS, Linux and Windows.

- Without `--budget`, interactive elements are never removed. Every link,
  button, input, checkbox, option or element with `[cursor=pointer]` keeps its
  `[ref=…]`. This is checked on 40 real pages, on generated trees and on random
  page fragments, with a classifier written independently of the trimmer.
  With `--budget`, an outline can leave interactive elements out; they stay
  reachable through `meiose_expand`.
- No distinct URL is lost: hiding repeated URLs keeps every distinct URL
  visible somewhere on the page (tested on every fixture).
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

Recent originals are also cached in memory, up to 32 MB per meiose process
(128 MB with `MEIOSE_KEEP_DAYS=0`, where memory is the only copy). Logs go to
daily files in `<data dir>/logs/`; the last 7 are kept.

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
