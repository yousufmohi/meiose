# meiose — project context for agents

A single Rust binary that sits between an AI agent and any stdio MCP server,
forwards everything transparently, and trims bulky `tools/call` outputs (with
on-demand `meiose_expand` to get the original back).

```
agent ⇄ meiose ⇄ real MCP server (e.g. npx @playwright/mcp)
```

## Current status

| Milestone | Scope | Status |
|---|---|---|
| M1 | Passthrough proxy + `record` | Done (2026-11-01) |
| M2 | Playwright handler + snapshot/property tests + benchmark | Done (2026-11-01) |
| M3 | `meiose_expand` + store + `stats` + `log` + json/text handlers + snapshot diffing | Done (2026-11-01) |
| M4 | `install` / `uninstall` / `doctor` + install tests | Done (2026-11-01) |
| M5 | Protocol test suite + fail-open hardening | Done (2026-11-01); eval harness pulled forward from M6 |
| M6 | Eval harness + README + cargo-dist + npm wrapper | Done — **v0.1.0 released 2026-11-02** (GitHub, Homebrew tap, crates.io, npm); 0.1.1 released |
| Perf | URL rules, `query` expand, `--budget`, uncapped-agent eval, lighter footprint | Done — **0.2.0 released 2026-11-02** (PR #1) |
| JSON | measured on GitHub MCP; query brings JSON fields; short big lists stay valid JSON | Done — **0.2.2 released 2026-11-02** (0.2.1: query fields, short lists; 0.2.2: 45K output cap) |
| Security | audit fixes: private files, purge, redaction, secure delete, escapes, scoped expand, size limits, pinned/attested releases; 24 h retention, `--no-store` | Done — **0.3.0 released 2026-11-03** |

Work one milestone at a time. After each: report what was built, test results
and open questions, then **wait for the user's go-ahead**.

Name check (2026-11-01): `meiose` is free on crates.io, npm, Homebrew
(formula + cask) and as a GitHub org.

## Non-negotiable rules

1. **Transparency.** Any message meiose does not intentionally modify is
   forwarded byte-for-byte (unknown methods, notifications, errors, stderr).
   Never parse-and-reserialize a message you are not changing.
2. **stdout carries only protocol messages.** Logs go to the log file or stderr.
3. **Never break the agent.**
   - Under no budget, never drop an interactive element (`[ref=...]` that
     is clickable, fillable or selectable). Under a budget, an outline may omit
     them; they must stay reachable through `meiose_expand` (full or `query`).
     The default budget is 20,000 tokens, or none when `initialize` names
     Claude Code (the maintainer asked for this on 2026-11-06). `--budget 0`
     turns it off. A snapshot that does not parse is never outlined.
   - Every trimmed output has a marker:
     `[312 nodes trimmed — call meiose_expand("k3x9q2abm7tw4")]`.
   - `meiose_expand` returns the original output byte-for-byte.
4. **Fail open.** If trimming errors or panics, forward the original and log it.
   Never hang, deadlock or corrupt the stream.
5. **Overhead.** Trimming a 400K-token snapshot takes < 5 ms (benchmarked).
6. **Zero config.** Conservative defaults; aggressive mode is opt-in.
7. Every error message says what went wrong **and** what to do next.
8. `install` prints a diff per config change and asks to confirm (`--yes` skips),
   and writes a file only if it is still what that diff was computed from;
   otherwise it plans again from the current content (and asks again if the
   diff changed). Never revert another program's edits.
9. Write tests alongside code. **Never weaken a test to make it pass**; report it.
10. Small readable modules; no abstraction beyond the handler trait.

## Protocol facts (verified against the spec, 2026-11-01)

Latest spec is **2026-07-28** ("modern"); `2025-11-25` and earlier are
"legacy". meiose must handle both, because real clients and servers are mixed.

stdio framing (identical in both eras):
- One JSON-RPC message per line, `\n`-delimited, **no embedded newlines**, UTF-8.
- No batching (single request / notification / response per line).
- Server may log anything to stderr; client may forward it. meiose forwards it.
- Shutdown: client closes server stdin → wait → force-kill. Server may exit by
  closing stdout.

Era differences that matter to a proxy:
- **Legacy:** `initialize` handshake; the *server may send requests* to the
  client (`sampling/createMessage`, `roots/list`, `elicitation/create`) and the
  client answers. Request ids are therefore namespaced per direction.
- **Modern:** no handshake; every request carries
  `_meta["io.modelcontextprotocol/protocolVersion"]`; clients may probe with
  `server/discover`; servers never send requests (server→client input uses
  `resultType: "input_required"` results); results carry `resultType`.
- `tools/list` is paginated (`cursor` / `nextCursor`) and may carry `ttlMs`.
  Inject `meiose_expand` only into the first page (request had no `cursor`).
- Only trim `tools/call` results that are complete (`resultType` absent or
  `"complete"`) and not JSON-RPC errors. Leave `structuredContent`, images,
  audio and resource links alone unless a handler explicitly owns them.

## Observed in real traffic (M1 recordings, 2026-11-01)

- **Claude Code is dual-era**: it sends `server/discover` (string id
  `server-discover-probe-1`) first, then falls back to `initialize` on error.
- **Ids collide across directions**: client `initialize` used id `0` and the
  server's `roots/list` request also used id `0`. The id→method map MUST be
  keyed by (direction, id).
- **Playwright MCP 1.65** speaks `2025-11-25`, sends
  `notifications/tools/list_changed` right after `tools/list`, and
  `browser_navigate` no longer inlines the snapshot: it writes
  `.playwright-mcp/page-*.yml` (in the server's cwd) and returns a link. Only
  `browser_snapshot` inlines the tree (Wikipedia "Rust" page: 506K chars).
  Snapshot text is `### Page` header + a ```yaml fenced aria tree; refs look
  like `[ref=e13]` / `[ref=f1e2]`, plus `[cursor=pointer]`, `[active]`, `/url:`.

## Playwright handler (M2) — rules and why

Token breakdown of the 40 fixtures (o200k): refs 30%, content 23%, `/url`
16%, generic lines 10%, interactive lines 8%, indentation 6%, cursor 6%.
So refs and wrappers are the levers, not text.

Conservative (default, 31% saved over the 40 committed fixtures):
- collapse unnamed, inert, single-child `generic` wrappers (children promoted);
- drop empty inert nodes and separator-only text nodes (`- text: "|"`),
  except table cells (cell, columnheader, rowheader, gridcell): an empty
  cell keeps its place so the row's other values stay in their columns;
- hide `[ref=…]` on non-interactive **structural** roles (generic, cell, row,
  list(item), paragraph, code, text…); headings, img, landmarks keep refs;
- remove `[cursor=pointer]` where the role implies it (link, button, option…);
- dedupe ≥20 *identical (incl. text)* composite inert siblings, keep 10;
- shorten only blobs > 1200 chars (to 1000). Paragraphs are content;
- hide a `/url` already shown earlier on the page (only URLs actually emitted
  count, so every distinct URL survives; tested on every fixture);
- a link whose only child is its `/url` goes on one line: `- link "x" [ref]: /p`.
  25% of hidden repeated URLs belong to a differently named link (e.g. two
  arXiv authors sharing a search URL): the agent can click it but not see its
  URL without an expand.

Aggressive (opt-in, ~48% saved): flatten all unnamed generics, hide every
inert ref, drop `/url` lines, dedupe runs ≥6 keep 3, shorten > 200 chars.

Hard guarantees (tested on every fixture, generated trees, fixture windows):
interactive refs survive in order (test uses its own classifier), output is
smaller than input, marker present, text outside the yaml block untouched,
block stays well-formed. Unparseable lines are kept verbatim and treated as
interactive. Inputs < 4096 bytes are never touched. Anything odd (odd indent,
skipped level, a lone `\r`) → return None with one warning per process, and
because the handler `claims` any text whose ```yaml fence holds a
`- … [ref=…]` line, the json/text handlers never touch it either: the whole
page is forwarded (fail open means the whole page, never a head-and-tail
cut). CRLF is normalised to LF before parsing (`find_block` itself never
returns a body with a CR, so diffs and outlines only see LF); the headers
`### Snapshot` (1.5x), `- Page Snapshot:` (0.0.3x) and `- Page Snapshot`
(≤ 0.0.29) are all accepted.

Perf (criterion, same laptop, back to back with the pre-URL-rules code,
2026-11-02): 244K-token page 2.4 ms (2.3), synthetic 400K-token page 4.1 ms
(4.3) — the 5 ms requirement, now benched directly — and 488K tokens 5.1 ms
(4.8). The URL set uses a word-at-a-time hasher; SipHash cost about 7%.
Timings drift 10%+ between sessions on this laptop: always compare back to back.

`--budget N` (off by default): over-budget snapshots → aggressive trim →
`playwright::outline` (structure roles, then controls, then the rest in page
order). Budget is N×3 bytes (no tiktoken on the message path), so approximate.
Diffs are computed against the view actually sent (fix in this branch).

`meiose_expand(id, query)` (`src/search.rs`): each hit brings the largest
ancestor whose subtree is ≤ 40 lines (a cell's row, a rustdoc method header
with its version badge), plus ancestors; ≤ 30 hits, ≤ 24 KB. JSON only: when
the enclosing object is bigger, its one-line fields come too (GitHub issues
are 100+ lines; last-item probe 2/6 → 6/6, `examples/json_probe.rs`). Query expands are
recorded with handler "query".

**Not yet verified:** that agents succeed as often with trimmed snapshots —
that is the M6 eval. The structural-ref hiding is the riskiest rule.

## Proxy intercept (M3)

- `src/intercept.rs` is the only place traffic is changed. Client requests
  (`tools/call`, `tools/list`) are tracked by id; only server *responses* are
  looked up, so server-initiated requests reusing an id are never confused.
- `tools/list` first page (no `cursor`) gets `meiose_expand` appended. If the
  server already has a tool by that name, trimming is off for the session.
- Nothing is trimmed until `meiose_expand` was advertised in this process,
  nor when the store failed to open, nor for `resultType` ≠ complete, errors,
  or `--passthrough`.
- The original `result` raw bytes are stored *before* the trimmed response is
  sent; `meiose_expand` splices them back verbatim (tested byte-for-byte).
  Originals live in memory (32 MB cap; 128 MB with `--no-store` or
  MEIOSE_KEEP_HOURS=0) and SQLite (24-hour TTL, decided 2026-11-03; 512 MB
  cap on db + WAL as on disk; `secure_delete`,
  incremental auto-vacuum, WAL truncated by prunes and on close).
  Expand is scoped by `store::server_key` (hash of cwd + full cmd); ids are
  13 chars from `getrandom`. Store queue bounded (1024 jobs / 64 MB): when
  full, outputs go untrimmed (never trim without a stored original).
- Frames > 16 MB (`frame::MAX_FRAME`; debug: MEIOSE_TEST_MAX_FRAME_KB) are
  streamed through in chunks, never parsed/trimmed/stored; the stdout writer
  takes `Out::Stream` so nothing interleaves inside one.
- `log`/`stats`/`doctor` output goes through `report::printable` (control
  chars removed) and ignores a closed stdout.
- Snapshot diff: Playwright refs are stable within one loaded document, NOT
  across navigations (a reload renamed `e3` → `f2e3`; verified with real
  Playwright 1.65), so diffs only help repeat snapshots of the page you are
  on. A repeat snapshot of the *same URL* as the *immediately previous*
  snapshot (< 10 min old) is sent as a ```diff of the view the agent was
  sent, with ancestor context, if < 40% of lines changed (a reload changes
  every ref, so it falls back to full). `--no-diff` disables it. Measured:
  Wikipedia repeat 134K → 79 tokens. A → B → A diffing was tried and
  rejected: refs change after the reload, so it never fires.
- Handler order: playwright → json (arrays > 40 keep 10; arrays ≤ 40 items but
  > 40 KB compact keep whole items up to 16 KB, so short lists of big objects
  never reach the text cut; then a 45,000-char output cap (Claude Code shows
  ≤ 50,000) via `json::fit`: one-line fields first, whole list items, nested
  values summarized; always valid JSON; numbers exact via
  `arbitrary_precision`) → text (> 40 KB: head 16 KB + tail 4 KB).
  A handler that `claims` an output it could not trim ends the chain
  (`Handler::claims`): the output is forwarded whole, and the handler logs
  why, once per process. Before this, a snapshot with one odd line fell
  through to the text handler and lost about 95% of its interactive refs.
- Stats rows are written by the store thread, so SQLite is off the message
  path. Tokens are *estimated* (`tokens::estimate`: other bytes/3.75 +
  indent/8 + 7 per ref; estimated fixture saving 31.8% vs exact 31.1%) so the
  o200k vocabulary (~36 MB per process) never loads; `MEIOSE_EXACT_TOKENS=1`
  counts exactly (the eval harness sets it). Runtime: 2 worker threads. Logs:
  daily, last 7 files kept. Footprint: 14.8 MB / 13 threads per process. Whole intercept path
  for a 500 KB snapshot: 2.0–2.6 ms. `meiose stats` counts expanded tokens
  against savings.
- Playwright 1.65 renamed `browser_click`'s `ref` argument to `target`.

## Install / uninstall / doctor (M4)

- Configs: Claude Code `~/.claude.json` (or `$CLAUDE_CONFIG_DIR/.claude.json`;
  top-level `mcpServers` AND per-project `projects.<path>.mcpServers`),
  Claude Desktop `<config dir>/Claude/claude_desktop_config.json` (+ MSIX
  `%LOCALAPPDATA%/Packages/Claude_*/LocalCache/Roaming/Claude/…`), Cursor
  `~/.cursor/mcp.json`. Project `.mcp.json` / `.cursor/mcp.json` are not touched.
- Wrap = `command: <meiose>`, `args: ["run","--",<old command>,…old args]`.
  Remote (url/http/sse) and already-wrapped (`meiose run|record …`) skipped.
- Files are re-rendered with their detected style (indent, CRLF, trailing
  newline; key order and exact numbers preserved). The real 79 KB
  ~/.claude.json round-trips byte-identically, so only command/args lines
  change. The printed diff shows command/args only — never `env` (secrets).
- Backups + "what we wrote" copies + manifest in `<data>/install/` (`600`).
  Uninstall deletes a file's backups once it is restored/unwrapped; keeps
  them on failure, for missing files and with `--keep-backups`.
  `write_atomic` keeps the target's mode (Unix). The printed diff and
  recording headers pass args through `redact::args`; logs never get args.
  Uninstall: file unchanged since our write → byte-for-byte restore; else
  unwrap only entries still exactly as we wrote them, keep other edits.
  Claude Code rewrites its file constantly, so expect the second path there.
- The plan is computed before the `[y/N]` question and applied after it,
  minutes later if the user is slow; Claude Code may have rewritten the file
  in between (startup counters, `claude mcp add`). `install::apply` re-reads
  the file first and `write_atomic_if` compares once more right before the
  rename: if the bytes differ, `Outcome::FileChanged`, nothing is written,
  and a record made for that attempt is forgotten again (so the manifest and
  `doctor` never name a file meiose did not write). `setup::install` then
  plans again from the current content: same servers → applied after a
  one-line note; different → the new diff is shown and the question asked
  again; nothing left → skipped. Up to 5 replans per file, then it says to
  close the app that is writing it. When no file was updated the closing
  "Backups: …" lines are not printed.
- `install` refuses to embed a throwaway meiose path (npx cache, temp,
  target/debug) unless `--meiose-path` is given.
- `MEIOSE_FAKE_HOME` roots all config paths in one dir (tests, safe trials).

## Eval findings (eval/run_eval.py)

- Claude Code's documented limits: MCP text results over 50,000 characters
  (any token count) or over `MAX_MCP_OUTPUT_TOKENS` (default 25,000) are saved
  to a file and the model gets a note; `_meta["anthropic/maxResultSizeChars"]`
  raises the char limit up to 500,000. So in Claude Code the biggest pages never
  reach the model either way.
- 2026-11-02, Haiku, 19 tasks × 3 runs, reference = `--passthrough`
  (BENCHMARKS.md has the tables): Claude Code: default 57/57, cost −7% [3, 11],
  billed tokens noise; `--budget` did not pay off (more turns). Uncapped loop
  (`eval/agent_loop.py`): `--budget 20000` 54/57, billed −51% [20, 65],
  cost −93% [42, 96]; default within noise.
- URL rules, targeted eval (4 listing tasks; loop 20 runs, Claude Code 8):
  no significant accuracy effect (loop 70/80 reference, 65/80 with rules,
  61/80 without; CC 32/32 all). `books-mystery-third` is off-by-one about half
  the time even with no trimming (11/20): don't read single-task drops on it.
- The eval caught two rules: structural dedupe hiding data rows (now
  identical-text only), and `query` returning a hit's parent only (1/6 known
  answers present → 6/6 after climbing to the largest small ancestor; replay
  34/40 → 39/40).
- Run-to-run noise is large (cache reads ±30–70K per identical run). Use the
  report's bootstrap intervals; never claim a saving whose interval includes 0.

## Rejected ideas (do not rebuild without new evidence)

- Diffing A → B → A across navigations: refs change after a reload
  (`e3` → `f2e3`), so the diff never fires.
- `[ref=e1]` → `[e1]`: saves 2.0%, changes a format agents may rely on.
- Collapsing `listitem` wrappers: about 1%.
- Cleaning query strings or URL prefixes: 1.1% and 1.4% of bytes.
- Optimising indentation: about 29% of bytes but about 6% of tokens.

## Release (M6)

- cargo-dist 0.32 (`dist-workspace.toml`, `.github/workflows/release.yml`):
  5 targets (macOS x64/arm64, Linux x64/arm64 gnu, Windows x64), shell +
  PowerShell installers, Homebrew formula → `yousufmohi/homebrew-tap`
  (secret HOMEBREW_TAP_TOKEN), npm package `meiose` (secret NPM_TOKEN).
  Release = push a `v*` tag. `dist plan` / `dist build --artifacts=local`.
- crates.io: `include` keeps the package to src + docs (75 KB);
  `cargo publish --dry-run` passes. Needs `cargo login`.
- CI (`.github/workflows/ci.yml`): fmt; clippy -D warnings + full tests on
  ubuntu/macos/windows (Python 3.12 for fake_server.py); bench job.
- Verified on Windows, Linux (WSL Ubuntu 24.04 + CI) and macOS (CI, all 118
  tests); first green CI run 2026-11-02. Local Linux runs use a separate
  CARGO_TARGET_DIR.
- `install::program_name` splits on both `/` and `\` (configs travel between
  OSes; `Path::file_stem` on Linux does not split Windows paths).
- `MEIOSE_KEEP_HOURS` (default 24; 0 = originals never written to disk;
  `MEIOSE_KEEP_DAYS` honoured when it is unset). `run --no-store` = memory
  only for one server (`StoreHandle::memory_only`); stats still on disk.

## Layout

```
src/main.rs  src/lib.rs  src/cli.rs  src/proxy.rs  src/frame.rs  src/record.rs
src/paths.rs  src/logging.rs  src/tokens.rs  src/rpc.rs (envelope, raw splicing)
src/intercept.rs  src/diff.rs  src/store.rs (SQLite + memory)  src/report.rs (stats/log text)
src/install.rs (config discovery, wrap/unwrap, backups)  src/setup.rs (install/uninstall UX)  src/doctor.rs
src/handlers/{mod.rs (trait + fail-open trim_text), playwright.rs, json.rs, text.rs}
src/search.rs (meiose_expand query)  src/procgroup.rs (process tree)
src/secure.rs (owner-only files/dirs on Unix)  src/redact.rs (hide secret args)  src/purge.rs
tests/fixtures/playwright/*.txt     40 public-page snapshots (committed)
tests/fixtures/local/               your private fixtures (git-ignored)
tests/snapshots/playwright/*.snap   approved conservative outputs (insta)
tests/common/mod.rs                 independent invariant checker
tests/{passthrough,playwright_fixtures,playwright_props,expand,install,secrets,hostile}.rs
tests/fake_server.py (modes: mirror, exit, burst, hang, mcp; mcp tools incl. hostile, big_text)  tests/fixtures/proxy/
benches/prune.rs  examples/report.rs (per page; --local adds private fixtures)
examples/measure.rs (budget totals, pages over CC limits, full vs query expand)
examples/search_probe.rs (are known answers in query results)
examples/json_probe.rs (JSON outputs saved as .txt: savings + last-item query)
eval/run_eval.py  eval/agent_loop.py (uncapped agent)  eval/tasks.json (19 tasks)
scripts/capture_fixtures.py         drive real Playwright MCP → fixtures
scripts/extract_fixtures.py         your recordings → tests/fixtures/local/
```

## Commands

```bash
cargo build                         # debug build
cargo test                          # all tests (needs python on PATH for fake_server.py)
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo bench --bench prune           # trim time on the largest fixture
cargo run --release --example report [-- --markdown] [-- --dump DIR]  # savings table
cargo insta review                  # approve/reject changed snapshot outputs

python scripts/capture_fixtures.py --meiose target/release/meiose.exe   # recapture
python scripts/extract_fixtures.py  # your recordings → tests/fixtures/local/

meiose stats [--days 7]  ·  meiose log [-n 20] [--show ID]  ·  meiose doctor
meiose install [--dry-run] [--yes] [--only a,b] [--client claude-code,cursor] [--meiose-path P]
meiose uninstall [--yes] [--keep-backups]  ·  meiose purge [--yes]
meiose run [--passthrough | --aggressive] [--no-diff] [--no-store] -- <server cmd>

# try it with Claude Code (installed user-scope on the dev machine as pw-meiose)
cargo install --path .              # close Claude sessions using it first (Windows lock)
claude mcp add --scope user pw-meiose -- meiose record -- npx -y @playwright/mcp@latest
```

## Environment notes

- Dev machine is Windows 11 with Rust (rustup, stable-msvc) and VS 2022 Build
  Tools. In Git Bash, `export PATH="$HOME/.cargo/bin:$PATH"` if `cargo` is missing.
- `npx` is `npx.cmd`; spawning resolves PATHEXT via the `which` crate.
- Env knobs: `MEIOSE_LOG` (log level), `MEIOSE_DATA_DIR` (data/log/recording
  root), `MEIOSE_SHUTDOWN_GRACE_MS` (default 3000), `MEIOSE_TEST_PYTHON`.
- `main` must call `std::process::exit` while the tokio runtime is alive:
  dropping it blocks on the pending stdin read (regression-tested by
  `server_exit_code_is_propagated` / `server_initiated_shutdown_*`).

## Hardening (M5)

- Process tree: Windows Job Object (KILL_ON_JOB_CLOSE) — the server and all
  its descendants die with meiose, even if meiose is TerminateProcess'd.
  Unix: own process group, SIGKILL to the group on forced kill.
  (`src/procgroup.rs`). Known Unix gap: if meiose is SIGKILLed, the server
  only gets stdin EOF (well-behaved servers exit on it).
- Ctrl-C / SIGTERM / Ctrl-Break: stop the server tree and exit.
- Fail open: `MEIOSE_TEST_PANIC=client|server` (debug builds only) injects a
  panic into the intercept; tests prove traffic still flows unchanged.
  Unusable store (MEIOSE_DATA_DIR not a dir) → nothing injected or trimmed.
- tests/protocol.rs: out-of-order ids under load, notifications mid-stream,
  5 MB snapshot (trim + byte-exact expand), malformed server lines,
  malformed/batch/CRLF client lines, partial writes, slow server, server
  crash mid-request, agent that stops reading, stuck server's grandchild,
  hard-killed meiose (Windows).
- Windows testing gotcha: child processes inherit every inheritable handle;
  an orphaned test server can hold cargo's output pipe and hang the run.
  The grandchild in fake_server uses null stdio for this reason.

## Release notes for next time

- npm tokens must be granular, "Read and write (publish and stage)", with
  **Bypass 2FA** enabled, or CI publish fails with E403. After publish, npm
  shows a `0.0.0-stage` placeholder for ~1 minute before the real version.
- Rerun only failed release jobs: `gh run rerun <run-id> --failed`.
- Release actions are pinned by SHA in `dist-workspace.toml`
  (`[dist.github-action-commits]`) and artifacts are attested
  (`github-attestations = true`). Never hand-edit release.yml: change the
  config, `dist generate`, `dist plan` (a hand edit fails `dist plan`).
  When bumping `cargo-dist-version`, re-check which actions it uses and pin
  them all (`grep uses: .github/workflows/release.yml`).
- ci.yml: `permissions: contents: read`, SHA-pinned actions, `cargo audit`
  job. Dependabot (`.github/dependabot.yml`) covers cargo + ci.yml actions.
- npm still publishes with the NPM_TOKEN secret: trusted publishing (OIDC)
  needs hand edits to release.yml that dist 0.32 cannot generate.
- crates.io publishing is manual (`cargo publish`); bump the version in
  Cargo.toml, update CHANGELOG.md, push, then tag `vX.Y.Z`.
