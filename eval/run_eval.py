"""A/B eval: the same browser tasks through Claude Code headless, with and
without meiose. Reports success rate, tokens, cost and time per mode.

usage:
  python eval/run_eval.py [--quick] [--runs 2] [--model haiku] [--modes baseline,meiose]
                          [--tasks id1,id2] [--jobs 1] [--pure-baseline] [--meiose PATH]

Modes:
  baseline    Playwright MCP through `meiose run --passthrough` (byte-identical
              traffic; lets us count tool-output tokens). With
              --pure-baseline, plain `npx @playwright/mcp` with no meiose at all.
  meiose      `meiose run` (conservative defaults, diffing on)
  aggressive  `meiose run --aggressive`

Every mode gets the same prompt, model and allowed tools. A run succeeds if
its `ANSWER:` line (or, failing that, its whole reply) matches the task's
regex. Results go to eval/results/<timestamp>/{runs.jsonl,report.md}.

Costs real money (it calls Claude). --quick runs 5 tasks once per mode.
"""

import argparse
import concurrent.futures as cf
import json
import os
import re
import shutil
import sqlite3
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TOOLS = ["browser_navigate", "browser_snapshot", "browser_click", "browser_type",
         "browser_fill_form", "browser_select_option", "browser_press_key",
         "browser_wait_for", "browser_navigate_back", "meiose_expand"]
INSTRUCTIONS = (
    "\n\nUse the pw browser tools and read pages with browser_snapshot. "
    "Finish with one final line of the form 'ANSWER: <your answer>'."
)
QUICK = ["books-travel-upc", "quotes-page3", "countries-bhutan", "hockey-1992", "todomvc"]


def default_meiose() -> str:
    exe = "meiose.exe" if os.name == "nt" else "meiose"
    for cand in [ROOT / "target" / "release" / exe, shutil.which("meiose")]:
        if cand and Path(cand).is_file():
            return str(cand)
    sys.exit("meiose not found: run `cargo build --release` first, or pass --meiose PATH")


def server_cmd(mode: str, meiose: str, pure: bool) -> list:
    pw = ["npx", "-y", "@playwright/mcp@latest", "--headless", "--isolated"]
    if mode == "baseline":
        return pw if pure else [meiose, "run", "--passthrough", "--"] + pw
    if mode == "meiose":
        return [meiose, "run", "--"] + pw
    if mode == "aggressive":
        return [meiose, "run", "--aggressive", "--"] + pw
    raise ValueError(mode)


def tool_stats(db: Path) -> dict:
    """Tool-output tokens before/after meiose, from the run's own database."""
    if not db.is_file():
        return {}
    con = sqlite3.connect(db)
    try:
        r = con.execute(
            "SELECT COALESCE(SUM(CASE WHEN kind='call' THEN tokens_before END),0),"
            " COALESCE(SUM(CASE WHEN kind='call' THEN tokens_after END),0),"
            " COALESCE(SUM(kind='expand'),0),"
            " COALESCE(SUM(CASE WHEN kind='expand' THEN tokens_after END),0) FROM calls").fetchone()
        handlers = [f"{t}:{h or '-'}:{b}->{a}" for t, h, b, a in con.execute(
            "SELECT tool, handler, tokens_before, tokens_after FROM calls ORDER BY ts_ms")]
        return {"tool_tokens_before": r[0], "tool_tokens_after": r[1],
                "expands": r[2], "expand_tokens": r[3], "calls": handlers}
    finally:
        con.close()


def run_one(task: dict, mode: str, rep: int, args, out_dir: Path) -> dict:
    work = Path(tempfile.mkdtemp(prefix=f"meiose-eval-{task['id']}-{mode}-"))
    data = work / "data"
    cmd = server_cmd(mode, args.meiose, args.pure_baseline)
    cfg = {"mcpServers": {"pw": {"command": cmd[0], "args": cmd[1:],
                                 "env": {"MEIOSE_DATA_DIR": str(data)}}}}
    (work / "mcp.json").write_text(json.dumps(cfg), encoding="utf-8")
    claude = shutil.which("claude") or "claude"
    argv = [claude, "-p", task["prompt"] + INSTRUCTIONS,
            "--mcp-config", str(work / "mcp.json"), "--strict-mcp-config",
            "--allowedTools", ",".join(f"mcp__pw__{t}" for t in TOOLS),
            "--model", args.model, "--output-format", "json"]
    started = time.time()
    rec = {"task": task["id"], "mode": mode, "rep": rep, "model": args.model}
    try:
        p = subprocess.run(argv, cwd=work, capture_output=True, timeout=args.timeout,
                           encoding="utf-8", errors="replace")
        d = json.loads(p.stdout)
    except subprocess.TimeoutExpired:
        d = {"is_error": True, "result": f"TIMEOUT after {args.timeout}s"}
    except (json.JSONDecodeError, OSError) as e:
        d = {"is_error": True, "result": f"harness error: {e!r}"}
    u = d.get("usage") or {}
    result = d.get("result") or ""
    m = re.findall(r"ANSWER:\s*(.+)", result)
    checked = m[-1] if m else result
    rec.update({
        "ok": bool(re.search(task["answer"], checked, re.I)) and not d.get("is_error"),
        "answer": (m[-1] if m else result[-200:]).strip(),
        "input_tokens": u.get("input_tokens", 0),
        "cache_write_tokens": u.get("cache_creation_input_tokens", 0),
        "cache_read_tokens": u.get("cache_read_input_tokens", 0),
        "output_tokens": u.get("output_tokens", 0),
        "cost_usd": d.get("total_cost_usd") or 0.0,
        "duration_s": round((d.get("duration_ms") or (time.time() - started) * 1000) / 1000, 1),
        "turns": d.get("num_turns", 0),
        "is_error": bool(d.get("is_error")),
    })
    rec["total_tokens"] = (rec["input_tokens"] + rec["cache_write_tokens"]
                           + rec["cache_read_tokens"] + rec["output_tokens"])
    rec.update(tool_stats(data / "meiose.db"))
    with open(out_dir / "runs.jsonl", "a", encoding="utf-8") as f:
        f.write(json.dumps(rec) + "\n")
    shutil.rmtree(work, ignore_errors=True)
    mark = "ok  " if rec["ok"] else "FAIL"
    print(f"  {mark} {task['id']:<20} {mode:<10} #{rep}  {rec['total_tokens']:>8,} tok  "
          f"${rec['cost_usd']:.4f}  {rec['duration_s']:>5}s  {rec['answer'][:40]!r}", flush=True)
    return rec


def pct(new, old):
    return 100.0 * (1 - new / old) if old else 0.0


def report(runs: list, modes: list, tasks: list, args) -> str:
    L = [f"# meiose eval — {time.strftime('%Y-%m-%d %H:%M')}", "",
         f"Model `{args.model}`, {args.runs} run(s) per task per mode, {len(tasks)} tasks. "
         f"Baseline: {'plain Playwright MCP' if args.pure_baseline else 'meiose --passthrough (byte-identical traffic)'}.",
         "", "## Summary", "",
         "| mode | success | total tokens (mean/run) | vs baseline | cost (mean/run) | vs baseline | time (mean/run) | tool-output tokens |",
         "|---|---:|---:|---:|---:|---:|---:|---:|"]
    by_mode = {m: [r for r in runs if r["mode"] == m] for m in modes}
    base = by_mode.get("baseline") or []
    bt = statistics.mean(r["total_tokens"] for r in base) if base else 0
    bc = statistics.mean(r["cost_usd"] for r in base) if base else 0
    for m in modes:
        rs = by_mode[m]
        if not rs:
            continue
        ok = sum(r["ok"] for r in rs)
        t = statistics.mean(r["total_tokens"] for r in rs)
        c = statistics.mean(r["cost_usd"] for r in rs)
        d = statistics.mean(r["duration_s"] for r in rs)
        tb = sum(r.get("tool_tokens_before", 0) for r in rs)
        ta = sum(r.get("tool_tokens_after", 0) + r.get("expand_tokens", 0) for r in rs)
        tool = f"{tb:,} → {ta:,} ({pct(ta, tb):.0f}% less)" if tb else "n/a"
        vs_t = "—" if m == "baseline" or not bt else f"{pct(t, bt):+.0f}% less" if t <= bt else f"{-pct(t, bt):.0f}% more"
        vs_c = "—" if m == "baseline" or not bc else f"{pct(c, bc):+.0f}% less" if c <= bc else f"{-pct(c, bc):.0f}% more"
        L.append(f"| {m} | {ok}/{len(rs)} | {t:,.0f} | {vs_t.replace('+', '')} | ${c:.4f} | {vs_c.replace('+', '')} | {d:.0f}s | {tool} |")
    L += ["", "Total tokens = input + cache writes + cache reads + output, as billed context. "
          "Tool-output tokens = what the browser tools returned (o200k count), after meiose incl. expands.",
          "", "## Per task (mean total tokens per run; ✓ = runs answered correctly)", "",
          "| task | " + " | ".join(modes) + " | notes |", "|---|" + "---:|" * len(modes) + "---|"]
    for t in tasks:
        cells = []
        for m in modes:
            rs = [r for r in runs if r["task"] == t["id"] and r["mode"] == m]
            if not rs:
                cells.append("—")
                continue
            cells.append(f"{statistics.mean(r['total_tokens'] for r in rs):,.0f} "
                         f"({sum(r['ok'] for r in rs)}/{len(rs)} ✓)")
        L.append(f"| {t['id']} | " + " | ".join(cells) + f" | {t.get('notes', '')} |")
    regress = []
    for t in tasks:
        b_ok = [r["ok"] for r in runs if r["task"] == t["id"] and r["mode"] == "baseline"]
        for m in modes:
            if m == "baseline":
                continue
            for r in runs:
                if r["task"] == t["id"] and r["mode"] == m and not r["ok"] and any(b_ok):
                    regress.append(r)
    L += ["", "## Failures where baseline succeeded", ""]
    if not regress:
        L.append("None.")
    for r in regress:
        L.append(f"- **{r['task']}** ({r['mode']} #{r['rep']}): answered {r['answer']!r}. "
                 f"meiose calls: {', '.join(r.get('calls', [])) or 'none recorded'}")
    L += ["", "Small samples are noisy: model runs differ even with identical inputs. "
          "Treat single-run differences in success as signals to investigate, not verdicts."]
    return "\n".join(L) + "\n"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--quick", action="store_true", help="5 representative tasks, 1 run per mode")
    ap.add_argument("--runs", type=int, default=2)
    ap.add_argument("--model", default="haiku")
    ap.add_argument("--modes", default="baseline,meiose")
    ap.add_argument("--tasks", default="")
    ap.add_argument("--jobs", type=int, default=1, help="parallel runs (each gets its own browser)")
    ap.add_argument("--timeout", type=int, default=420)
    ap.add_argument("--pure-baseline", action="store_true")
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
    out_dir = ROOT / "eval" / "results" / time.strftime("%Y%m%d-%H%M%S")
    out_dir.mkdir(parents=True, exist_ok=True)
    jobs = [(t, m, r) for r in range(1, args.runs + 1) for t in tasks for m in modes]
    print(f"{len(jobs)} runs ({len(tasks)} tasks × {len(modes)} modes × {args.runs}), model {args.model}, "
          f"meiose {args.meiose}\nresults → {out_dir}", flush=True)
    runs = []
    with cf.ThreadPoolExecutor(max_workers=max(1, args.jobs)) as ex:
        for fut in cf.as_completed([ex.submit(run_one, t, m, r, args, out_dir) for t, m, r in jobs]):
            runs.append(fut.result())
    rep = report(runs, modes, tasks, args)
    (out_dir / "report.md").write_text(rep, encoding="utf-8")
    print("\n" + rep)


if __name__ == "__main__":
    main()
