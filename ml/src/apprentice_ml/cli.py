"""Command line entry point: `apprentice-ml`."""

from __future__ import annotations

import argparse
import sys

from apprentice_ml import __version__


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="apprentice-ml", description="apprentice-harness ML tooling"
    )
    parser.add_argument("--version", action="version", version=f"apprentice-ml {__version__}")
    parser.add_subparsers(dest="command", help="subcommands are added by later milestones")
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    if args.command is None:
        parser.print_help()
        return 0
    return 0


if __name__ == "__main__":
    sys.exit(main())
