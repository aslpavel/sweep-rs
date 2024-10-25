"""Run sweep inside a newly create kitty window"""

# pyright: strict
from __future__ import annotations
import argparse
import asyncio
import json
import sys
import shlex
from typing import Any
from .. import sweep
from . import sweep_default_cmd


async def main(args: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--theme", help="sweep theme")
    parser.add_argument("--sweep", help="path to the sweep command")
    parser.add_argument("--tty", help="path to the tty")
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
    parser.add_argument(
        "--input", type=argparse.FileType("r"), help="read input from a file"
    )
    opts = parser.parse_args(args)

    candidates: list[Any]
    input = sys.stdin if opts.input is None else opts.input
    if opts.json:
        candidates = json.load(input)
    else:
        candidates = []
        for line in input:
            candidates.append(line.strip())

    kitty_args = ["kitty", "--title", opts.title or "sweep-menu"]
    kitty_class = getattr(opts, "class", None)
    if kitty_class:
        kitty_args.extend(["--class", kitty_class])

    result = await sweep(
        candidates,
        sweep=[
            *kitty_args,
            *(shlex.split(opts.sweep) if opts.sweep else sweep_default_cmd()),
        ],
        tty=opts.tty,
        theme=opts.theme,
        prompt=opts.prompt,
        prompt_icon=opts.prompt_icon,
        nth=opts.nth,
        delimiter=opts.delimiter,
        scorer=opts.scorer,
        keep_order=opts.keep_order,
        no_match=opts.no_match,
        tmp_socket=True,
        layout="full",
    )

    if opts.json:
        json.dump(result, sys.stdout)
    else:
        for item in result:
            print(item)


if __name__ == "__main__":
    asyncio.run(main())
