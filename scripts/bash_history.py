#!/usr/bin/env python3
"""Interactively choose entry from bash history
"""
from datetime import datetime
from pathlib import Path
import argparse
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.realpath(__file__)))
from sweep_rpc import Sweep, SWEEP_SELECTED

BASH_HISTORY_FILE = "~/.bash_history"
SPLITTER_RE = re.compile("#(?P<date>\\d+)\n(?P<entry>([^#][^\n]+\n)+)")


def history(history_file=None):
    """List all bash history entries
    """
    if history_file is None:
        history_file = BASH_HISTORY_FILE
    text = Path(history_file).expanduser().resolve().read_text()
    unique = set()
    entries = []
    for entry in SPLITTER_RE.finditer(text):
        date = datetime.fromtimestamp(int(entry.group("date")))
        entry = entry.group("entry")
        if entry in unique:
            continue
        unique.add(entry)
        entries.append((date, entry))
    entries.sort(key=lambda e: e[0], reverse=True)
    return entries


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--theme", help="sweep theme (see sweep help)")
    parser.add_argument(
        "--history-file", default=BASH_HISTORY_FILE, help="path to history file"
    )
    opts = parser.parse_args()

    result = None
    with Sweep(
        nth="2..",
        prompt="HISTORY",
        theme=opts.theme,
        title="command history",
        keep_order=True,
    ) as sweep:
        candidates = [
            "{} {}".format(d.strftime("[%F %T]"), e)
            for d, e in history(opts.history_file)
        ]
        sweep.candidates_extend(candidates)

        while True:
            msg = sweep.poll()
            if msg is None:
                break
            msg_type, value = msg
            if msg_type == SWEEP_SELECTED:
                result = value
                break

    if result is not None:
        _0, _1, entry = result.split(maxsplit=2)
        print(entry, end="")


if __name__ == "__main__":
    main()
