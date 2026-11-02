# Testing meiose yourself

Five checks, from "does it run" to "how much does it actually save". Run
them from the repo root. In Git Bash, if `cargo` is missing:
`export PATH="$HOME/.cargo/bin:$PATH"`.

## 1. The test suite (1 minute)

```bash
cargo test
```

About 115 tests: framing, the Playwright trimmer on 40 real pages (refs
preserved, never larger, expand marker), property tests, end-to-end proxy
tests against a scripted MCP server (byte-for-byte expand, diffs, id
collisions, passthrough), install/uninstall on fake home directories, and the
protocol suite (out-of-order ids, 5 MB messages, malformed JSON, crashes, slow
servers, process-tree cleanup, injected panics). Needs Python 3 on PATH.

## 2. Is my setup healthy? (10 seconds)

```bash
meiose doctor
```

Every line is ✓, `!` (works, with a note) or ✗ (broken, with a `→` fix).

## 3. Use it for real and watch the numbers (5 minutes)

Install the current build (close Claude sessions that use meiose first; Windows
locks the running binary):

```bash
cargo install --path . --locked
meiose install --dry-run      # shows exactly what would change
meiose install                # asks before writing; backs up every file
```

Or wrap just Playwright by hand:

```bash
claude mcp add --scope user pw-meiose -- meiose run -- npx -y @playwright/mcp@latest
```

Then use your agent normally: browse a few pages, take snapshots, click
around. Afterwards:

```bash
meiose stats                  # "Last 7 days: 2.1M tokens → 340K (84% saved)"
meiose log                    # each call: tokens before → after, handler, expand id
meiose log --show <id>        # what the agent saw vs. the original
```

`stats` counts tokens the agent pulled back with `meiose_expand` against the
savings, so it cannot overstate them.

## 4. Did it hurt the agent? The A/B eval (15–60 minutes, costs a few dollars)

The only honest answer to "how much does it save" is to run the same tasks
with and without meiose and compare what Claude actually billed, and whether
the answers were still right:

```bash
cargo build --release
python eval/run_eval.py --quick              # 5 tasks × 2 modes × 1 run
python eval/run_eval.py                      # 13 tasks × 2 modes × 2 runs
python eval/run_eval.py --model sonnet --modes baseline,meiose,aggressive
```

Each run is `claude -p` with Playwright MCP, identical prompt, model and
allowed tools. The report (`eval/results/<time>/report.md`) has, per mode:

- **success** — answers checked against verified answer keys;
- **total tokens / cost** — from Claude's own usage report (input + cache
  writes + cache reads + output), so this is real billed context;
- **tool-output tokens** — what the browser tools returned, before and after
  meiose (incl. anything expanded);
- **failures where baseline succeeded**, with the meiose handler that touched
  each call, so a harmful rule can be found.

The baseline runs Playwright through `meiose --passthrough` (byte-identical
traffic, proven by the tests) so it can count tool tokens too; add
`--pure-baseline` to use plain `npx @playwright/mcp` with no meiose at all.

### What the first eval showed (2026-11-01, Haiku, 5 tasks × 1 run)

All 10 runs correct, but **no net saving in Claude Code: +3% tokens and cost**
with meiose, although tool outputs shrank 6%. Why:

- **Claude Code already offloads huge MCP results.** Anything over its output
  cap (~25K tokens) never reaches the model: it is saved to a file and the model
  gets a ~1.5K-character note instead. So trimming a 134K-token Wikipedia
  snapshot to 117K changes nothing there. meiose's token savings in Claude Code
  come only from outputs *under* the cap (roughly 2K–25K tokens), which enter
  the context and are re-read on every later turn, and from snapshot diffs.
- **On short tasks the fixed cost dominates.** Claude Code's own system prompt
  and tools are ~15–20K tokens per turn; a 1–2K-token saving on a small page is
  below the run-to-run noise (same task, same turns, cache reads varied by 30K).
- **Some tasks are guessable** (`wiki-rust`, `large-table-cell`): both modes
  answered correctly without the agent seeing the page. They need replacing
  with tasks whose answers only the page contains.

In clients without such a cap (Claude Desktop, Cursor, your own agents) the
big-page savings apply directly; that is not measured by this harness yet.

## 5. Undo

```bash
meiose uninstall              # restores files byte-for-byte if untouched since,
                              # otherwise unwraps only what meiose wrapped
claude mcp remove pw-meiose -s user   # if you added it by hand
```
