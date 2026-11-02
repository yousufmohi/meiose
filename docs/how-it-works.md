# How meiose works

```
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

```
[meiose: 312 nodes trimmed — call meiose_expand("k3x9q2ab") for the full snapshot]
```

When the agent needs something that was left out, it calls `meiose_expand`
with that id and gets the server's original result back unchanged.

## Handlers

Handlers are tried in order; the first one that makes an output smaller wins.

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

## Protocol

meiose speaks raw newline-delimited JSON-RPC over stdio and does not use an MCP
SDK. It works with both MCP protocol eras: the 2026-07-28 spec (no handshake,
`server/discover`) and `initialize`-based servers up to 2025-11-25. Request ids
are tracked per direction, because servers in the older era send their own
requests to the client and ids can collide.
