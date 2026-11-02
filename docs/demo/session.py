"""Real browsing session through meiose, used to fill the demo database.

usage: python docs/demo/session.py   (with MEIOSE_DATA_DIR set, see record.ps1)

Drives Playwright MCP through `meiose run` the way an agent would: Hacker News,
one line of it fetched with meiose_expand(query=...), the same page again (sent
as a diff), then a checkbox click and re-snapshot.
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile

meiose = shutil.which("meiose") or sys.exit("meiose not on PATH")
os.chdir(tempfile.mkdtemp())  # Playwright writes .playwright-mcp/ here
p = subprocess.Popen(
    [meiose, "run", "--", "npx", "-y", "@playwright/mcp@latest", "--headless", "--isolated"],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
)
next_id = 0


def request(method, params=None):
    global next_id
    next_id += 1
    msg = {"jsonrpc": "2.0", "id": next_id, "method": method, "params": params or {}}
    p.stdin.write((json.dumps(msg) + "\n").encode())
    p.stdin.flush()
    while True:
        m = json.loads(p.stdout.readline())
        if m.get("id") == next_id and "method" not in m:
            return m
        if "method" in m and "id" in m:  # server asks for roots
            p.stdin.write((json.dumps({"jsonrpc": "2.0", "id": m["id"], "result": {"roots": []}}) + "\n").encode())
            p.stdin.flush()


def call(name, args=None):
    return request("tools/call", {"name": name, "arguments": args or {}})["result"]["content"][0].get("text", "")


request("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                       "clientInfo": {"name": "meiose-demo", "version": "0"}})
p.stdin.write(b'{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
p.stdin.flush()
request("tools/list")

call("browser_navigate", {"url": "https://news.ycombinator.com/"})
hn = call("browser_snapshot")
# Only the lines the agent needs from the full page, not the whole page.
expand_id = hn.split('meiose_expand("')[1].split('"')[0]
call("meiose_expand", {"id": expand_id, "query": "login"})
call("browser_snapshot")
call("browser_navigate", {"url": "https://the-internet.herokuapp.com/checkboxes"})
snap = call("browser_snapshot")
ref = snap.split("checkbox [ref=")[1].split("]")[0]
call("browser_click", {"element": "first checkbox", "target": ref})
call("browser_snapshot")
call("browser_close")
p.stdin.close()
p.wait(timeout=30)
print("demo session recorded")
