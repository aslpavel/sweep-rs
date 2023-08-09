#!/usr/bin/env python3
"""Run sweep inside a newly create kitty window
"""
from __future__ import annotations
import argparse
import asyncio
import json
import sys
from typing import Any, List, Optional
from ..sweep import sweep


async def main(args: Optional[List[str]] = None) -> None:
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
        "--prompt-icon",
        default=None,
        help="set prompt icon",
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
    opts = parser.parse_args(args)

    candidates: List[Any]
    if opts.json:
        candidates = json.load(sys.stdin)
    else:
        candidates = []
        for line in sys.stdin:
            candidates.append(line.strip())

    kitty_args = ["kitty", "--title", opts.title or "sweep-menu"]
    kitty_class = getattr(opts, "class", None)
    if kitty_class:
        kitty_args.extend(["--class", kitty_class])

    result = await sweep(
        candidates,
        sweep=[*kitty_args, opts.sweep],
        prompt=opts.prompt,
        prompt_icon=opts.prompt_icon,
        nth=opts.nth,
        height=1024,
        delimiter=opts.delimiter,
        theme=opts.theme,
        scorer=opts.scorer,
        keep_order=opts.keep_order,
        no_match=opts.no_match,
        altscreen=True,
        tmp_socket=True,
        border=0,
    )

    if opts.json:
        json.dump(result, sys.stdout)
    else:
        print(result)


if __name__ == "__main__":
    asyncio.run(main())
