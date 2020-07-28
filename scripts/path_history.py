#!/usr/bin/env python3
from datetime import datetime
from pathlib import Path
import argparse
import fcntl
import io
import os
import sweep_rpc as rpc
import time


PATH_HISTORY_FILE = "~/.path_history"
DEFAULT_IGNORE = {".git", ".hg", "__pycache__", ".DS_Store", ".mypy_cache", "target"}


class PathHistory:
    """Access and modify fpath history"""

    def __init__(self, history_path=PATH_HISTORY_FILE):
        self.history_path = Path(history_path).expanduser().resolve()

    def load(self):
        """Load path history"""
        if not self.history_path.exists():
            return None, {}
        with self.history_path.open("r") as file:
            try:
                fcntl.lockf(file, fcntl.LOCK_SH)
                content = io.StringIO(file.read())
            finally:
                fcntl.lockf(file, fcntl.LOCK_UN)

        mtime = int(content.readline().strip() or "0")
        paths = {}
        for line in content:
            count, timestamp, path = line.split("\t")
            count = int(count)
            date = datetime.fromtimestamp(int(timestamp))
            paths[Path(path.strip("\n"))] = (count, date)
        return mtime, paths

    def add(self, path):
        """AddTo/Update path history"""
        path = Path(path).expanduser().resolve()
        if not path.exists():
            return

        while True:
            mtime, paths = self.load()
            count, _ = paths.get(path) or (0, datetime.now())
            count += 1
            paths[path] = (count, datetime.now())

            content = io.StringIO()
            content.write("{}\n".format(int(time.time())))
            for path, (count, date) in paths.items():
                content.write("{}\t{}\t{}\n".format(count, int(date.timestamp()), path))

            with self.history_path.open("a+") as file:
                try:
                    fcntl.lockf(file, fcntl.LOCK_EX)
                    # check if file was modified after loading
                    file.seek(0)
                    mtime_now = int(file.readline().strip() or "0")
                    if mtime_now != mtime:
                        continue
                    file.seek(0)
                    file.truncate(0)
                    file.write(content.getvalue())
                    return
                finally:
                    fcntl.lockf(file, fcntl.LOCK_UN)


def collapse_path(path):
    home = Path.home().parts
    parts = path.parts
    if home == parts[: len(home)]:
        parts = ("~", *parts[len(home) :])
    if len(parts) > 5:
        parts = (parts[0], "\u2026") + parts[-4:]
    return Path().joinpath(*parts)


def candidates_from_path(root, depth=3):
    def walk(path, depth):
        if depth < 0:
            return
        if not path.is_dir():
            return
        for item in path.iterdir():
            if item.name in DEFAULT_IGNORE:
                continue
            candidates.append(str(item.relative_to(root)))
            walk(item, depth - 1)

    root = Path(root)
    if not root.is_dir():
        return []
    candidates = []
    walk(root, depth)
    return candidates


def main():
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    parser_add = subparsers.add_parser("add")
    parser_add.add_argument("path", nargs="?")
    subparsers.add_parser("list")
    parser_select = subparsers.add_parser("select")
    parser_select.add_argument("--theme")
    opts = parser.parse_args()

    path_history = PathHistory()

    if opts.command == "add":
        path = opts.path or os.getcwd()
        path_history.add(path)

    elif opts.command == "list":
        _, paths = path_history.load()
        items = []
        for path, (count, date) in paths.items():
            items.append([count, date, path])
        items.sort(reverse=True)
        for count, date, path in items:
            print("{:<5} {} {}".format(count, date.strftime("[%F %T]"), path))

    elif opts.command == "select":
        _, paths = path_history.load()
        items = []
        for path, (count, date) in paths.items():
            items.append([count, date, path])
        items.sort(reverse=True)

        result = None
        tab = "ctrl+i"
        backspace = "backspace"
        with rpc.Sweep(
            sweep=["cargo", "run", "--"], prompt="PATH HISTORY", theme=opts.theme
        ) as sweep:
            sweep.candidates_extend([str(item[2]) for item in items])
            sweep.key_binding(tab, tab)
            sweep.key_binding(backspace, backspace)

            def load_path(path):
                candidates = candidates_from_path(current_path)
                if candidates:
                    sweep.prompt_set(str(collapse_path(current_path)))
                    sweep.niddle_set("")
                    sweep.candidates_clear()
                    sweep.candidates_extend(candidates)

            current_path = None
            while True:
                msg = sweep.poll()
                if msg is None:
                    return
                msg_type, value = msg
                if msg_type == rpc.SWEEP_SELECTED:
                    result = value
                    break
                if msg_type == rpc.SWEEP_KEYBINDING:
                    if value == tab:
                        sweep.current()
                    elif value == backspace:
                        if current_path is not None:
                            current_path = current_path.parent
                            load_path(current_path)
                elif msg_type == rpc.SWEEP_CURRENT:
                    if value is None:
                        continue
                    if current_path is None:
                        current_path = Path(value)
                    else:
                        current_path /= value
                    load_path(current_path)

        if result is not None:
            print(result)


if __name__ == "__main__":
    main()
