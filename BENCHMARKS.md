# Benchmarks

## End-to-end: Claude Code with and without meiose

Claude Code headless (`claude -p`, Claude Haiku) ran 15 browser tasks through
Playwright MCP, 2 runs each, with and without meiose. Every task has an answer
that only the page contains, and answers are checked automatically.

| | without meiose | with meiose |
|---|---:|---:|
| tasks answered correctly | 30/30 | 30/30 |
| billed tokens per task | 199,103 | 187,312 (−6%) |
| cost per task | $0.0077 | $0.0070 (−9%) |
| tool-output tokens | 293K | 219K (−23%) |

Billed tokens are input + cache writes + cache reads + output, as reported by
Claude Code. Cost falls more than tokens because a trimmed output is written to
the prompt cache once (expensive) and re-read cheaply afterwards.

Other measurements from real Playwright sessions:

| | |
|---|---:|
| repeat snapshot of an unchanged page (Wikipedia "Rust") | 134K → 81 tokens |
| proxy overhead for a 500 KB snapshot (parse, trim, re-encode) | 2.0–2.6 ms |

## Why the end-to-end saving is smaller than the trimming saving

Claude Code keeps any MCP tool result above roughly 25K tokens away from the
model: it saves the result to a file and gives the model a short note instead.
So in Claude Code, meiose only saves tokens on outputs below that cap (and on
repeated snapshots of the same page). Claude Code's own system prompt and tools
also cost about 15–20K tokens per turn, which dominates short tasks.

Agents without that cap (Claude Desktop, Cursor, your own) get the full
trimming effect on large pages. That is not measured end to end yet.

Run-to-run noise is large: cache reads for the same task with the same number
of turns varied by 30–70K tokens between identical runs. Only totals over many
runs are meaningful.

## Trimming alone: 40 real pages

Snapshots of 40 public pages (news, docs, shops, forms, Wikipedia), captured
from Playwright MCP 1.65 and stored in `tests/fixtures/playwright/`:

| mode | tokens saved (o200k count) |
|---|---:|
| default | 23% |
| `--aggressive` | 48% |

Trimming time (criterion, `cargo bench --bench prune`):

| input | time |
|---|---:|
| largest real page (Rust `Vec` docs, ~244K tokens) | 1.9 ms |
| synthetic 488K-token page | 3.9 ms |

## Reproduce

```bash
cargo build --release
python eval/run_eval.py --quick          # 5 tasks × 2 modes × 1 run
python eval/run_eval.py                  # 15 tasks × 2 modes × 2 runs
cargo run --release --example report     # per-page table for the 40 fixtures
cargo bench --bench prune
```

See [TESTING.md](../TESTING.md) for what each run measures and how to read it.
