# Changelog

## Unreleased

- `meiose_expand(id, query="…")` returns only the matching lines of a stored
  output, each with its enclosing row or block, instead of the whole thing.
  On the `Vec` docs, one method costs about 1,400 tokens instead of 244,000.
  Trim notes mention it.
- `--budget <TOKENS>` (off by default): Playwright snapshots over the budget
  are trimmed aggressively, then reduced to an outline that can be searched
  with `query`. Meant for agents that pass tool results to the model in full;
  in that eval it cut cost by about 93%. An outline can leave out links and
  buttons.
- Default mode hides a link's URL when the same URL already appeared on the
  page, and puts a link whose only child is its URL on one line. The 40 test
  pages now shrink by 31% instead of 23%.
- Fix: with `--budget`, a diff could describe lines the agent was never sent.
- `meiose log` and stats tell query expands from full ones.
- The report counts only committed fixtures (`--local` adds private ones); the
  README total previously included one private page.
- Trimming a 488K-token page takes 5.5 ms, up from 4.7 ms.

## 0.1.1 — 2026-11-02

- Faster dedupe signatures: text is hashed in 8-byte words and only for
  elements inside long sibling lists. Output is unchanged.
- README rewritten: per-page token table, manual setup for Claude Code,
  Claude Desktop and Cursor, and a "Why shouldn't I use meiose?" section.
  Benchmarks moved to BENCHMARKS.md.

## 0.1.0 — 2026-11-02

First release.

- `meiose run` / `record`: transparent stdio MCP proxy (both protocol eras).
- Playwright snapshot trimming (interactive refs always kept), snapshot diffs
  for repeat snapshots of a page, JSON and long-text handlers.
- `meiose_expand`: originals back byte-for-byte; SQLite store
  (`MEIOSE_KEEP_DAYS`, default 7, `0` = memory only).
- `meiose stats`, `meiose log`, `meiose install` / `uninstall` / `doctor` for
  Claude Code, Claude Desktop and Cursor.
- Fail-open everywhere; process-tree cleanup on Windows and Unix.
