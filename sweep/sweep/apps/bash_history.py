"""Interactively choose entry from bash history
"""
# pyright: strict
from __future__ import annotations
from datetime import datetime
from pathlib import Path
import argparse
import asyncio
import re
import shlex
from typing import Any, Dict, Iterable, List, Optional, Tuple
from .. import Icon, sweep, Candidate
from . import sweep_default_cmd

BASH_HISTORY_FILE = "~/.bash_history"
DATE_RE = re.compile(r"^#(\d+)$")
TERM_ICON = Icon(
    path="M20,19V7H4V19H20M20,3A2,2 0 0,1 22,5V19A2,2 0 0,1 20,21H4"
    "A2,2 0 0,1 2,19V5C2,3.89 2.9,3 4,3H20M13,17V15H18V17H13M9.58,13"
    "L5.57,9H8.4L11.7,12.3C12.09,12.69 12.09,13.33 11.7,13.72L8.42,17"
    "H5.59L9.58,13Z",
    view_box=(0, 0, 24, 24),
    size=(1, 3),
    fallback=" ",
)


def history(history_file: Optional[str] = None) -> Iterable[Tuple[datetime, str]]:
    """List all bash history entries"""
    if history_file is None:
        history_file = BASH_HISTORY_FILE
    entries: Dict[str, datetime] = {}
    entry: List[str] = []
    with Path(history_file).expanduser().resolve().open() as file:
        date = None
        for line in file:
            match = DATE_RE.match(line)
            if match is None:
                entry.append(line)
            else:
                if date is not None:
                    entries["".join(entry).strip()] = date
                date = datetime.fromtimestamp(int(match.group(1)))
                entry.clear()
        if date is not None:
            entries["".join(entry).strip()] = date
    return sorted(
        ((d, e) for e, d in entries.items()), key=lambda e: e[0], reverse=True
    )


async def main(args: Optional[List[str]] = None) -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--theme", help="sweep theme")
    parser.add_argument("--sweep", help="path to the sweep command")
    parser.add_argument("--tty", help="path to the tty")
    parser.add_argument("--query", help="initial query")
    parser.add_argument(
        "--history-file", default=BASH_HISTORY_FILE, help="path to history file"
    )
    opts = parser.parse_args(args)

    candidates: List[Any] = []
    for date, entry in history(opts.history_file):
        candidates.append(
            Candidate()
            .target_push(entry)
            .right_push(date.strftime(" %F %T"), active=False)
            .extra_update(item=entry)
        )

    items = await sweep(
        candidates,
        sweep=shlex.split(opts.sweep) if opts.sweep else sweep_default_cmd(),
        prompt="HISTORY",
        prompt_icon=TERM_ICON,
        query=opts.query,
        theme=opts.theme,
        title="command history",
        keep_order=True,
        scorer="substr",
        tty=opts.tty,
    )

    for item in items:
        print(item.extra.get("item", ""), end="")


if __name__ == "__main__":
    asyncio.run(main())
