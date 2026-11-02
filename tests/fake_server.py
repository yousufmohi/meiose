"""A misbehaving-on-demand MCP server for meiose's protocol tests.

Usage: python fake_server.py <mode> [arg]

Modes (M1; more arrive with the M5 protocol suite):
  mirror      echo every stdin line back to stdout byte-for-byte; log to stderr
  exit N      read one line, then exit with code N
  burst N     write N lines to stdout immediately, then exit 0 (stdin untouched)
  hang        ignore stdin EOF and never exit (the proxy must kill us)
  mcp FIXTURE a small scripted MCP server (see mcp() below); every line it
              writes is also appended to $FAKE_LOG if set

All I/O is binary so Windows never translates newlines.
"""

import sys
import time


def main() -> int:
    mode = sys.argv[1] if len(sys.argv) > 1 else "mirror"
    stdin = sys.stdin.buffer
    stdout = sys.stdout.buffer
    stderr = sys.stderr.buffer

    if mode == "mirror":
        count = 0
        while True:
            line = stdin.readline()
            if not line:
                stderr.write(b"fake: stdin closed after %d lines\n" % count)
                stderr.flush()
                return 0
            count += 1
            stdout.write(line)
            stdout.flush()
            stderr.write(b"fake: mirrored %d bytes\n" % len(line))
            stderr.flush()

    if mode == "exit":
        stdin.readline()
        return int(sys.argv[2])

    if mode == "burst":
        for i in range(int(sys.argv[2])):
            stdout.write(b'{"jsonrpc":"2.0","method":"notifications/message","params":{"n":%d}}\n' % i)
        stdout.flush()
        return 0

    if mode == "mcp":
        return mcp(sys.argv[2], stdin, stdout)

    if mode == "hang":
        while stdin.readline():
            pass
        while True:
            time.sleep(60)

    stderr.write(b"fake: unknown mode %s\n" % mode.encode())
    return 2


TOOLS_PAGE_1 = [
    {"name": "browser_snapshot", "description": "snapshot", "inputSchema": {"type": "object"}},
    {"name": "get_rows", "description": "rows", "inputSchema": {"type": "object"}},
]
TOOLS_PAGE_2 = [
    {"name": "small", "description": "small text", "inputSchema": {"type": "object"}},
    {"name": "fails", "description": "json-rpc error", "inputSchema": {"type": "object"}},
]


def mcp(fixture: str, stdin, stdout) -> int:
    """Tools:
      browser_snapshot {checked?: bool}  the fixture text; `checked` flips one
                                         checkbox line, for diff tests
      get_rows                           ~150 KB JSON array
      small                              tiny text
      fails                              JSON-RPC error response
      collide                            first sends a server→client request
                                         reusing the client's id, then answers
    Any meiose_expand call reaching us is a bug in the proxy: we answer with a
    JSON-RPC error saying so.
    """
    import json
    import os

    log_path = os.environ.get("FAKE_LOG")
    snapshot = open(fixture, encoding="utf-8").read()

    def send(obj):
        line = json.dumps(obj, ensure_ascii=False).encode("utf-8") + b"\n"
        stdout.write(line)
        stdout.flush()
        if log_path:
            with open(log_path, "ab") as f:
                f.write(line)

    def result(rid, res):
        send({"jsonrpc": "2.0", "id": rid, "result": res})

    while True:
        line = stdin.readline()
        if not line:
            return 0
        msg = json.loads(line)
        rid, method = msg.get("id"), msg.get("method")
        params = msg.get("params") or {}
        if method is None:
            continue  # a response to our own request (collide)
        if method == "initialize":
            result(rid, {"protocolVersion": "2025-11-25", "capabilities": {"tools": {}},
                         "serverInfo": {"name": "fake", "version": "0"}})
        elif method == "tools/list":
            if params.get("cursor"):
                result(rid, {"tools": TOOLS_PAGE_2})
            else:
                result(rid, {"tools": TOOLS_PAGE_1, "nextCursor": "page2"})
        elif method == "tools/call":
            name = params.get("name")
            args = params.get("arguments") or {}
            if name == "browser_snapshot":
                text = snapshot
                if args.get("checked"):
                    text = text.replace("- checkbox [ref=e5]", "- checkbox [checked] [ref=e5]", 1)
                result(rid, {"content": [{"type": "text", "text": text}]})
            elif name == "get_rows":
                rows = [{"id": i, "name": f"user{i}", "email": f"user{i}@example.com",
                         "bio": "lorem ipsum " * 8} for i in range(1000)]
                result(rid, {"content": [{"type": "text", "text": json.dumps(rows)}]})
            elif name == "small":
                result(rid, {"content": [{"type": "text", "text": "ok"}]})
            elif name == "fails":
                send({"jsonrpc": "2.0", "id": rid, "error": {"code": -32000, "message": "boom"}})
            elif name == "collide":  # tools list does not advertise it; tests call it directly
                send({"jsonrpc": "2.0", "id": rid, "method": "roots/list"})
                result(rid, {"content": [{"type": "text", "text": snapshot}]})
            else:
                send({"jsonrpc": "2.0", "id": rid,
                      "error": {"code": -32602, "message": f"fake server got unexpected tool {name}"}})
        elif rid is not None:
            send({"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": "unknown method"}})


if __name__ == "__main__":
    sys.exit(main())
