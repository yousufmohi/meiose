# Benchmarks

Numbers below come from runs on 2026-11-01 and 2026-11-02 with Playwright MCP
1.65. Token counts of tool outputs use OpenAI's `o200k_base` encoding as a
stand-in, because Claude's tokenizer is not public; ratios between before and
after are what matter. Billed tokens and cost in the evals are what Claude
reported.

## Trimming on real pages

Snapshots of 40 public pages (news, docs, shops, forms, Wikipedia) are stored in
`tests/fixtures/playwright/` (`cargo run --release --example report`):

| page | tokens | default | `--aggressive` |
|---|---:|---:|---:|
| Rust `Vec` docs | 244,156 | 166,563 (−32%) | 133,879 (−45%) |
| MDN `<input>` reference | 166,768 | 112,891 (−32%) | 93,199 (−44%) |
| Wikipedia "Rust (programming language)" | 133,768 | 105,700 (−21%) | 81,203 (−39%) |
| The Guardian front page | 39,101 | 30,194 (−23%) | 20,699 (−47%) |
| Hacker News front page | 13,713 | 8,262 (−40%) | 5,696 (−58%) |
| books.toscrape.com | 8,660 | 5,697 (−34%) | 4,218 (−51%) |
| all 40 pages | 943,239 | 650,329 (−31%) | 488,437 (−48%) |

Repeat snapshot of an unchanged page, through real Playwright (Wikipedia
"Rust"): 133,862 → 79 tokens. The second snapshot is sent as a note that
nothing changed.

### Getting one fact back: full expand vs `query`

`meiose_expand(id)` returns the whole original; `meiose_expand(id, query="…")`
returns the matching lines with their context
(`cargo run --release --example search_probe`):

| page | query | full expand | query result | answer present |
|---|---|---:|---:|---|
| Rust `Vec` docs | `extract_if` | 244,156 | 1,545 | yes |
| Rust `Vec` docs | `dedup_by_key` | 244,156 | 1,399 | yes |
| Rust `Vec` docs | `retain_mut` | 244,156 | 1,660 | yes |
| Wikipedia UN population list | `Mongolia` | 70,563 | 306 | yes |
| Wikipedia "Rust" | `Graydon` | 133,768 | 2,282 | yes |

### `--budget`

Totals over the 40 pages through the real interceptor
(`cargo run --release --example measure`), and how many pages stay over Claude
Code's limits for MCP results (25,000 tokens, and 50,000 characters for text):

| mode | tokens | pages over 25K tokens | pages over 50K characters |
|---|---:|---:|---:|
| default | 650,431 (−31%) | 5 | 8 |
| `--budget 20000` | 237,049 (−75%) | 0 | 6 |
| `--budget 12000` | 180,523 (−81%) | 0 | 0 |

The budget is converted at 3 bytes per token, so it is approximate: the largest
page under `--budget 12000` came to 12,343 tokens.

### Trim time

Criterion, `cargo bench --bench prune`, Windows laptop:

| input | time |
|---|---:|
| Rust `Vec` docs, 244K tokens | 2.4 ms |
| the `Vec` page doubled and cut at 400K tokens (synthetic) | 4.1 ms |
| the same page doubled, 488K tokens (synthetic) | 5.1 ms |

Measured back to back with the code before the URL rules (2.3, 4.3 and
4.8 ms), because timings on this laptop drift by 10% or more between sessions.

## JSON tools: GitHub's MCP server

Seven real outputs from `@modelcontextprotocol/server-github` (public repos,
no token), saved and measured offline with `examples/json_probe.rs`.
o200k tokens:

| output | tokens | default | aggressive | handler |
|---|---:|---:|---:|---|
| `list_issues` microsoft/playwright, 100 open | 78,757 | 12,876 (−84%) | 3,214 (−96%) | json |
| `search_issues` playwright "screenshot" | 100,677 | 18,211 (−82%) | 3,901 (−96%) | json |
| `list_commits` rust-lang/rust, 50 | 94,554 | 21,839 (−77%) | 5,370 (−94%) | json |
| `search_repositories` "model context protocol" | 17,954 | 3,533 (−80%) | 1,093 (−94%) | json |
| `list_pull_requests` playwright (16 open) | 25,013 | 5,859 (−77%) | 4,579 (−82%) | text |
| `get_file_contents` (a directory) | 4,284 | untouched | 656 (−85%) | — |
| `get_issue` | 1,202 | untouched | untouched | — |

The JSON handler keeps the first 10 items of a long list, so the rest are
reachable only through `meiose_expand`. The probe asks `query` for the **last**
item by its title and checks the result contains that item's URL. Before this
change it failed on 4 of 6 lists: a GitHub issue is 100+ lines of JSON, so the
match came back with its ancestors only. A JSON match now also brings the
one-line fields of its object (number, state, URLs, dates): 6 of 6 found, at
185–2,068 tokens per query.

The pull-request list has only 16 items, under the JSON handler's 40-item
threshold, so it falls through to the text handler, which keeps the first 16 KB
and last 4 KB: the agent sees the start and end of the array, cut mid-item.

## Footprint on the host

meiose's own process during a session with five 924K-character snapshots
(Windows, 12 cores):

| | before | after |
|---|---:|---:|
| private memory | 52.8 MB | 14.8 MB |
| threads | 23 | 13 |
| CPU time | 0.36 s | 0.09 s |

Most of the difference is the token counter: stats now use an estimate instead
of loading the o200k vocabulary (about 36 MB per process, and every wrapped
MCP server runs its own meiose). On the 40 fixtures the estimated saving is
31.8% against an exact 31.1%. `MEIOSE_EXACT_TOKENS=1` restores exact counts;
the eval harness sets it, so every eval number above is exact. The rest is two
runtime threads instead of one per core and a 32 MB in-memory cache of
originals (they are also on disk). The browser that Playwright MCP starts is
far larger than meiose and is stopped with it.

## Claude Code's limits

Claude Code saves an MCP tool's text result to a file, and shows the model a
short note instead, when it is longer than 50,000 characters, or longer than
`MAX_MCP_OUTPUT_TOKENS` (default 25,000 tokens). A tool can raise the character
limit up to 500,000 with `_meta["anthropic/maxResultSizeChars"]`
([Claude Code docs](https://code.claude.com/docs/en/mcp#mcp-output-limits-and-warnings)).
So in Claude Code, the largest pages never reach the model either way, and
meiose saves tokens only on outputs below the limit and on repeat snapshots.

## End-to-end evals

`eval/run_eval.py` runs browser tasks whose answers only the page contains, and
checks the answers automatically. 19 tasks, 3 runs per task per mode, Claude
Haiku. The reference mode is `meiose --passthrough`, which forwards traffic
unchanged. Savings are summed over per-task means; brackets are a bootstrap 95%
interval over tasks, and an interval that includes zero is marked as noise.

### Claude Code

| mode | correct | billed tokens / run | saving | cost / run | saving | runs with an expand |
|---|---:|---:|---|---:|---|---:|
| reference | 57/57 | 195,323 ± 72,763 | — | $0.0073 ± 0.0020 | — | 0 |
| default | 57/57 | 196,884 ± 65,370 | −1% (noise) | $0.0068 ± 0.0014 | 7% [3, 11] | 0 |
| `--budget 20000` | 56/57 | 226,706 ± 146,099 | −16% (noise) | $0.0076 ± 0.0034 | −4% (noise) | 8 |
| `--budget 12000` | 57/57 | 217,379 ± 98,992 | −11% (noise) | $0.0076 ± 0.0025 | −3% (noise) | 10 |

In Claude Code, only the default mode's 7% cost saving is outside the noise.
`--budget` adds turns (searches with `query`) and does not pay off there.

### An agent with no output limit

`eval/agent_loop.py` is a minimal agent: the harness talks to the MCP server and
pastes every result into the conversation in full. Claude runs as one
stream-JSON session with no built-in tools.

| mode | correct | billed tokens / run | saving | cost / run | saving | runs with an expand |
|---|---:|---:|---|---:|---|---:|
| reference | 57/57 | 128,068 ± 159,071 | — | $0.0466 ± 0.1082 | — | 0 |
| default | 54/57 | 129,214 ± 177,727 | −1% (noise) | $0.0362 ± 0.0806 | 22% (noise) | 0 |
| `--budget 20000` | 54/57 | 63,378 ± 39,023 | 51% [20, 65] | $0.0034 ± 0.0023 | 93% [42, 96] | 12 |
| `--budget 12000` | 53/57 | 72,846 ± 54,006 | 43% [16, 56] | $0.0033 ± 0.0024 | 93% [43, 96] | 15 |

The two `--budget` rows were re-run after a search fix (below), against the
reference and default rows from the first run with the same harness, tasks and
model, two hours earlier. Every failure in the `--budget` rows is either a
timeout or an off-by-one answer on a listing page that the reference also gets
wrong (next section); none is on the large-page tasks.

### Listing pages and the URL rules

The default mode hides a link's URL when the same URL already appeared on the
page, and puts a link whose only child is its URL on one line. Four
listing-page tasks, run with and without those two rules:

| client | reference | default | default without the URL rules |
|---|---:|---:|---:|
| agent with no output limit, 20 runs each | 70/80 | 65/80 | 61/80 |
| Claude Code, 8 runs each | 32/32 | 32/32 | 32/32 |

None of the differences is significant (Fisher's exact test: default vs
reference p = 0.38, with vs without the rules p = 0.56). One task,
`books-mystery-third` ("the third book in the Mystery category"), is answered
off by one about half the time even with no trimming (11/20).

### Rules the evals changed

- Search context. `query` used to return a match with its parent only, which
  missed the answer on real pages: a country's numbers sit in sibling cells of
  its link, and rustdoc's version badge sits beside the method heading. On the
  fixtures, 1 of 6 known answers was present before and 6 of 6 after. Replaying
  the four large-page tasks with `--budget 20000`, 10 runs each: 34/40 correct
  before and 39/40 after (the one miss was a timeout), with about half the
  tokens on the `Vec` docs tasks.
- Dedupe. The default mode used to collapse long runs of structurally similar
  elements. On a 250-country list that hid the row the agent needed and forced
  an expand (+43% tokens on that task). It now collapses only elements that are
  identical including their text.

## Reproduce

```sh
cargo build --release
python eval/run_eval.py --quick                    # 5 tasks, 1 run per mode
python eval/run_eval.py --modes baseline,meiose,budget20000 --runs 3
python eval/run_eval.py --client loop --modes baseline,meiose,budget20000 --runs 3
cargo run --release --example report               # per-page table
cargo run --release --example measure              # budget and query totals
cargo run --release --example search_probe         # are answers in query results
cargo bench --bench prune
```

See [TESTING.md](TESTING.md) for what each run measures and how to read it.
