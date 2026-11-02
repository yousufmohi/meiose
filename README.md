# meiose

meiose is a proxy between an AI agent and its MCP servers that shrinks large
tool outputs before they reach the model. The originals stay available through
a `meiose_expand` tool.

[![CI](https://github.com/yousufmohi/meiose/actions/workflows/ci.yml/badge.svg)](https://github.com/yousufmohi/meiose/actions/workflows/ci.yml)

| | without meiose | with meiose |
|---|---:|---:|
| tasks answered correctly | 30/30 | 30/30 |
| cost per task | $0.0077 | $0.0070 (−9%) |
| tool-output tokens | 293K | 219K (−23%) |

Measured with Claude Code and Claude Haiku on 15 browser tasks through
Playwright MCP, 2 runs each ([details](docs/benchmarks.md)). Claude Code
already hides tool results above ~25K tokens from the model, so it gains less
than agents without that limit.

<!-- TODO: demo GIF (docs/demo.gif) of `meiose install`, a browsing session, then `meiose stats` -->

Output of `meiose stats` after a short Playwright session (one large page was
expanded on purpose, which counts against the savings):

```
Last 7 days: 269K tokens → 251K (6% saved)
  7 tool calls, 3 trimmed · 1 expands (134K tokens, included above)

  server                     tool               calls    before     after   saved
  npx-playwright-mcp-latest  browser_snapshot       4      268K      117K     56%
  npx-playwright-mcp-latest  browser_navigate       2       222       222      0%
  npx-playwright-mcp-latest  browser_click          1       126       126      0%
```

## Install

```bash
brew install yousufmohi/tap/meiose
npm install -g meiose
cargo install meiose
curl -LsSf https://github.com/yousufmohi/meiose/releases/latest/download/meiose-installer.sh | sh
```

Windows (PowerShell):

```powershell
irm https://github.com/yousufmohi/meiose/releases/latest/download/meiose-installer.ps1 | iex
```

## Usage

```bash
meiose install      # wrap the MCP servers in Claude Code, Claude Desktop and Cursor
meiose stats        # tokens saved over the last 7 days
meiose uninstall    # undo install
```

`meiose install` shows every change and asks before writing; it backs up each
file first. Restart your agent afterwards. `meiose doctor` checks the setup and
prints a fix for each problem.

To wrap a single server by hand, put `meiose run --` in front of its command:

```bash
claude mcp add playwright -- meiose run -- npx -y @playwright/mcp@latest
```

## How it works

```
 agent ──stdin──▶ meiose ──stdin──▶ MCP server (e.g. npx @playwright/mcp)
 agent ◀─stdout── meiose ◀─stdout── MCP server
                    │
                    ├─ trims tools/call results (Playwright, JSON, text handlers)
                    ├─ sends repeat snapshots of a page as a diff
                    ├─ adds one tool, meiose_expand(id), answered locally
                    └─ stores originals and stats in SQLite (local only)
```

meiose forwards every message it does not intentionally change exactly as
received. It trims the text of tool results: Playwright snapshots lose empty
wrappers and the refs of non-interactive elements, long JSON arrays keep a
sample, and very long text keeps its head and tail. Links, buttons, inputs and
anything clickable are never removed. Each trimmed result ends with a note
naming a `meiose_expand` id, which returns the server's original result
byte-for-byte. If anything inside meiose fails, the original passes through.

## Documentation

- [How it works](docs/how-it-works.md): trimming rules, diffs, protocol support
- [Benchmarks](docs/benchmarks.md): full results and how to reproduce them
- [Safety](docs/safety.md): guarantees, what is stored and for how long
- [Configuration](docs/configuration.md): flags, environment variables, supported servers, uninstall
- [Troubleshooting](docs/troubleshooting.md)
- [Testing](TESTING.md): test suite and A/B eval

## License

MIT or Apache-2.0, at your option.
