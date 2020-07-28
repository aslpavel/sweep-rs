#!/usr/bin/env python3
from datetime import datetime
from pathlib import Path
import argparse
import fcntl
import io
import os
import sweep_rpc as rpc
import time
from collections import deque


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

    def update(self, update):
        """AddTo/Update path history"""
        while True:
            mtime, paths = self.load()
            update(paths)

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

    def add(self, path):
        def update_add(paths):
            count, _ = paths.get(path) or (0, datetime.now())
            count += 1
            paths[path] = (count, datetime.now())

        path = Path(path).expanduser().resolve()
        if not path.exists():
            return
        self.update(update_add)

    def cleanup(self):
        def update_cleanup(paths):
            for path in list(paths.keys()):
                if not Path(path).exists():
                    del paths[path]

        self.update(update_cleanup)


def collapse_path(path):
    home = Path.home().parts
    parts = path.parts
    if home == parts[: len(home)]:
        parts = ("~", *parts[len(home) :])
    if len(parts) > 5:
        parts = (parts[0], "\u2026") + parts[-4:]
    return Path().joinpath(*parts)


def candidates_from_path(root, soft_limit=1024):
    """Build candidates list from provided root path

    Soft limit determines the depth of traversal once soft limit
    is reached none of the elements that are deeper will be returned
    """
    candidates = []
    max_depth = soft_limit
    queue = deque([(root, 0)])
    while queue:
        path, depth = queue.popleft()
        if depth > max_depth:
            break
        if not path.is_dir():
            continue
        try:
            for item in path.iterdir():
                if item.name in DEFAULT_IGNORE:
                    continue
                candidates.append(str(item.relative_to(root)))
                if len(candidates) >= soft_limit:
                    max_depth = depth
                queue.append((item, depth + 1))
        except PermissionError:
            pass
    candidates.sort()
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
        path_history.cleanup()
        _, paths = path_history.load()
        items = []
        for path, (count, date) in paths.items():
            items.append([count, date, path])
        items.sort(reverse=True)
        for count, date, path in items:
            print("{:<5} {} {}".format(count, date.strftime("[%F %T]"), path))

    elif opts.command == "select":
        path_history.cleanup()
        _, paths = path_history.load()
        items = []
        for path, (count, date) in paths.items():
            items.append([count, date, path])
        items.sort(reverse=True)

        result = None
        tab = "ctrl+i"
        backspace = "backspace"
        with rpc.Sweep(prompt="PATH HISTORY", theme=opts.theme) as sweep:
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
                    if current_path is None:
                        result = value
                    else:
                        result = current_path / value
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
