"""Turn `meiose record` sessions into handler fixtures.

usage:
  python scripts/extract_fixtures.py [RECORDING.jsonl ...] [--out tests/fixtures/local/playwright]

With no files, reads every recording in the default recordings directory
(%LOCALAPPDATA%\\meiose\\recordings on Windows, ~/.local/share/meiose/recordings
on Linux, ~/Library/Application Support/meiose/recordings on macOS).

Every tools/call result whose text contains a Playwright snapshot is written to
<out>/<recording>-<request id>.txt. The default output directory is git-ignored
because your own browsing may include private pages: review before moving any
file into tests/fixtures/playwright/.
"""

import argparse
import json
import os
import platform
import re
import sys
from pathlib import Path


def default_recordings() -> Path:
    if os.environ.get("MEIOSE_DATA_DIR"):
        return Path(os.environ["MEIOSE_DATA_DIR"]) / "recordings"
    system = platform.system()
    if system == "Windows":
        return Path(os.environ["LOCALAPPDATA"]) / "meiose" / "recordings"
    if system == "Darwin":
        return Path.home() / "Library" / "Application Support" / "meiose" / "recordings"
    return Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local" / "share")) / "meiose" / "recordings"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("files", nargs="*", type=Path)
    ap.add_argument("--out", type=Path, default=Path("tests/fixtures/local/playwright"))
    a = ap.parse_args()
    files = a.files or sorted(default_recordings().glob("*.jsonl"))
    a.out.mkdir(parents=True, exist_ok=True)
    written = 0
    for f in files:
        pending = {}  # request id -> tool name (client→server only)
        for line in f.open(encoding="utf-8"):
            e = json.loads(line)
            if e.get("dir") not in ("c2s", "s2c"):
                continue
            try:
                m = json.loads(e["raw"])
            except json.JSONDecodeError:
                continue
            if e["dir"] == "c2s" and m.get("method") == "tools/call":
                pending[json.dumps(m.get("id"))] = m.get("params", {}).get("name", "?")
            elif e["dir"] == "s2c" and "result" in m:
                key = json.dumps(m.get("id"))
                if key not in pending:
                    continue
                pending.pop(key)
                for c in m["result"].get("content", []):
                    text = c.get("text", "")
                    if c.get("type") == "text" and "### Snapshot\n```yaml\n" in text:
                        rid = re.sub(r"[^A-Za-z0-9]+", "-", str(m.get("id")))
                        path = a.out / f"{f.stem}-{rid}.txt"
                        path.write_bytes(text.encode("utf-8"))
                        written += 1
    print(f"{written} snapshot fixtures from {len(files)} recordings → {a.out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
