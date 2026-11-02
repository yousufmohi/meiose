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
python eval/run_eval.py                      # 15 tasks × 2 modes × 2 runs
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

### Results so far (2026-11-01, Claude Haiku, Playwright MCP 1.65)

Full eval, 15 tasks × 2 runs per mode:

| | baseline | meiose |
|---|---:|---:|
| correct answers | 30/30 | **30/30** |
| billed tokens per run | 199,103 | 187,312 (**6% less**) |
| cost per run | $0.0077 | $0.0070 (**9% less**) |
| tool-output tokens | 293K | 219K (**23% less**) |

Cost falls more than tokens because the trimmed output is written to the
prompt cache once (expensive) and re-read cheaply afterwards.

How to read this:

- **Claude Code already offloads huge MCP results.** Anything over its output
  cap (~25K tokens) never reaches the model: it is saved to a file and the
  model gets a short note. So in Claude Code meiose only saves on outputs under
  the cap, plus snapshot diffs. Clients without such a cap (Claude Desktop,
  Cursor, your own agents) get the full trimming effect on big pages; this
  harness does not measure them yet.
- **Claude Code's own context dominates short tasks** (~15–20K tokens of
  system prompt and tools per turn), and cache reads vary ±30–70K between
  identical runs. Per-task differences in the table are mostly noise; only
  the totals over 60 runs mean much.
- **The eval already caught one harmful rule.** Structural dedupe hid unique
  rows of a 250-country list (the agent had to expand, +43% tokens). Since
  then, conservative mode only collapses siblings that are identical
  including their text; that task is now even with baseline.

## 5. Undo

```bash
meiose uninstall              # restores files byte-for-byte if untouched since,
                              # otherwise unwraps only what meiose wrapped
claude mcp remove pw-meiose -s user   # if you added it by hand
```
