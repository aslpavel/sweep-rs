#!/usr/bin/env python3
"""Run sweep inside a newly create kitty window
"""
from __future__ import annotations
from pathlib import Path
import argparse
import asyncio
import json
import sys
from typing import Any, List

sys.path.insert(0, str(Path(__file__).expanduser().resolve().parent))
from sweep import sweep


async def main() -> None:
    parser = argparse.ArgumentParser(
        description="Run sweep inside a newly create kitty window"
    )
    parser.add_argument("--class", help="kitty window class/app_id")
    parser.add_argument("--title", help="kitty window title")
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
    parser.add_argument("--sweep", default="sweep", help="sweep binary")
    args = parser.parse_args()

    candidates: List[Any]
    if args.json:
        candidates = json.load(sys.stdin)
    else:
        candidates = []
        for line in sys.stdin:
            candidates.append(line.strip())

    kitty_args = ["kitty", "--title", args.title or "sweep-menu"]
    kitty_class = getattr(args, "class", None)
    if kitty_class:
        kitty_args.extend(["--class", kitty_class])

    result = await sweep(
        candidates,
        sweep=[*kitty_args, args.sweep],
        prompt=args.prompt,
        nth=args.nth,
        height=1024,
        delimiter=args.delimiter,
        theme=args.theme,
        scorer=args.scorer,
        keep_order=args.keep_order,
        no_match=args.no_match,
        altscreen=True,
        tmp_socket=True,
        border=0
    )

    if args.json:
        json.dump(result, sys.stdout)
    else:
        print(result)


if __name__ == "__main__":
    asyncio.run(main())
