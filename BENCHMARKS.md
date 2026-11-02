# Benchmarks

All numbers below come from runs on 2026-11-01 and 2026-11-02 with meiose
0.1.0 and Playwright MCP 1.65. Token counts use OpenAI's `o200k_base` encoding
as a stand-in, because Claude's tokenizer is not public; ratios between
before and after are what matter.

## Trimming on real pages

Snapshots of 40 public pages (news, docs, shops, forms, Wikipedia) are stored in
`tests/fixtures/playwright/`. Selected pages:

| page | tokens | default | `--aggressive` |
|---|---:|---:|---:|
| Rust `Vec` docs | 244,156 | 193,388 (−21%) | 133,867 (−45%) |
| MDN `<input>` reference | 166,768 | 119,646 (−28%) | 93,187 (−44%) |
| Wikipedia "Rust (programming language)" | 133,768 | 116,596 (−13%) | 81,191 (−39%) |
| The Guardian front page | 39,101 | 31,601 (−19%) | 20,687 (−47%) |
| Hacker News front page | 13,713 | 9,591 (−30%) | 5,684 (−59%) |
| books.toscrape.com | 8,660 | 6,390 (−26%) | 4,206 (−51%) |
| all 40 pages | 955,708 | 732,642 (−23%) | 493,351 (−48%) |

Repeat snapshot of an unchanged page, through real Playwright (Wikipedia
"Rust"): 133,862 → 79 tokens. The second snapshot is sent as a note that
nothing changed.

Trim time (criterion, `cargo bench --bench prune`, Windows laptop):

| input | time |
|---|---:|
| Rust `Vec` docs, 244K tokens | 2.3 ms |
| the same page doubled, 488K tokens (synthetic) | 4.7 ms |

The whole proxy path for a 500 KB snapshot (parse, trim, re-encode, hand off to
the store) took 2.0–2.6 ms in a live session.

## End to end: Claude Code with and without meiose

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
the prompt cache once, which is the expensive part, and re-read cheaply after.

### Why the end-to-end saving is smaller

Claude Code does not pass very large MCP results to the model. In testing, a
499,003-character snapshot was saved to a file and the model got a short note
instead. The exact threshold was not measured; from the note it is likely
around 25K tokens. So in Claude Code, meiose saves tokens only on outputs below
that limit and on repeated snapshots of a page. Claude Code's own system prompt
and tools also cost about 15–20K tokens per turn, which dominates short tasks.

Agents without such a limit (Claude Desktop, Cursor, your own) should see the
page-level savings above. That has not been measured end to end yet.

Run-to-run noise is large: cache reads for the same task with the same number
of turns varied by 30–70K tokens between identical runs. Only totals over many
runs mean much.

### A rule the eval changed

An earlier version also collapsed long runs of structurally similar elements,
such as the rows of a 250-country list. The eval showed this hid the row the
agent needed and forced an expand (+43% tokens on that task). The default mode
now only collapses elements that are identical including their text.

## Reproduce

```sh
cargo build --release
python eval/run_eval.py --quick          # 5 tasks × 2 modes × 1 run
python eval/run_eval.py                  # 15 tasks × 2 modes × 2 runs
cargo run --release --example report     # per-page table for the 40 fixtures
cargo bench --bench prune
```

See [TESTING.md](TESTING.md) for what each run measures and how to read it.
