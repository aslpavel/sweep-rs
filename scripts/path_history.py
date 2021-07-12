#!/usr/bin/env python3
"""Simple tool to maintain and navigate visited path history
"""
from collections import deque
from datetime import datetime
from pathlib import Path
import argparse
import asyncio
import fcntl
import inspect
import io
import os
import re
import sys
import time

sys.path.insert(0, str(Path(__file__).expanduser().resolve().parent))
from sweep import Sweep, SWEEP_SELECTED, SWEEP_KEYBINDING


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


def candidates_from_path(root: Path, soft_limit=4096):
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
                tag = "/" if item.is_dir() else ""
                path_relative = str(item.relative_to(root))
                candidates.append(
                    {"entry": f"{path_relative}{tag}", "path": path_relative}
                )
                if len(candidates) >= soft_limit:
                    max_depth = depth
                queue.append((item, depth + 1))
        except PermissionError:
            pass
    return candidates


KEY_LIST = "ctrl+i"  # tab
KEY_PARENT = "backspace"  # only triggered when input is empty
KEY_HISTORY = "ctrl+h"
KEY_OPEN = "ctrl+o"
KEY_ALL = [KEY_LIST, KEY_PARENT, KEY_HISTORY, KEY_OPEN]


class PathSelector:
    def __init__(self, sweep, history):
        self.sweep = sweep
        self.history = history
        # None - history mode
        # Path - path mode
        self.path = None

    async def show_history(self):
        """Show history"""
        # load history items
        _, paths = self.history.load()
        items = []
        count_max = 0
        for path, (count, timestamp) in paths.items():
            items.append([count, timestamp, path])
            count_max = max(count_max, count)
        items.sort(reverse=True)
        count_align = len(str(count_max)) + 1

        # create candidates
        cwd = str(Path.cwd())
        candidates = [dict(entry=f"{' ' * count_align}{cwd}", path=cwd)]
        for count, _timestamp, path in items:
            path = str(path)
            if path == cwd:
                continue
            candidates.append(
                {"entry": [(str(count).ljust(count_align), False), path], "path": path}
            )

        # update sweep
        await self.sweep.prompt_set("󰪻  PATH HISTORY")
        await self.sweep.niddle_set("")
        await self.sweep.candidates_clear()
        await self.sweep.candidates_extend(candidates)

    async def show_path(self):
        """Show current path"""
        await self.sweep.niddle_set("")
        await self.sweep.prompt_set("󰥩  {}".format(collapse_path(self.path)))
        candidates = candidates_from_path(self.path)
        if candidates:
            await self.sweep.candidates_clear()
            await self.sweep.candidates_extend(candidates)

    async def run(self):
        for key in KEY_ALL:
            await self.sweep.key_binding(key, key)

        await self.show_history()
        async for event in self.sweep:
            if event.method == SWEEP_SELECTED:
                path = event.params["path"]
                if self.path is None:
                    return path
                return self.path / path

            elif event.method == SWEEP_KEYBINDING:
                # list directory under cursor
                if event.params == KEY_LIST:
                    entry = await self.sweep.current()
                    if entry is None:
                        continue

                    path = Path(entry["path"])
                    if self.path is None:
                        self.path = path
                        await self.show_path()
                    elif (self.path / path).is_dir():
                        self.path /= path
                        await self.show_path()

                # list parent directory, list current directory in history mode
                elif event.params == KEY_PARENT:
                    if self.path is None:
                        self.path = Path.cwd()
                    else:
                        self.path = self.path.parent
                    await self.show_path()

                # switch to history mode
                elif event.params == KEY_HISTORY:
                    self.path = None
                    await self.show_history()

                # return directory associted with current entry
                elif event.params == KEY_OPEN:
                    entry = await self.sweep.current()
                    if entry is None:
                        continue

                    path = Path(entry["path"])
                    if self.path is None:
                        return path
                    else:
                        path = self.path / path
                        if path.is_dir():
                            return path
                        return path.parent


async def main():
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

        async with Sweep(
            sweep=[opts.sweep], theme=opts.theme, title="path history", tty=opts.tty
        ) as sweep:
            selector = PathSelector(sweep, path_history)
            result = await selector.run()

        if result is not None:
            print(result)


if __name__ == "__main__":
    asyncio.run(main())
