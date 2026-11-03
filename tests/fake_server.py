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
    """A scripted MCP server. Tools (arguments in braces):

      browser_snapshot {checked?}   the fixture; `checked` flips one checkbox
      huge_snapshot                 the fixture's tree repeated to ~5 MB
      get_rows                      ~150 KB JSON array
      small                         tiny text
      fails                         JSON-RPC error response
      collide                       sends a server->client request reusing the
                                    client's id, then answers
      echo {text, delay_ms?}        returns `text` after a delay, answered on
                                    its own thread (so responses interleave)
      notify {count}                `count` progress notifications, then answers
      garbage                       a malformed line, then answers
      split                         writes its response in two parts, 300 ms apart
      hostile                       the fixture with terminal escapes (OSC 52
                                    clipboard write, colours, a bell) in its text
      big_text {kb}                 plain text of about `kb` KiB
      crash                         exits immediately with code 3
      spawn_grandchild              starts a long-lived grandchild (pid written
                                    to $GRANDCHILD_PID) and from then on ignores
                                    stdin EOF, so the proxy must kill the tree

    Every line written goes to $FAKE_LOG, every line received to $FAKE_IN_LOG.
    Malformed client lines are logged and skipped. Any meiose_expand that
    reaches us is a proxy bug: answered with an error saying so.
    """
    import json
    import os
    import subprocess
    import threading
    import time

    log_path = os.environ.get("FAKE_LOG")
    in_log_path = os.environ.get("FAKE_IN_LOG")
    snapshot = open(fixture, encoding="utf-8").read()
    lock = threading.Lock()
    hang_on_eof = False

    def write_raw(data: bytes):
        # Log first: a test may look a line up as soon as it receives it.
        with lock:
            if log_path:
                with open(log_path, "ab") as f:
                    f.write(data)
            stdout.write(data)
            stdout.flush()

    def send(obj):
        write_raw(json.dumps(obj, ensure_ascii=False).encode("utf-8") + b"\n")

    def result(rid, res):
        send({"jsonrpc": "2.0", "id": rid, "result": res})

    def text_result(rid, text):
        result(rid, {"content": [{"type": "text", "text": text}]})

    def huge():
        head, rest = snapshot.split("```yaml\n", 1)
        body, tail = rest.split("\n```", 1)
        reps = 5 * 1024 * 1024 // max(len(body), 1) + 1
        return head + "```yaml\n" + "\n".join([body] * reps) + "\n```" + tail

    def delayed_echo(rid, text, delay):
        time.sleep(delay)
        text_result(rid, text)

    while True:
        line = stdin.readline()
        if not line:
            if hang_on_eof:
                while True:
                    time.sleep(60)
            return 0
        if in_log_path:
            with open(in_log_path, "ab") as f:
                f.write(line)
        try:
            msg = json.loads(line)
        except ValueError:
            sys.stderr.buffer.write(b"fake: skipped malformed line\n")
            sys.stderr.buffer.flush()
            continue
        if not isinstance(msg, dict):
            continue  # batches and other shapes: logged above, otherwise ignored
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
                text_result(rid, text)
            elif name == "huge_snapshot":
                text_result(rid, huge())
            elif name == "get_rows":
                rows = [{"id": i, "name": f"user{i}", "email": f"user{i}@example.com",
                         "bio": "lorem ipsum " * 8} for i in range(1000)]
                text_result(rid, json.dumps(rows))
            elif name == "small":
                text_result(rid, "ok")
            elif name == "fails":
                send({"jsonrpc": "2.0", "id": rid, "error": {"code": -32000, "message": "boom"}})
            elif name == "collide":
                send({"jsonrpc": "2.0", "id": rid, "method": "roots/list"})
                text_result(rid, snapshot)
            elif name == "echo":
                threading.Thread(target=delayed_echo, daemon=True,
                                 args=(rid, args.get("text", ""), args.get("delay_ms", 0) / 1000)).start()
            elif name == "notify":
                for i in range(args.get("count", 3)):
                    send({"jsonrpc": "2.0", "method": "notifications/progress",
                          "params": {"progressToken": rid, "progress": i, "total": args.get("count", 3)}})
                text_result(rid, "notified")
            elif name == "hostile":
                evil = "\x1b]52;c;ZXZpbA==\x07\x1b[31mred\x1b[0m\r\x9b2J\x07"
                text_result(rid, evil + snapshot)
            elif name == "big_text":
                line = "the quick brown fox jumps over the lazy dog 0123456789\n"
                text_result(rid, line * (int(args.get("kb", 1)) * 1024 // len(line) + 1))
            elif name == "garbage":
                write_raw(b"{this is not json\n")
                text_result(rid, "after garbage")
            elif name == "split":
                data = json.dumps({"jsonrpc": "2.0", "id": rid,
                                   "result": {"content": [{"type": "text", "text": "split ok"}]}}).encode() + b"\n"
                with lock:
                    if log_path:
                        with open(log_path, "ab") as f:
                            f.write(data)
                    stdout.write(data[:20])
                    stdout.flush()
                    time.sleep(0.3)
                    stdout.write(data[20:])
                    stdout.flush()
            elif name == "crash":
                os._exit(3)
            elif name == "spawn_grandchild":
                # Null stdio: if it ever survives, it must not hold the test's
                # pipes open (that would turn a test failure into a hang).
                gc = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(600)"],
                                      stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                      stderr=subprocess.DEVNULL)
                with open(os.environ["GRANDCHILD_PID"], "w") as f:
                    f.write(str(gc.pid))
                hang_on_eof = True
                text_result(rid, f"grandchild {gc.pid}")
            else:
                send({"jsonrpc": "2.0", "id": rid,
                      "error": {"code": -32602, "message": f"fake server got unexpected tool {name}"}})
        elif rid is not None:
            send({"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": "unknown method"}})


if __name__ == "__main__":
    sys.exit(main())
