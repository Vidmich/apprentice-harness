"""Command line entry point: `apprentice-ml`."""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import asdict

from apprentice_ml import __version__, traces


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="apprentice-ml", description="apprentice-harness ML tooling"
    )
    parser.add_argument("--version", action="version", version=f"apprentice-ml {__version__}")
    sub = parser.add_subparsers(dest="command")

    traces_cmd = sub.add_parser("traces", help="inspect a harness trace store or bundle")
    traces_sub = traces_cmd.add_subparsers(dest="traces_command", required=True)
    stats_cmd = traces_sub.add_parser(
        "stats",
        help="print counts per table and event kind of a traces.sqlite or a trace bundle",
    )
    stats_cmd.add_argument(
        "path",
        nargs="?",
        help="traces.sqlite, a bundle directory or a bundle .tar.zst",
    )
    stats_cmd.add_argument("--db", help="path to traces.sqlite (same as the positional)")
    stats_cmd.add_argument("--json", action="store_true", help="machine-readable output")
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    if args.command is None:
        parser.print_help()
        return 0
    if args.command == "traces" and args.traces_command == "stats":
        path = args.path or args.db
        if path is None:
            parser.error("traces stats needs a path (traces.sqlite or a bundle)")
        try:
            s = traces.stats_of(path)
        except (FileNotFoundError, ValueError, ImportError) as e:
            print(f"error: {e}", file=sys.stderr)
            return 2
        print(json.dumps(asdict(s), indent=2) if args.json else traces.format_stats(s))
        return 0
    parser.error(f"unknown command {args.command}")
    return 2


if __name__ == "__main__":
    sys.exit(main())
