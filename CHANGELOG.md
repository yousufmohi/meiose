# Changelog

## Unreleased

- `meiose_expand(query=…)` on JSON outputs: a match also brings the one-line
  fields of its object (number, state, URLs), so a title found in a GitHub
  issue list arrives with the issue's number and link. Measured on real GitHub
  MCP outputs: the last item of each list found 2/6 → 6/6.

## 0.2.0 — 2026-11-02

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
- Lighter on the host: stats use a token estimate instead of loading the
  o200k vocabulary, the runtime uses 2 worker threads instead of one per core,
  the in-memory cache of originals is capped at 32 MB (they are also on disk;
  128 MB with `MEIOSE_KEEP_DAYS=0`), and only the last 7 daily log files are
  kept. One meiose process went from 52.8 MB to 14.8 MB and from 23 to 13
  threads. `MEIOSE_EXACT_TOKENS=1` restores exact token counts.
- Trimming a 400K-token page takes 4.1 ms; a 488K-token page 5.1 ms (4.8 ms
  before the URL rules).

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
