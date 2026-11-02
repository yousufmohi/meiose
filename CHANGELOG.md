# Changelog

## 0.1.0 — unreleased

First release.

- `meiose run` / `record`: transparent stdio MCP proxy (both protocol eras).
- Playwright snapshot trimming (interactive refs always kept), snapshot diffs
  for repeat snapshots of a page, JSON and long-text handlers.
- `meiose_expand`: originals back byte-for-byte; SQLite store
  (`MEIOSE_KEEP_DAYS`, default 7, `0` = memory only).
- `meiose stats`, `meiose log`, `meiose install` / `uninstall` / `doctor` for
  Claude Code, Claude Desktop and Cursor.
- Fail-open everywhere; process-tree cleanup on Windows and Unix.
