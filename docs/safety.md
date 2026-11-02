# Safety

These properties are tested on every commit on macOS, Linux and Windows.

## Guarantees

- Interactive elements are never removed. Every link, button, input,
  checkbox, option or element with `[cursor=pointer]` keeps its `[ref=…]`.
  This is checked on 40 real pages, on generated trees and on random page
  fragments, with a classifier written independently of the trimmer.
- A trimmed output is always smaller than the original and says how to get
  the original back.
- `meiose_expand` returns the server's result byte-for-byte.
- meiose fails open. If trimming fails or panics, if the store cannot be
  opened, or if the agent never saw `meiose_expand`, the original passes
  through unchanged.
- The proxy does not hang on slow servers, crashing servers, 5 MB messages,
  malformed JSON or out-of-order responses.
- The server and everything it started stop with meiose. On Windows the
  server runs in a Job Object that is killed when meiose exits, even if
  meiose itself is killed. On Unix the server leads its own process group.
- meiose makes no network calls and sends no telemetry. It never writes logs
  to stdout.

## What is stored on disk

Original tool outputs (what pages or tools returned) are kept in SQLite in
your data directory for 7 days, so `meiose_expand` and `meiose log` work. Set
`MEIOSE_KEEP_DAYS` to change that; `MEIOSE_KEEP_DAYS=0` keeps originals in
memory for the session only.

`meiose install` keeps backups of the config files it changes in the same
directory. Those are full copies, including any `env` values (API keys).

`meiose record` writes every message to a JSONL file, including anything typed
into forms. Use it for debugging only.

Data directory: `%LOCALAPPDATA%\meiose` on Windows,
`~/Library/Application Support/meiose` on macOS, `~/.local/share/meiose` on
Linux, or `MEIOSE_DATA_DIR` if set.
