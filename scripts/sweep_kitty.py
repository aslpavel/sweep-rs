#!/usr/bin/env python3
"""Run sweep inside a newly create kitty window
"""
from pathlib import Path
import argparse
import asyncio
import json
import sys

sys.path.insert(0, str(Path(__file__).expanduser().resolve().parent))
from sweep import sweep


async def main():
    parser = argparse.ArgumentParser(description="Run sweep inside a newly create kitty window")
    parser.add_argument(
        "-p",
        "--prompt",
        default="INPUT",
        help="override prompt string",
    )
    parser.add_argument(
        "--nth",
        help="comma-seprated list of fields for limiting search",
    )
    parser.add_argument("--delimiter", help="filed delimiter")
    parser.add_argument("--theme", help="theme as a list of comma separated attributes")
    parser.add_argument("--scorer", help="default scorer")
    parser.add_argument(
        "--json",
        action="store_true",
        help="expect candidates in JSON format",
    )
    parser.add_argument(
        "--no-match",
        choices=["nothing", "input"],
        help="what is returned if there is no match on enter",
    )
    parser.add_argument(
        "--keep-order",
        help="keep order of elements (do not use ranking score)",
    )
    args = parser.parse_args()

    if args.json:
        candidates = json.load(sys.stdin)
    else:
        candidates = []
        for line in sys.stdin:
            candidates.append(line.strip())

    result = await sweep(
        candidates,
        sweep=["kitty", "--title", "sweep-menu", "sweep"],
        prompt=args.prompt,
        nth=args.nth,
        height=1024,
        delimiter=args.delimiter,
        theme=args.theme,
        scorer=args.scorer,
        keep_order=args.keep_order,
        no_match=args.no_match,
        altscreen=True,
    )

    if args.json:
        json.dump(result, sys.stdout)
    else:
        print(result)


if __name__ == "__main__":
    asyncio.run(main())
