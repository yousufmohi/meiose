"""Capture Playwright MCP snapshot fixtures by driving a real browser through meiose.

usage:
  python scripts/capture_fixtures.py [--meiose PATH] [--out tests/fixtures/playwright] [URL ...]

For each URL: browser_navigate, then browser_snapshot. The exact text of each
snapshot result is written to <out>/<slug>.txt. The full session is also
recorded by `meiose record` (into a temp dir) for protocol-level debugging.

Only use public pages for fixtures that get committed. To turn your own
recordings into fixtures, use scripts/extract_fixtures.py, which writes to the
git-ignored tests/fixtures/local/.
"""

import argparse
import json
import os
import queue
import re
import subprocess
import sys
import tempfile
import threading
from pathlib import Path

DEFAULT_URLS = [
    # reference / long text
    "https://en.wikipedia.org/wiki/Rust_(programming_language)",
    "https://en.wikipedia.org/wiki/List_of_countries_by_population_(United_Nations)",
    "https://developer.mozilla.org/en-US/docs/Web/HTML/Element/input",
    "https://docs.python.org/3/library/asyncio-task.html",
    "https://doc.rust-lang.org/std/vec/struct.Vec.html",
    "https://docs.rs/tokio/latest/tokio/",
    "https://react.dev/learn",
    "https://arxiv.org/list/cs.AI/recent",
    # news / feeds / link lists
    "https://news.ycombinator.com/",
    "https://lobste.rs/",
    "https://old.reddit.com/r/rust/",
    "https://www.bbc.com/news",
    "https://lite.cnn.com/",
    "https://www.theguardian.com/international",
    # code hosting / packages
    "https://github.com/rust-lang/rust",
    "https://github.com/microsoft/playwright/issues",
    "https://www.npmjs.com/package/@playwright/mcp",
    "https://crates.io/crates/serde",
    "https://pypi.org/project/requests/",
    "https://stackoverflow.com/questions/tagged/rust",
    # commerce / listings
    "https://www.ebay.com/sch/i.html?_nkw=mechanical+keyboard",
    "https://books.toscrape.com/",
    "https://www.etsy.com/search?q=mug",
    "https://www.imdb.com/chart/top/",
    # forms / interactive widgets
    "https://the-internet.herokuapp.com/login",
    "https://the-internet.herokuapp.com/dropdown",
    "https://the-internet.herokuapp.com/checkboxes",
    "https://the-internet.herokuapp.com/tables",
    "https://the-internet.herokuapp.com/large",
    "https://demo.playwright.dev/todomvc/",
    "https://httpbin.org/forms/post",
    "https://www.w3schools.com/html/html_forms.asp",
    "https://demoqa.com/automation-practice-form",
    "https://www.saucedemo.com/",
    # gov / misc
    "https://www.usa.gov/",
    "https://www.weather.gov/",
    "https://www.python.org/",
    "https://www.rust-lang.org/",
    "https://duckduckgo.com/html/?q=model+context+protocol",
    "https://example.com/",
]


def slug(url: str) -> str:
    s = re.sub(r"^https?://(www\.)?", "", url)
    s = re.sub(r"[^a-zA-Z0-9]+", "-", s).strip("-").lower()
    return s[:80] or "page"


class Session:
    def __init__(self, meiose: str, rec_dir: str):
        self.p = subprocess.Popen(
            [meiose, "record", "--out", rec_dir, "--",
             "npx", "-y", "@playwright/mcp@latest", "--headless", "--isolated"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        )
        self.q: "queue.Queue[bytes]" = queue.Queue()
        threading.Thread(target=self._pump, daemon=True).start()
        self.next_id = 1

    def _pump(self):
        for line in self.p.stdout:
            self.q.put(line)
        self.q.put(b"")

    def request(self, method, params=None, timeout=60.0):
        rid = self.next_id
        self.next_id += 1
        msg = {"jsonrpc": "2.0", "id": rid, "method": method}
        if params is not None:
            msg["params"] = params
        self.p.stdin.write((json.dumps(msg) + "\n").encode())
        self.p.stdin.flush()
        while True:
            line = self.q.get(timeout=timeout)
            if not line:
                raise RuntimeError("meiose exited")
            m = json.loads(line)
            if m.get("id") == rid and ("result" in m or "error" in m):
                return m
            if "method" in m and "id" in m:  # server→client request (e.g. roots/list)
                reply = {"jsonrpc": "2.0", "id": m["id"], "result": {"roots": []}}
                self.p.stdin.write((json.dumps(reply) + "\n").encode())
                self.p.stdin.flush()

    def notify(self, method):
        self.p.stdin.write((json.dumps({"jsonrpc": "2.0", "method": method}) + "\n").encode())
        self.p.stdin.flush()

    def close(self):
        try:
            self.request("tools/call", {"name": "browser_close", "arguments": {}}, timeout=15)
        except Exception:
            pass
        self.p.stdin.close()
        try:
            self.p.wait(timeout=20)
        except subprocess.TimeoutExpired:
            self.p.kill()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--meiose", default="meiose")
    ap.add_argument("--out", default="tests/fixtures/playwright")
    ap.add_argument("urls", nargs="*")
    a = ap.parse_args()
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    rec_dir = tempfile.mkdtemp(prefix="meiose-capture-")
    # Run from a temp cwd so Playwright's .playwright-mcp/ files land there.
    os.chdir(tempfile.mkdtemp(prefix="meiose-capture-cwd-"))

    s = Session(a.meiose, rec_dir)
    init = s.request("initialize", {
        "protocolVersion": "2025-11-25", "capabilities": {},
        "clientInfo": {"name": "meiose-capture", "version": "0"}})
    print("server:", init["result"]["serverInfo"], file=sys.stderr)
    s.notify("notifications/initialized")

    ok = 0
    for url in a.urls or DEFAULT_URLS:
        try:
            nav = s.request("tools/call", {"name": "browser_navigate", "arguments": {"url": url}}, timeout=60)
            if "error" in nav or nav["result"].get("isError"):
                print(f"SKIP {url}: navigate failed", file=sys.stderr)
                continue
            s.request("tools/call", {"name": "browser_wait_for", "arguments": {"time": 2}}, timeout=30)
            snap = s.request("tools/call", {"name": "browser_snapshot", "arguments": {}}, timeout=60)
            if "error" in snap or snap["result"].get("isError"):
                print(f"SKIP {url}: snapshot failed", file=sys.stderr)
                continue
            texts = [c["text"] for c in snap["result"]["content"] if c.get("type") == "text"]
            text = "\n".join(texts)
            path = out / f"{slug(url)}.txt"
            path.write_bytes(text.encode("utf-8"))
            ok += 1
            print(f"OK   {len(text):>8} chars  {path.name}", file=sys.stderr)
        except Exception as e:  # keep going: one bad site must not end the run
            print(f"SKIP {url}: {e!r}", file=sys.stderr)
    s.close()
    print(f"{ok} fixtures written to {out} (recording in {rec_dir})", file=sys.stderr)


if __name__ == "__main__":
    main()
