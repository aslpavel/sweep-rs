#!/usr/bin/env python3
import argparse
import re
from sweep_rpc import Sweep, SWEEP_SELECTED
from datetime import datetime
from pathlib import Path

BASH_HISTORY_FILE = Path("~/.bash_history").expanduser().resolve()
SPLITTER_RE = re.compile("#(?P<date>\\d+)\n(?P<entry>[^#]+)\n")


def history():
    """List all bash history entries
    """
    text = BASH_HISTORY_FILE.read_text()
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
    parser.add_argument("--theme")
    opts = parser.parse_args()

    result = None
    with Sweep(nth="2..", prompt="HISTORY", theme=opts.theme) as sweep:
        candidates = ["{} {}".format(d.strftime("[%F %T]"), e) for d, e in history()]
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
        print(entry)


if __name__ == "__main__":
    main()
