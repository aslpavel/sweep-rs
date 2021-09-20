#!/usr/bin/env python3
"""Interactively choose entry from bash history
"""
from datetime import datetime
from pathlib import Path
import argparse
import asyncio
import re
import sys
from typing import Dict, Iterable, List, Optional, Tuple

sys.path.insert(0, str(Path(__file__).expanduser().resolve().parent))
from sweep import Candidate, sweep

BASH_HISTORY_FILE = "~/.bash_history"
DATE_RE = re.compile(r"^#(\d+)$")


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


async def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--theme", help="sweep theme (see sweep help)")
    parser.add_argument(
        "--history-file", default=BASH_HISTORY_FILE, help="path to history file"
    )
    parser.add_argument("--sweep", default="sweep", help="path to the sweep command")
    parser.add_argument("--tty", help="path to the tty")
    opts = parser.parse_args()

    candidates: List[Candidate] = []
    for date, entry in history(opts.history_file):
        candidates.append(
            {
                "entry": [[date.strftime("[%F %T] "), False], entry],
                "item": entry,
            }
        )

    result = await sweep(
        candidates,
        sweep=[opts.sweep],
        prompt="󰆍  HISTORY",
        theme=opts.theme,
        title="command history",
        keep_order=True,
        scorer="substr",
        tty=opts.tty,
    )

    if result is not None:
        print(result["item"], end="")


if __name__ == "__main__":
    asyncio.run(main())
