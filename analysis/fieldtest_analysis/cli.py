"""CLI: ``fieldtest-analyze <test_dir>`` → write/print the per-test summary.

    python -m fieldtest_analysis <test_dir> [-o summary.json]
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from .loader import load_test
from .summary import build_summary


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="fieldtest-analyze")
    ap.add_argument("test_dir", help="aggregated test directory (contains mobile/ and/or server/)")
    ap.add_argument("-o", "--output", help="write summary JSON here (default: stdout)")
    ap.add_argument("--plots", metavar="DIR", help="also write the standard plots (PNG) to DIR")
    args = ap.parse_args(argv)

    td = load_test(args.test_dir)
    summary = build_summary(td)
    text = json.dumps(summary, indent=2, default=str)
    if args.output:
        Path(args.output).write_text(text)
        print(f"wrote {args.output}", file=sys.stderr)
    else:
        print(text)

    if args.plots:
        from .plots import write_all

        for p in write_all(td, args.plots):
            print(f"wrote {p}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
