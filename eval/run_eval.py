"""A/B eval: the same browser tasks with and without meiose.

usage:
  python eval/run_eval.py [--client claude-code|loop] [--modes baseline,meiose,budget20000]
                          [--runs 2] [--model haiku] [--tasks id1,id2] [--quick] [--jobs 1]
                          [--no-read] [--meiose PATH]

Clients:
  claude-code  `claude -p` with Playwright MCP. Claude Code saves MCP results
               over 50,000 characters (or MAX_MCP_OUTPUT_TOKENS, default 25,000
               tokens) to a file. The Read tool is allowed so the agent can use
               that file, as it would interactively (--no-read to disallow).
  loop         eval/agent_loop.py: a minimal agent with no output cap. The
               harness calls the MCP server and pastes every result in full.

Modes:
  pure        plain `npx @playwright/mcp`, no meiose at all
  baseline    the same through `meiose run --passthrough` (byte-identical
              traffic, and lets us count tool-output tokens)
  meiose      `meiose run` (default settings)
  aggressive  `meiose run --aggressive`
  budgetN     `meiose run --budget N`, e.g. budget20000

A run is correct if its `ANSWER:` line (or, failing that, its whole reply)
matches the task's regex. Results: eval/results/<timestamp>/{runs.jsonl,report.md}.
Costs real money (it calls Claude). --quick runs 5 tasks once per mode.
"""

import argparse
import concurrent.futures as cf
import json
import os
import random
import re
import shutil
import sqlite3
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import agent_loop  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
TOOLS = ["browser_navigate", "browser_snapshot", "browser_click", "browser_type",
         "browser_fill_form", "browser_select_option", "browser_press_key",
         "browser_wait_for", "browser_navigate_back", "meiose_expand"]
INSTRUCTIONS = (
    "\n\nUse the pw browser tools and read pages with browser_snapshot. "
    "Finish with one final line of the form 'ANSWER: <your answer>'."
)
LOOP_INSTRUCTIONS = "\n\nRead pages with browser_snapshot."
QUICK = ["books-travel-upc", "countries-vanuatu", "vec-extract-if", "hockey-1992", "todomvc"]
# Claude Code's note when it moves a large tool result to a file.
OFFLOAD = re.compile(r"exceeds maximum allowed tokens|has been saved to|saved to a file", re.I)


def default_meiose() -> str:
    exe = "meiose.exe" if os.name == "nt" else "meiose"
    for cand in [ROOT / "target" / "release" / exe, shutil.which("meiose")]:
        if cand and Path(cand).is_file():
            return str(cand)
    sys.exit("meiose not found: run `cargo build --release` first, or pass --meiose PATH")


def server_cmd(mode: str, meiose: str) -> list:
    pw = ["npx", "-y", "@playwright/mcp@latest", "--headless", "--isolated"]
    if mode == "pure":
        return pw
    if mode == "baseline":
        return [meiose, "run", "--passthrough", "--"] + pw
    if mode == "meiose":
        return [meiose, "run", "--"] + pw
    if mode == "aggressive":
        return [meiose, "run", "--aggressive", "--"] + pw
    if m := re.fullmatch(r"budget(\d+)", mode):
        return [meiose, "run", "--budget", m.group(1), "--"] + pw
    raise ValueError(f"unknown mode {mode!r}")


def tool_stats(db: Path) -> dict:
    """Tool-output tokens and expands, from the run's own meiose database."""
    if not db.is_file():
        return {}
    con = sqlite3.connect(db)
    try:
        r = con.execute(
            "SELECT COALESCE(SUM(CASE WHEN kind='call' THEN tokens_before END),0),"
            " COALESCE(SUM(CASE WHEN kind='call' THEN tokens_after END),0),"
            " COALESCE(SUM(kind='expand'),0),"
            " COALESCE(SUM(kind='expand' AND handler='query'),0),"
            " COALESCE(SUM(CASE WHEN kind='expand' THEN tokens_after END),0) FROM calls").fetchone()
        calls = [f"{t}:{h or '-'}:{b}->{a}" for t, h, b, a in con.execute(
            "SELECT tool, handler, tokens_before, tokens_after FROM calls ORDER BY ts_ms")]
        return {"tool_tokens_before": r[0], "tool_tokens_after": r[1], "expands": r[2],
                "query_expands": r[3], "full_expands": r[2] - r[3], "expand_tokens": r[4],
                "calls": calls}
    finally:
        con.close()


def run_claude_code(task, cmd, env, args, work: Path) -> dict:
    cfg = {"mcpServers": {"pw": {"command": cmd[0], "args": cmd[1:], "env": env}}}
    (work / "mcp.json").write_text(json.dumps(cfg), encoding="utf-8")
    allowed = [f"mcp__pw__{t}" for t in TOOLS] + ([] if args.no_read else ["Read"])
    argv = [shutil.which("claude") or "claude", "-p", task["prompt"] + INSTRUCTIONS,
            "--mcp-config", str(work / "mcp.json"), "--strict-mcp-config",
            "--allowedTools", ",".join(allowed), "--model", args.model,
            "--output-format", "stream-json", "--verbose"]
    try:
        p = subprocess.run(argv, cwd=work, capture_output=True, timeout=args.timeout,
                           encoding="utf-8", errors="replace")
    except subprocess.TimeoutExpired:
        return {"is_error": True, "result": f"TIMEOUT after {args.timeout}s"}
    d = {"is_error": True, "result": "no result event"}
    offloads = reads = 0
    for line in p.stdout.splitlines():
        try:
            m = json.loads(line)
        except ValueError:
            continue
        msg = m.get("message")
        content = msg.get("content") if isinstance(msg, dict) else None
        if m.get("type") == "user" and isinstance(content, list):
            for c in content:
                if isinstance(c, dict) and c.get("type") == "tool_result" and OFFLOAD.search(json.dumps(c.get("content"))):
                    offloads += 1
        if m.get("type") == "assistant" and isinstance(content, list):
            reads += sum(1 for c in content if isinstance(c, dict) and c.get("type") == "tool_use" and c.get("name") == "Read")
        if m.get("type") == "result":
            d = m
    d["offloads"], d["reads"] = offloads, reads
    return d


def run_one(task: dict, mode: str, rep: int, args, out_dir: Path) -> dict:
    work = Path(tempfile.mkdtemp(prefix=f"meiose-eval-{task['id']}-{mode}-"))
    data = work / "data"
    cmd = server_cmd(mode, args.meiose)
    started = time.time()
    if args.client == "loop":
        env = dict(os.environ, MEIOSE_DATA_DIR=str(data))
        d = agent_loop.run(task["prompt"] + LOOP_INSTRUCTIONS, cmd, env, args.model, TOOLS, args.timeout)
    else:
        d = run_claude_code(task, cmd, {"MEIOSE_DATA_DIR": str(data)}, args, work)
    u = d.get("usage") or {}
    result = d.get("result") or ""
    m = re.findall(r"ANSWER:\s*(.+)", result)
    checked = m[-1] if m else result
    rec = {
        "task": task["id"], "mode": mode, "rep": rep, "model": args.model, "client": args.client,
        "ok": bool(re.search(task["answer"], checked, re.I)) and not d.get("is_error"),
        "answer": (m[-1] if m else result[-200:]).strip(),
        "input_tokens": u.get("input_tokens", 0) or 0,
        "cache_write_tokens": u.get("cache_creation_input_tokens", 0) or 0,
        "cache_read_tokens": u.get("cache_read_input_tokens", 0) or 0,
        "output_tokens": u.get("output_tokens", 0) or 0,
        "cost_usd": d.get("total_cost_usd") or 0.0,
        "duration_s": round(time.time() - started, 1),
        "turns": d.get("num_turns", 0),
        "is_error": bool(d.get("is_error")),
        "offloads": d.get("offloads", 0),
        "reads": d.get("reads", 0),
        "largest_tool_result_chars": d.get("largest_tool_result_chars"),
    }
    rec["billed_tokens"] = (rec["input_tokens"] + rec["cache_write_tokens"]
                            + rec["cache_read_tokens"] + rec["output_tokens"])
    rec.update({"tool_tokens_before": 0, "tool_tokens_after": 0, "expands": 0,
                "query_expands": 0, "full_expands": 0, "expand_tokens": 0, "calls": []})
    rec.update(tool_stats(data / "meiose.db"))
    with open(out_dir / "runs.jsonl", "a", encoding="utf-8") as f:
        f.write(json.dumps(rec) + "\n")
    shutil.rmtree(work, ignore_errors=True)
    mark = "ok  " if rec["ok"] else "FAIL"
    print(f"  {mark} {task['id']:<22} {mode:<12} #{rep}  {rec['billed_tokens']:>9,} tok  "
          f"${rec['cost_usd']:.4f}  exp {rec['expands']}({rec['query_expands']}q)  "
          f"off {rec['offloads']}  {rec['duration_s']:>5}s  {rec['answer'][:40]!r}", flush=True)
    return rec


# ---------------- report ----------------

def mean_sd(xs):
    xs = list(xs)
    if not xs:
        return 0.0, 0.0
    return statistics.mean(xs), (statistics.stdev(xs) if len(xs) > 1 else 0.0)


def paired_saving_ci(runs, mode, base, key, tasks, n=4000, seed=1):
    """Saving of `mode` vs `base` on `key` (1 - sum_mode / sum_base) over the
    per-task means, with a bootstrap 95% interval from resampling tasks."""
    per = []
    for t in tasks:
        a = [r[key] for r in runs if r["task"] == t and r["mode"] == mode]
        b = [r[key] for r in runs if r["task"] == t and r["mode"] == base]
        if a and b:
            per.append((statistics.mean(a), statistics.mean(b)))
    if not per or not sum(b for _, b in per):
        return None
    point = 1 - sum(a for a, _ in per) / sum(b for _, b in per)
    rng = random.Random(seed)
    boots = []
    for _ in range(n):
        s = [rng.choice(per) for _ in per]
        sb = sum(b for _, b in s)
        if sb:
            boots.append(1 - sum(a for a, _ in s) / sb)
    boots.sort()
    return point, boots[int(0.025 * len(boots))], boots[int(0.975 * len(boots)) - 1]


def fmt_ci(ci):
    if ci is None:
        return "—"
    p, lo, hi = ci
    verdict = "" if lo > 0 or hi < 0 else " (within noise)"
    return f"{100*p:+.0f}% [{100*lo:+.0f}, {100*hi:+.0f}]{verdict}"


def report(runs: list, modes: list, tasks: list, args) -> str:
    ids = [t["id"] for t in tasks]
    base = "baseline" if "baseline" in modes else ("pure" if "pure" in modes else None)
    L = [f"# meiose eval — {time.strftime('%Y-%m-%d %H:%M')}", "",
         f"Client `{args.client}`, model `{args.model}`, {len(tasks)} tasks, "
         f"{args.runs} run(s) per task per mode. Reference mode: `{base}`.", "",
         "## Summary", "",
         "Savings are 1 − (mode / reference), summed over per-task means; brackets are a "
         "bootstrap 95% interval over tasks. Positive = mode uses less.", "",
         "| mode | correct | billed tokens / run (mean ± sd) | saving | cost / run (mean ± sd) | saving | "
         "runs with expand | expands (query/full) | offloaded results | tool-output tokens |",
         "|---|---:|---:|---|---:|---|---:|---:|---:|---:|"]
    for m in modes:
        rs = [r for r in runs if r["mode"] == m]
        if not rs:
            continue
        bt, bsd = mean_sd(r["billed_tokens"] for r in rs)
        ct, csd = mean_sd(r["cost_usd"] for r in rs)
        tb = sum(r["tool_tokens_before"] for r in rs)
        ta = sum(r["tool_tokens_after"] + r["expand_tokens"] for r in rs)
        tool = f"{tb:,} → {ta:,}" if tb else "n/a"
        with_exp = sum(1 for r in rs if r["expands"])
        q = sum(r["query_expands"] for r in rs)
        f_ = sum(r["full_expands"] for r in rs)
        L.append(
            f"| {m} | {sum(r['ok'] for r in rs)}/{len(rs)} | {bt:,.0f} ± {bsd:,.0f} | "
            f"{'—' if m == base else fmt_ci(paired_saving_ci(runs, m, base, 'billed_tokens', ids))} | "
            f"${ct:.4f} ± {csd:.4f} | "
            f"{'—' if m == base else fmt_ci(paired_saving_ci(runs, m, base, 'cost_usd', ids))} | "
            f"{with_exp}/{len(rs)} | {q}/{f_} | {sum(r['offloads'] for r in rs)} | {tool} |")
    L += ["", "Billed tokens = input + cache writes + cache reads + output, as reported by Claude. "
          "Tool-output tokens are o200k counts of what the tools returned (after meiose, incl. expands).",
          "", "## Per task", "",
          "Mean billed tokens per run, correct runs, and expands (query/full).", "",
          "| task | " + " | ".join(modes) + " |", "|---|" + "---:|" * len(modes)]
    for t in tasks:
        cells = []
        for m in modes:
            rs = [r for r in runs if r["task"] == t["id"] and r["mode"] == m]
            if not rs:
                cells.append("—")
                continue
            cells.append(f"{statistics.mean(r['billed_tokens'] for r in rs):,.0f} "
                         f"({sum(r['ok'] for r in rs)}/{len(rs)} ✓, "
                         f"{sum(r['query_expands'] for r in rs)}/{sum(r['full_expands'] for r in rs)})")
        L.append(f"| {t['id']} | " + " | ".join(cells) + " |")
    L += ["", "## Failures", ""]
    fails = [r for r in runs if not r["ok"]]
    if not fails:
        L.append("None.")
    for r in sorted(fails, key=lambda r: (r["task"], r["mode"], r["rep"])):
        L.append(f"- {r['task']} ({r['mode']} #{r['rep']}): answered {r['answer'][:120]!r}. "
                 f"Calls: {', '.join(r['calls'][:12]) or 'none recorded'}")
    return "\n".join(L) + "\n"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--client", choices=["claude-code", "loop"], default="claude-code")
    ap.add_argument("--quick", action="store_true", help="5 representative tasks, 1 run per mode")
    ap.add_argument("--runs", type=int, default=2)
    ap.add_argument("--model", default="haiku")
    ap.add_argument("--modes", default="baseline,meiose")
    ap.add_argument("--tasks", default="")
    ap.add_argument("--jobs", type=int, default=1, help="parallel runs (each gets its own browser)")
    ap.add_argument("--timeout", type=int, default=600)
    ap.add_argument("--no-read", action="store_true", help="claude-code: do not allow the Read tool")
    ap.add_argument("--meiose", default=None)
    args = ap.parse_args()
    sys.stdout.reconfigure(encoding="utf-8")
    args.meiose = args.meiose or default_meiose()
    if not shutil.which("claude"):
        sys.exit("`claude` (Claude Code CLI) not found on PATH")
    if not shutil.which("npx"):
        sys.exit("`npx` not found on PATH: install Node.js")

    all_tasks = json.loads((ROOT / "eval" / "tasks.json").read_text(encoding="utf-8"))
    wanted = [t for t in args.tasks.split(",") if t] or (QUICK if args.quick else [])
    tasks = [t for t in all_tasks if not wanted or t["id"] in wanted]
    if args.quick:
        args.runs = 1
    modes = [m for m in args.modes.split(",") if m]
    for m in modes:
        server_cmd(m, args.meiose)  # fail fast on a typo
    out_dir = ROOT / "eval" / "results" / time.strftime("%Y%m%d-%H%M%S")
    out_dir.mkdir(parents=True, exist_ok=True)
    jobs = [(t, m, r) for r in range(1, args.runs + 1) for t in tasks for m in modes]
    print(f"{len(jobs)} runs ({len(tasks)} tasks × {len(modes)} modes × {args.runs}), client {args.client}, "
          f"model {args.model}, meiose {args.meiose}\nresults → {out_dir}", flush=True)
    runs = []
    with cf.ThreadPoolExecutor(max_workers=max(1, args.jobs)) as ex:
        for fut in cf.as_completed([ex.submit(run_one, t, m, r, args, out_dir) for t, m, r in jobs]):
            runs.append(fut.result())
    rep = report(runs, modes, tasks, args)
    (out_dir / "report.md").write_text(rep, encoding="utf-8")
    print("\n" + rep)


if __name__ == "__main__":
    main()
