#!/usr/bin/env python3
"""Simple tool to maintain and navigate visited path history
"""
from collections import deque
from datetime import datetime
from pathlib import Path
import argparse
import fcntl
import inspect
import io
import os
import re
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.realpath(__file__)))
import sweep_rpc as rpc


PATH_HISTORY_FILE = "~/.path_history"
DEFAULT_IGNORE = re.compile(
    "|".join(
        [
            "\\.git",
            "\\.hg",
            "__pycache__",
            "\\.DS_Store",
            "\\.mypy_cache",
            "target",
            ".*\\.elc",
            ".*\\.pyo",
            ".*\\.pyc",
        ]
    )
)


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
            date = int(timestamp)
            paths[Path(path.strip("\n"))] = (count, date)
        return mtime, paths

    def update(self, update):
        """AddTo/Update path history"""
        while True:
            now = int(time.time())
            mtime_last, paths = self.load()
            if not update(mtime_last, now, paths):
                return

            content = io.StringIO()
            content.write("{}\n".format(now))
            for path, (count, date) in paths.items():
                content.write("{}\t{}\t{}\n".format(count, date, path))

            with self.history_path.open("a+") as file:
                try:
                    fcntl.lockf(file, fcntl.LOCK_EX)
                    # check if file was modified after loading
                    file.seek(0)
                    mtime_now = int(file.readline().strip() or "0")
                    if mtime_now != mtime_last:
                        continue
                    file.seek(0)
                    file.truncate(0)
                    file.write(content.getvalue())
                    return
                finally:
                    fcntl.lockf(file, fcntl.LOCK_UN)

    def add(self, path):
        """Add/Update path in the history"""

        def update_add(mtime_last, now, paths):
            count, update_last = paths.get(path) or (0, now)
            if mtime_last == update_last:
                # last update was for the same path, do not update
                return False
            count += 1
            paths[path] = (count, now)
            return True

        path = Path(path).expanduser().resolve()
        if not path.exists():
            return
        self.update(update_add)

    def cleanup(self):
        """Remove paths from the history which no longre exist"""

        def update_cleanup(_mtime_last, _now, paths):
            updated = False
            for path in list(paths.keys()):
                exists = False
                try:
                    exists = Path(path).exists()
                except PermissionError:
                    pass
                if not exists:
                    del paths[path]
                    updated = True
            return updated

        self.update(update_cleanup)


def collapse_path(path):
    """Collapse long paths with ellipsis"""
    home = Path.home().parts
    parts = path.parts
    if home == parts[: len(home)]:
        parts = ("~", *parts[len(home) :])
    if len(parts) > 5:
        parts = (parts[0], "\u2026") + parts[-4:]
    return Path().joinpath(*parts)


def candidates_path_key(path):
    """Key used to order path candidates"""
    hidden = 1 if path.name.startswith(".") else 0
    not_dir = 0 if path.is_dir() else 1
    return (hidden, not_dir, path)


def candidates_from_path(root, soft_limit=4096):
    """Build candidates list from provided root path

    Soft limit determines the depth of traversal once soft limit
    is reached none of the elements that are deeper will be returned
    """
    candidates = []
    max_depth = None
    queue = deque([(root, 0)])
    while queue:
        path, depth = queue.popleft()
        if max_depth and depth > max_depth:
            break
        if not path.is_dir():
            continue
        try:
            for item in sorted(path.iterdir(), key=candidates_path_key):
                if DEFAULT_IGNORE.match(item.name):
                    continue
                candidates.append(
                    "{}{}".format(item.relative_to(root), "/" if item.is_dir() else "")
                )
                if len(candidates) >= soft_limit:
                    max_depth = depth
                queue.append((item, depth + 1))
        except PermissionError:
            pass
    return candidates


def main():
    """Maintain and navigate visited path history"""
    parser = argparse.ArgumentParser(description=inspect.getdoc(main))
    subparsers = parser.add_subparsers(dest="command", required=True)
    parser_add = subparsers.add_parser("add", help="add/update path in the history")
    parser_add.add_argument("path", nargs="?", help="target path")
    subparsers.add_parser("list", help="list all entries in the history")
    parser_select = subparsers.add_parser(
        "select", help="interactively select path from the history or its subpaths"
    )
    parser_select.add_argument(
        "--theme", help="sweep theme, see sweep help from more info"
    )
    parser_select.add_argument(
        "--sweep", default="sweep", help="path to the sweep command"
    )
    parser_select.add_argument("--tty", help="path to the tty")
    opts = parser.parse_args()

    path_history = PathHistory()

    if opts.command == "add":
        path = opts.path or os.getcwd()
        path_history.add(path)

    elif opts.command == "list":
        path_history.cleanup()
        _, paths = path_history.load()
        items = []
        for path, (count, timestamp) in paths.items():
            items.append([count, timestamp, path])
        items.sort(reverse=True)
        for count, timestamp, path in items:
            date = datetime.fromtimestamp(timestamp).strftime("[%F %T]")
            print("{:<5} {} {}".format(count, date, path))

    elif opts.command == "select":
        path_history.cleanup()
        _, paths = path_history.load()
        items = []
        for path, (count, timestamp) in paths.items():
            items.append([count, timestamp, path])
        items.sort(reverse=True)

        result = None
        key_dir_list = "ctrl+i"  # tab
        key_dir_up = "backspace"  # only triggered when input is empty
        key_dir_hist = "ctrl+h"
        key_dir_open = "ctrl+o"
        with rpc.Sweep(
            sweep=[opts.sweep], theme=opts.theme, title="path history", tty=opts.tty
        ) as sweep:
            sweep.key_binding(key_dir_list, key_dir_list)
            sweep.key_binding(key_dir_up, key_dir_up)
            sweep.key_binding(key_dir_hist, key_dir_hist)
            sweep.key_binding(key_dir_open, key_dir_open)

            def history():
                cwd = str(Path.cwd())
                candidates = [cwd]
                for item in items:
                    path = str(item[2])
                    if path == cwd:
                        continue
                    candidates.append(path)

                sweep.prompt_set("PATH HISTORY")
                sweep.niddle_set("")
                sweep.candidates_clear()
                sweep.candidates_extend(candidates)

            def load_path(path):
                sweep.niddle_set("")
                sweep.prompt_set(str(collapse_path(path)))
                candidates = candidates_from_path(path)
                if candidates:
                    sweep.candidates_clear()
                    sweep.candidates_extend(candidates)

            history()
            current_path = None
            while True:
                msg = sweep.poll()
                if msg is None:
                    return
                msg_type, value = msg

                if msg_type == rpc.SWEEP_SELECTED:
                    if current_path is None:
                        result = value
                    else:
                        result = current_path / value
                    break

                elif msg_type == rpc.SWEEP_KEYBINDING:
                    if value == key_dir_list:
                        path = sweep.current()
                        if path is None:
                            continue
                        path = Path(path)
                        if current_path is None:
                            current_path = path
                        elif (current_path / path).is_dir():
                            current_path /= path
                        else:
                            continue
                        load_path(current_path)

                    elif value == key_dir_up:
                        if current_path is not None:
                            current_path = current_path.parent
                            load_path(current_path)

                    elif value == key_dir_hist:
                        history()
                        current_path = None

                    elif value == key_dir_open:
                        current = sweep.current()
                        if current is None:
                            result = current_path
                        else:
                            if current_path is None:
                                path = Path(current)
                            else:
                                path = current_path / current
                            result = path if path.is_dir() else path.parent
                        break

        if result is not None:
            print(result)


if __name__ == "__main__":
    main()
