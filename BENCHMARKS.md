# Benchmarks

Numbers below come from a run on 2026-11-06 at commit `a561c38`, with
`@playwright/mcp@latest` (0.0.84) and Claude Code 2.1.319, except where a section
says otherwise. The `--budget 12000` eval rows, the listing-page tasks, the
footprint and the rule-change history are from 2026-11-01 and 2026-11-02 and were
not re-run. Token counts of tool outputs use OpenAI's `o200k_base` encoding as a
stand-in, because Claude's tokenizer is not public; ratios between before and
after are what matter. Billed tokens and cost in the evals are what Claude
reported.

## Trimming on real pages

Snapshots of 40 public pages (news, docs, shops, forms, Wikipedia) are stored in
`tests/fixtures/playwright/` (`cargo run --release --example report`):

| page | tokens | default | `--aggressive` |
|---|---:|---:|---:|
| Rust `Vec` docs | 244,156 | 166,563 (-32%) | 133,879 (-45%) |
| MDN `<input>` reference | 166,768 | 112,899 (-32%) | 93,207 (-44%) |
| Wikipedia "Rust (programming language)" | 133,768 | 105,700 (-21%) | 81,203 (-39%) |
| The Guardian front page | 39,101 | 30,194 (-23%) | 20,699 (-47%) |
| Hacker News front page | 13,713 | 8,390 (-39%) | 5,824 (-58%) |
| books.toscrape.com | 8,660 | 5,697 (-34%) | 4,218 (-51%) |
| all 40 pages | 943,239 | 650,465 (-31%) | 488,573 (-48%) |

Repeat snapshot of an unchanged page, through real Playwright (Wikipedia
"Rust"): 135,571 → 84 tokens. The second snapshot is sent as a note that
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
| default | 650,656 (-31%) | 5 | 8 |
| `--budget 20000` | 237,278 (-75%) | 0 | 6 |
| `--budget 12000` | 180,742 (-81%) | 0 | 0 |

The budget is converted at 3 bytes per token, so it is approximate: the largest
page under `--budget 12000` came to 12,341 tokens.

### Trim time

Criterion, `cargo bench --bench prune`, Windows laptop:

| input | time |
|---|---:|
| Rust `Vec` docs, 244K tokens | 2.2 ms |
| the `Vec` page doubled and cut at 400K tokens (synthetic) | 3.6 ms |
| the same page doubled, 488K tokens (synthetic) | 4.5 ms |

Timings on this laptop drift by 10% or more between sessions.

## JSON tools: GitHub's MCP server

Seven real outputs from `@modelcontextprotocol/server-github` (public repos,
no token), saved and measured offline with `examples/json_probe.rs`. o200k
tokens. These are live data, and the saved outputs from the first run were not
kept, so on 2026-11-06 they were fetched again with the same tools and the
parameters may differ. Compare them with earlier numbers only roughly.

| output | tokens | default | aggressive | handler |
|---|---:|---:|---:|---|
| `list_issues` microsoft/playwright, 57 returned | 89,946 | 11,864 (-87%) | 3,061 (-97%) | json |
| `search_issues` playwright "screenshot" | 189,751 | 11,468 (-94%) | 4,024 (-98%) | json |
| `list_commits` rust-lang/rust, 50 | 94,567 | 13,836 (-85%) | 4,551 (-95%) | json |
| `search_repositories` "model context protocol", 30 | 10,593 | untouched | 1,094 (-90%) | n/a |
| `list_pull_requests` playwright (20 open) | 30,791 | 4,554 (-85%) | 4,474 (-85%) | json |
| `get_file_contents` (a directory) | 3,296 | untouched | 545 (-83%) | n/a |
| `get_issue` | 1,202 | untouched | untouched | n/a |

The JSON handler keeps the first 10 items of a long list, so the rest are
reachable only through `meiose_expand`. The probe asks `query` for the **last**
item by its title and checks the result contains that item's URL. Before this
change it failed on 4 of 6 lists: a GitHub issue is 100+ lines of JSON, so the
match came back with its ancestors only. A JSON match now also brings the
one-line fields of its object (number, state, URLs, dates): 6 of 6 found, at
342 to 1,792 tokens per query.

On 2026-11-02 the pull-request list had only 16 items, under the JSON handler's 40-item
threshold, but they are big (nested `head`/`base` repository objects are 56% of
the bytes). It used to fall through to the text handler, which keeps the first
16 KB and last 4 KB and so cut the JSON mid-item (-77%). A list too short for
the 40-item rule but over 40 KB of JSON now keeps whole items up to 16 KB
(here 3 of 16), so the output stays valid JSON (-80%). Lists of more than 40
items keep their 10 samples as before.

Finally, JSON output is capped at 45,000 characters, under the 50,000 at which
Claude Code saves a tool result to a file instead of showing it. Before the
cap, the 10 sampled issues of the "screenshot" search came to 70K characters
and the 10 commits to 81K, so in Claude Code the agent would have seen
neither. Over the cap, an object keeps its one-line fields (ids, states, URLs)
first, then nested values in order while they fit; the rest become one-line
summaries such as `{…900 keys: a, b, …}`, and lists keep whole items in order.
The output stays valid JSON, and the marker says what was summarized. Real
GitHub outputs trim in 0.3–1.5 ms.

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
| reference | 57/57 | 197,008 ± 64,391 | n/a | $0.0075 ± 0.0020 | n/a | 0 |
| default | 57/57 | 198,100 ± 66,573 | -1% (noise) | $0.0069 ± 0.0014 | 7% [3, 11] | 0 |
| `--budget 20000` | 57/57 | 213,573 ± 76,202 | -8% (noise) | $0.0075 ± 0.0020 | 1% (noise) | 7 |
| `--budget 12000` (2026-11-02) | 57/57 | 217,379 ± 98,992 | -11% (noise) | $0.0076 ± 0.0025 | -3% (noise) | 10 |

In Claude Code, only the default mode's 7% cost saving is outside the noise.
`--budget` adds turns (searches with `query`) and does not pay off there.

### An agent with no output limit

`eval/agent_loop.py` is a minimal agent: the harness talks to the MCP server and
pastes every result into the conversation in full. Claude runs as one
stream-JSON session with no built-in tools.

| mode | correct | billed tokens / run | saving | cost / run | saving | runs with an expand |
|---|---:|---:|---|---:|---|---:|
| reference | 55/57 | 123,671 ± 142,433 | n/a | $0.0463 ± 0.1073 | n/a | 0 |
| default | 54/57 | 103,029 ± 142,165 | 17% (noise) | $0.0285 ± 0.0722 | 38% (noise) | 0 |
| `--budget 20000` | 54/57 | 61,357 ± 32,710 | 50% [22, 66] | $0.0032 ± 0.0019 | 93% [44, 96] | 12 |
| `--budget 12000` (2026-11-02) | 53/57 | 72,846 ± 54,006 | 43% [16, 56] | $0.0033 ± 0.0024 | 93% [43, 96] | 15 |
| default after this change (`--budget 20000`, no `filename` option) | 57/57 | 64,407 ± 36,242 | 48% | $0.0035 ± 0.0022 | 92% [38, 96] | 12 |

The `--budget 12000` row was measured against the 2026-11-02 reference
($0.0466), not the one above. Of the 8 wrong runs on 2026-11-06, 5 were timeouts
(1 reference, 2 default, 2 `--budget 20000`) and 3 were wrong answers (1 in each
mode, on `books-mystery-third` or `quotes-page3`). The only failure on a
large-page task was one default-mode timeout on `vec-extract-if`. Runs used
`--jobs 4`.

The default row was run the same day as the reference, in a separate run. In
that table the "default" row is the previous default, now `--budget 0`.

### Why large pages cost so much on Claude Haiku

`claude -p` with one large prompt, as reported by Claude Code (2.1.319):

| model | cache-write tokens | cost per million |
|---|---:|---:|
| `claude-haiku-5-5` | 22,570 to 95,773 | $0.20 |
| `claude-haiku-5-5` | 103,715 to 201,685 | $1.00 |
| `claude-sonnet-5-5` | 32,623 and 115,381 | $4.00 |

So on Haiku the price per token rose 5x between 95,773 and 103,715 tokens, and
much of the 92% comes from staying under that line. Tokens fell 48%; on a model
without the jump, expect a saving closer to that. The full eval has not been run
on Sonnet. Two large-page tasks with the test agent, 2 runs each, show where the
budget meets the jump:

| budget | cost per run |
|---:|---:|
| 30,000 | $0.0082 |
| 40,000 | $0.0104 |
| 60,000 | $0.0105 |
| 80,000 | $0.0163 |
| 100,000 | $0.0640 |

### Other ideas tested on 2026-11-06

- `--aggressive`: on the test agent $0.0213 per task and 53/57, against $0.0285
  and 54/57 for the previous default. On Claude Code $0.0068 and 57/57, against
  $0.0069 and 57/57. It stays opt-in.
- Hiding tools: 25 Playwright tools are about 5,500 tokens, but a `--tools` flag
  gave no saving on Claude Code ($0.0069 with and without), and the test agent
  already lists only the tools it uses. The flag was not kept.
- Skipping sections repeated from the previous page: only 1%, 6% and 13% of a
  page's text was shared on three sites. Not built.
- Removing `browser_snapshot`'s `filename` option, in Cline with a free model,
  1 run per task: Vanuatu went from 67,851 tokens with 3 file reads to 32,850
  with none. The Wikipedia run with meiose was aborted before it answered, and
  the `Vec` tasks hit the free quota, so this is not yet a Cline result.

### Long sessions

`python eval/run_eval.py --long` runs 5 tasks of about 20 to 30 turns, from
`eval/long_tasks.json`, Claude Haiku, 2026-11-06. Claude Code, 3 runs per task,
all 30 runs correct:

| task | cost without meiose | cost with meiose | saved |
|---|---:|---:|---:|
| `long-books-prices` | $0.0907 | $0.0361 | 60% |
| `long-wiki-years` | $0.0131 | $0.0110 | 16% |
| `long-todo-steps` | $0.0174 | $0.0152 | 13% |
| `long-quotes-count` | $0.0184 | $0.0170 | 7% |
| `long-rust-versions` | $0.0154 | $0.0172 | -12% |
| all 5 | $0.0310 | $0.0193 | 38% (noise, range -2% to 55%) |
| all but `long-books-prices` | $0.0161 | $0.0151 | 6% (noise, range -6% to 14%) |

The books task saved 60% in each of its 3 runs. Its cost without meiose is 2.6x
what its tokens would cost at the rate below Haiku's price jump, so it appears
to cross that line. meiose cuts the tokens written to the cache from 124,306 to
86,680 and reduces that excess, though not all of it. The other four tasks stay
under the line, or have their large pages saved to a file by Claude Code, and
gain little. The saving in Claude Code therefore depends on the session.

Test agent, 1 run per task, all 10 runs correct, so these are a first look:

| task | cost without meiose | cost with meiose |
|---|---:|---:|
| `long-wiki-years` | $1.1871 | $0.0887 |
| `long-rust-versions` | $1.0981 | $0.0996 |
| `long-books-prices` | $0.0646 | $0.0225 |
| `long-quotes-count` | $0.0158 | $0.0120 |
| `long-todo-steps` | $0.0080 | $0.0062 |

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
python eval/run_eval.py --long --modes baseline,meiose --runs 3 --timeout 1800
cargo run --release --example report               # per-page table
cargo run --release --example measure              # budget and query totals
cargo run --release --example search_probe         # are answers in query results
cargo bench --bench prune
```

See [TESTING.md](TESTING.md) for what each run measures and how to read it.
