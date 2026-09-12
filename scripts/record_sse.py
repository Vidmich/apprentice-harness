"""Record an Anthropic Messages SSE stream as a test fixture.

    uv run --project ml python scripts/record_sse.py NAME REQUEST.json
    uv run --project ml python scripts/record_sse.py NAME --raw captured.txt

The first form spends real tokens: it POSTs REQUEST.json (a Messages API body;
`stream: true` is forced) with `curl -N` using ANTHROPIC_API_KEY, against
ANTHROPIC_BASE_URL when set. The second form only redacts a stream captured
by other means. Either way the result lands in
crates/core/tests/fixtures/sse/NAME.txt, normalised and redacted:

- line endings become LF and the file ends with one blank line;
- message ids become `msg_01REDACTED`, tool-use ids `toolu_01REDACTED`
  (numbered when a stream has several, so parallel tool calls stay distinct);
- thinking signatures become `REDACTED`: they are opaque to the harness and
  a real one is both long and account-bound;
- the `model` and the usage counts are kept as recorded (fixtures should say
  what they were captured from and tests assert on the counts).

Stdlib only; run through `uv run --project ml` so the interpreter is the
project's Python.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from collections.abc import Callable
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FIXTURES = ROOT / "crates" / "core" / "tests" / "fixtures" / "sse"
ANTHROPIC_VERSION = "2023-06-01"


def redact(raw: str) -> str:
    text = raw.replace("\r\n", "\n").replace("\r", "\n")

    def numbered(prefix: str) -> Callable[[re.Match[str]], str]:
        seen: dict[str, str] = {}

        def sub(m: re.Match[str]) -> str:
            ident = m.group(0)
            if ident not in seen:
                n = len(seen) + 1
                seen[ident] = f"{prefix}_{n:02d}REDACTED"
            return seen[ident]

        return sub

    text = re.sub(r"\bmsg_[A-Za-z0-9]{6,}", numbered("msg"), text)
    text = re.sub(r"\btoolu_[A-Za-z0-9]{6,}", numbered("toolu"), text)
    text = re.sub(r'"signature":"[^"]*"', '"signature":"REDACTED"', text)
    return text.rstrip("\n") + "\n\n"


def capture(request: Path) -> str:
    key = os.environ.get("ANTHROPIC_API_KEY")
    if not key:
        sys.exit("record_sse: set ANTHROPIC_API_KEY (this recording spends real tokens)")
    base = os.environ.get("ANTHROPIC_BASE_URL", "https://api.anthropic.com").rstrip("/")
    body = json.loads(request.read_text(encoding="utf-8"))
    body["stream"] = True
    cmd = [
        "curl",
        "-sS",
        "-N",
        "--fail-with-body",
        f"{base}/v1/messages",
        "-H",
        f"x-api-key: {key}",
        "-H",
        f"anthropic-version: {ANTHROPIC_VERSION}",
        "-H",
        "content-type: application/json",
        "--data-binary",
        "@-",
    ]
    proc = subprocess.run(cmd, input=json.dumps(body).encode(), capture_output=True, check=False)
    out = proc.stdout.decode("utf-8", errors="replace")
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr.decode("utf-8", errors="replace"))
        sys.stderr.write(out)
        sys.exit(f"record_sse: curl failed with exit code {proc.returncode}")
    return out


def main() -> None:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("name", help="fixture name (crates/core/tests/fixtures/sse/NAME.txt)")
    src = ap.add_mutually_exclusive_group(required=True)
    src.add_argument("request", nargs="?", type=Path, help="Messages API request body (JSON)")
    src.add_argument(
        "--raw", type=Path, help="already captured stream to redact instead of calling"
    )
    args = ap.parse_args()

    raw = args.raw.read_text(encoding="utf-8") if args.raw else capture(args.request)
    if not raw.lstrip().startswith("event:"):
        sys.stderr.write(raw[:2000] + "\n")
        sys.exit("record_sse: that is not an SSE stream (an error body?); nothing written")

    out = FIXTURES / f"{args.name}.txt"
    out.write_bytes(redact(raw).encode("utf-8"))
    events = re.findall(r"^event: (\S+)", raw, flags=re.M)
    print(f"wrote {out.relative_to(ROOT)} ({len(events)} events: {' '.join(events)})")


if __name__ == "__main__":
    main()
