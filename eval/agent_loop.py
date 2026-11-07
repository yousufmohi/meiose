"""A minimal agent with no tool-output cap, for measuring clients other than
Claude Code.

The harness talks to the MCP server itself and pastes every tool result into
the conversation in full; Claude only decides the next step. Claude runs as
one long-lived `claude -p` session in stream-JSON mode with no built-in tools
and a short system prompt, so prompt caching works as in a real agent.

    run(prompt, server_cmd, env, model, timeout) -> dict  (see run_eval.py)
"""

import json
import queue
import shutil
import subprocess
import tempfile
import threading
import time

SYSTEM = """You control a web browser through tools to complete the user's task.
Every reply must be exactly one JSON object and nothing else:
  {"tool": "<tool name>", "arguments": {...}}   to call a tool, or
  {"answer": "<final answer>"}                  when you are done.
After each tool call you receive the tool's result. Tools:
"""
MAX_STEPS = 25


class _Lines:
    """Read a pipe line by line on a thread, so reads can time out."""

    def __init__(self, pipe):
        self.q: "queue.Queue[str]" = queue.Queue()
        threading.Thread(target=self._pump, args=(pipe,), daemon=True).start()

    def _pump(self, pipe):
        for line in pipe:
            self.q.put(line)
        self.q.put("")

    def get(self, deadline: float) -> str:
        return self.q.get(timeout=max(0.1, deadline - time.time()))


class Mcp:
    def __init__(self, cmd: list, env: dict, cwd: str):
        self.p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL, env=env, cwd=cwd,
                                  encoding="utf-8", errors="replace")
        self.out = _Lines(self.p.stdout)
        self.next_id = 0

    def _send(self, msg: dict):
        self.p.stdin.write(json.dumps(msg) + "\n")
        self.p.stdin.flush()

    def request(self, method: str, params: dict, deadline: float) -> dict:
        self.next_id += 1
        rid = self.next_id
        self._send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        while True:
            line = self.out.get(deadline)
            if not line:
                raise RuntimeError("MCP server exited")
            m = json.loads(line)
            if m.get("id") == rid and "method" not in m:
                return m
            if "method" in m and "id" in m:  # server asks us something (roots/list)
                self._send({"jsonrpc": "2.0", "id": m["id"], "result": {"roots": []}})

    def start(self, deadline: float) -> list:
        self.request("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                                    "clientInfo": {"name": "meiose-eval-loop", "version": "0"}}, deadline)
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        return self.request("tools/list", {}, deadline)["result"]["tools"]

    def call(self, name: str, arguments: dict, deadline: float) -> str:
        r = self.request("tools/call", {"name": name, "arguments": arguments}, deadline)
        if "error" in r:
            return f"Error: {r['error'].get('message', r['error'])}"
        return "\n".join(c.get("text", f"<{c.get('type')}>") for c in r["result"].get("content", []))

    def close(self):
        try:
            self.p.stdin.close()
            self.p.wait(timeout=20)
        except Exception:
            self.p.kill()


def _first_json(text: str):
    """The first JSON object in a reply, tolerating code fences around it."""
    i = text.find("{")
    while i != -1:
        try:
            obj, _ = json.JSONDecoder().raw_decode(text[i:])
            if isinstance(obj, dict):
                return obj
        except ValueError:
            pass
        i = text.find("{", i + 1)
    return None


def run(prompt: str, server_cmd: list, env: dict, model: str, allowed: list,
        timeout: int, max_steps: int = MAX_STEPS) -> dict:
    deadline = time.time() + timeout
    requests = []
    work = tempfile.mkdtemp(prefix="meiose-loop-")
    usage = {"input_tokens": 0, "cache_creation_input_tokens": 0,
             "cache_read_input_tokens": 0, "output_tokens": 0}
    out = {"result": "", "is_error": False, "num_turns": 0, "total_cost_usd": 0.0,
           "tool_calls": 0, "largest_tool_result_chars": 0}
    mcp = Mcp(server_cmd, env, work)
    claude = None
    try:
        tools = [t for t in mcp.start(deadline) if t["name"] in allowed]
        system = SYSTEM + json.dumps(
            [{"name": t["name"], "description": t.get("description", ""),
              "inputSchema": t.get("inputSchema", {})} for t in tools])
        claude = subprocess.Popen(
            [shutil.which("claude") or "claude", "-p", "--input-format", "stream-json",
             "--output-format", "stream-json", "--verbose", "--tools", "",
             "--strict-mcp-config", "--no-session-persistence", "--model", model,
             "--system-prompt", system],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            encoding="utf-8", errors="replace", cwd=work)
        replies = _Lines(claude.stdout)

        def say(text: str) -> str:
            claude.stdin.write(json.dumps({"type": "user", "message": {"role": "user", "content": text}}) + "\n")
            claude.stdin.flush()
            while True:
                line = replies.get(deadline)
                if not line:
                    raise RuntimeError("claude exited")
                m = json.loads(line)
                if m.get("type") != "result":
                    continue
                for k in usage:
                    usage[k] += (m.get("usage") or {}).get(k, 0) or 0
                u = m.get("usage") or {}
                requests.append([u.get(k, 0) or 0 for k in (
                    "input_tokens", "cache_read_input_tokens",
                    "cache_creation_input_tokens", "output_tokens")])
                out["total_cost_usd"] = m.get("total_cost_usd") or out["total_cost_usd"]
                out["num_turns"] += 1
                if m.get("is_error"):
                    raise RuntimeError(f"model error: {str(m.get('result'))[:300]}")
                return m.get("result") or ""

        reply = say(prompt)
        for _ in range(max_steps):
            obj = _first_json(reply)
            if obj and "answer" in obj:
                out["result"] = f"ANSWER: {obj['answer']}"
                break
            if obj and "tool" in obj:
                if obj["tool"] not in allowed:
                    reply = say(f"Unknown tool {obj['tool']!r}. Use one of the listed tools.")
                    continue
                text = mcp.call(obj["tool"], obj.get("arguments") or {}, deadline)
                out["tool_calls"] += 1
                out["largest_tool_result_chars"] = max(out["largest_tool_result_chars"], len(text))
                reply = say(f"Tool result:\n{text}")
            else:
                reply = say("Reply with exactly one JSON object, as described.")
        else:
            out["is_error"] = True
            out["result"] = f"gave up after {max_steps} steps"
    except queue.Empty:
        out["is_error"], out["result"] = True, f"TIMEOUT after {timeout}s"
    except Exception as e:  # report, don't crash the whole eval
        out["is_error"], out["result"] = True, f"loop error: {e}"
    finally:
        if claude:
            try:
                claude.stdin.close()
                claude.wait(timeout=20)
            except Exception:
                claude.kill()
        mcp.close()
        shutil.rmtree(work, ignore_errors=True)
    out["usage"] = usage
    out["requests"] = requests
    return out
