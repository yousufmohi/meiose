# Changelog

## Unreleased

- Snapshot diffs no longer assume the agent has the previous snapshot. A
  repeat snapshot says "unchanged" only when the page itself is unchanged,
  not just the trimmed view (under `--aggressive` or `--budget` most of a
  page is hidden); every diff says to call `meiose_expand` if the previous
  snapshot is gone (saved to a file by the client, taken by a subagent, lost
  to context compaction); and for Claude Code, which saves results over
  50,000 characters to a file, no diff is made against a snapshot that was
  never shown to the model.

## 0.3.0 — 2026-11-03

- `meiose stats` says "1 tool call" and "1 expand" in the singular.
- **Stored outputs now expire after 24 hours, not 7 days.** Expanding an
  output from a session more than a day old no longer works.
  `MEIOSE_KEEP_HOURS` sets it; `MEIOSE_KEEP_DAYS` still works if
  `MEIOSE_KEEP_HOURS` is not set, so `MEIOSE_KEEP_DAYS=7` restores the old
  behaviour.
- `meiose run --no-store -- <server>` keeps that server's original outputs in
  memory only, for servers whose outputs hold secrets. Expand works until the
  session ends; stats are still recorded.

Security: secrets on disk.

- `meiose install` and `uninstall` keep a config file's permissions. A `600`
  `~/.claude.json` used to become `644`. The temporary file is created
  owner-only and removed if the write fails.
- On macOS and Linux the data directory is private: directories `700`, the
  database (and its WAL and SHM files), logs, backups, the manifest and
  recordings `600`, from the moment they are created, including recordings
  saved with `--out`. Existing data directories are tightened on startup.
- `meiose uninstall` deletes its config backups once a file is restored or
  unwrapped (`--keep-backups` keeps them). New `meiose purge` deletes stored
  outputs, logs, recordings and leftover backups after asking.
- Server arguments are no longer logged (only the program name). Recording
  headers and the install diff hide secret-looking values, and the server
  label in stats and recording filenames never includes a URL, an
  assignment or a flag's secret value.
- Pruned outputs are overwritten in the database (`secure_delete`) and the
  write-ahead log is truncated, so they are gone from disk. A running proxy
  also prunes every hour, not only every 200 outputs.

Security: hostile pages and servers.

- `meiose log`, `log --show` and `stats` strip control characters (OSC 52
  clipboard writes, colours, cursor moves) from what they print, including
  tool and server names. Stored originals and `meiose_expand` are unchanged.
  Piping them into `head` no longer panics.
- `meiose_expand` only returns outputs stored by the same server (same
  command line and directory), in memory and in the database. Outputs stored
  by older versions can no longer be expanded; `log --show` still shows them.
  Expand ids are 13 characters with 65 random bits from the OS (were 8).
- Messages over 16 MB pass through untouched, in chunks, without being
  parsed, trimmed or stored. A 300 MB output took 1.7 GB of memory; now 30 MB.
- The store queue is bounded (1024 jobs, 64 MB). When it is full, outputs go
  through untrimmed and call stats are dropped.
- The 512 MB cap counts the database and its write-ahead log on disk. Four
  processes writing 3.2 GB used to leave 3.1 GB (2.4 GB of it log); now
  under 500 MB at the end and about 850 MB at peak. meiose closes its
  database on exit, so the log is removed.

Security: release pipeline.

- Every GitHub Action in the release and CI workflows is pinned to a full
  commit SHA (release pins live in `dist-workspace.toml`).
- Release archives get GitHub artifact attestations
  (`gh attestation verify <file> --repo yousufmohi/meiose`).
- CI runs with a read-only token and runs `cargo audit`; Dependabot opens
  weekly update PRs for Cargo dependencies and CI actions.
- New SECURITY.md: how to report a vulnerability, and the threat model.

## 0.2.2 — 2026-11-02

- JSON output is capped at 45,000 characters, under Claude Code's 50,000
  limit, and stays valid JSON. One big object no longer falls through to the
  text handler's head-and-tail cut, and 10 sampled GitHub issues (70K
  characters) now fit (−82% → −88%). Over the cap, one-line fields come
  first, lists keep whole items, and nested values that don't fit are
  summarized (`{…900 keys: a, b, …}`).

## 0.2.1 — 2026-11-02

- A short JSON list of big items (16 GitHub pull requests, 85 KB) was cut
  mid-item by the text fallback. Lists under 40 items but over 40 KB now keep
  whole items up to 16 KB and summarize the rest, so the output stays valid
  JSON. Longer lists are trimmed as before.
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
