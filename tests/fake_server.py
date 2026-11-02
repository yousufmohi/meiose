"""A misbehaving-on-demand MCP server for meiose's protocol tests.

Usage: python fake_server.py <mode> [arg]

Modes (M1; more arrive with the M5 protocol suite):
  mirror      echo every stdin line back to stdout byte-for-byte; log to stderr
  exit N      read one line, then exit with code N
  burst N     write N lines to stdout immediately, then exit 0 (stdin untouched)
  hang        ignore stdin EOF and never exit (the proxy must kill us)

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

    if mode == "hang":
        while stdin.readline():
            pass
        while True:
            time.sleep(60)

    stderr.write(b"fake: unknown mode %s\n" % mode.encode())
    return 2


if __name__ == "__main__":
    sys.exit(main())
