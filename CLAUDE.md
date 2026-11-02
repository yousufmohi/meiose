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
| M2 | Playwright handler + snapshot/property tests + benchmark | **Done (2026-11-01), awaiting user review** |
| M3 | `meiose_expand` + store + `stats` + `log` + json/text handlers | not started |
| M4 | `install` / `uninstall` / `doctor` + install tests | not started |
| M5 | Protocol test suite + fail-open hardening | not started |
| M6 | Eval harness + README + cargo-dist + npm wrapper | not started |

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
   - Never drop an interactive element (`[ref=...]` that is clickable,
     fillable or selectable).
   - Every trimmed output has a marker:
     `[312 nodes trimmed — call meiose_expand("abc123")]`.
   - `meiose_expand` returns the original output byte-for-byte.
4. **Fail open.** If trimming errors or panics, forward the original and log it.
   Never hang, deadlock or corrupt the stream.
5. **Overhead.** Trimming a 400K-token snapshot takes < 5 ms (benchmarked).
6. **Zero config.** Conservative defaults; aggressive mode is opt-in.
7. Every error message says what went wrong **and** what to do next.
8. `install` prints a diff per config change and asks to confirm (`--yes` skips).
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

Conservative (default, ~25% saved over fixtures):
- collapse unnamed, inert, single-child `generic` wrappers (children promoted);
- drop empty inert nodes and separator-only text nodes (`- text: "|"`);
- hide `[ref=…]` on non-interactive **structural** roles (generic, cell, row,
  list(item), paragraph, code, text…); headings, img, landmarks keep refs;
- remove `[cursor=pointer]` where the role implies it (link, button, option…);
- dedupe ≥20 structurally identical *composite* inert siblings, keep 10;
- shorten only blobs > 1200 chars (to 1000). Paragraphs are content.

Aggressive (opt-in, ~48% saved): flatten all unnamed generics, hide every
inert ref, drop `/url` lines, dedupe runs ≥6 keep 3, shorten > 200 chars.

Hard guarantees (tested on every fixture, generated trees, fixture windows):
interactive refs survive in order (test uses its own classifier), output is
smaller than input, marker present, text outside the yaml block untouched,
block stays well-formed. Unparseable lines are kept verbatim and treated as
interactive. Inputs < 4096 bytes are never touched. Anything odd (odd indent,
skipped level, `\r`) → return None (fail open).

Perf: 244K-token page 1.9 ms, synthetic 488K-token page 3.9 ms (criterion).

**Not yet verified:** that agents succeed as often with trimmed snapshots —
that is the M6 eval. The structural-ref hiding is the riskiest rule.

## Layout

```
src/main.rs  src/lib.rs  src/cli.rs  src/proxy.rs  src/frame.rs  src/record.rs
src/paths.rs  src/logging.rs  src/tokens.rs
src/handlers/{mod.rs (trait + fail-open trim_text), playwright.rs}
tests/fixtures/playwright/*.txt     40 public-page snapshots (committed)
tests/fixtures/local/               your private fixtures (git-ignored)
tests/snapshots/playwright/*.snap   approved conservative outputs (insta)
tests/common/mod.rs                 independent invariant checker
tests/{passthrough,playwright_fixtures,playwright_props}.rs  tests/fake_server.py
benches/prune.rs  examples/report.rs
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

## Known gaps (tracked for M5)

- Windows: killing `npx.cmd` kills cmd.exe but may orphan node; needs a Job
  Object. Graceful path (close stdin) works and is what clients use.
- No Ctrl-C/SIGTERM handling yet; we rely on `kill_on_drop` and stdin EOF.
