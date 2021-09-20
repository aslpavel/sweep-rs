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
from typing import (
    Any,
    Callable,
    Deque,
    Dict,
    Iterator,
    List,
    NamedTuple,
    Optional,
    Tuple,
    cast,
)
from dataclasses import dataclass

sys.path.insert(0, str(Path(__file__).expanduser().resolve().parent))
from sweep import Sweep, SWEEP_SELECTED, SWEEP_KEYBINDING, Candidate


PATH_HISTORY_FILE = "~/.path_history"
DEAFULT_SOFT_LIMIT = 65536
DEFAULT_IGNORE = re.compile(
    "|".join(
        [
            "\\.git",
            "\\.hg",
            "__pycache__",
            "\\.DS_Store",
            "\\.mypy_cache",
            "\\.pytest_cache",
            "\\.hypothesis",
            "target",
            ".*\\.elc",
            ".*\\.pyo",
            ".*\\.pyc",
        ]
    )
)


@dataclass
class PathHistoryEntry:
    path: Path
    count: int
    atime: int


@dataclass
class PathHistory:
    mtime: Optional[int]
    entries: Dict[Path, PathHistoryEntry]

    def __iter__(self):
        return iter(self.entries.values())


class PathHistoryStore:
    """Access and modify fpath history"""

    def __init__(self, history_path: str = PATH_HISTORY_FILE):
        self.history_path = Path(history_path).expanduser().resolve()

    def load(self) -> PathHistory:
        """Load path history"""
        if not self.history_path.exists():
            return PathHistory(None, {})
        with self.history_path.open("r") as file:
            try:
                fcntl.lockf(file, fcntl.LOCK_SH)
                content = io.StringIO(file.read())
            finally:
                fcntl.lockf(file, fcntl.LOCK_UN)

        mtime = int(content.readline().strip() or "0")
        paths: Dict[Path, PathHistoryEntry] = {}
        for line in content:
            count, timestamp, path = line.split("\t")
            count = int(count)
            date = int(timestamp)
            path = Path(path.strip("\n"))
            paths[path] = PathHistoryEntry(path, count, date)
        return PathHistory(mtime, paths)

    def update(self, update: Callable[[int, PathHistory], bool]):
        """AddTo/Update path history"""
        while True:
            now = int(time.time())
            history = self.load()
            mtime_last = history.mtime
            if not update(now, history):
                return

            content = io.StringIO()
            content.write(f"{now}\n")
            for entry in history:
                content.write(f"{entry.count}\t{entry.atime}\t{entry.path}\n")

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

    def add(self, path: Path):
        """Add/Update path in the history"""

        def update_add(now: int, history: PathHistory):
            entry = history.entries.get(path, PathHistoryEntry(path, 0, now))
            if history.mtime == entry.atime:
                # last update was for the same path, do not update
                return False
            history.entries[path] = PathHistoryEntry(path, entry.count + 1, now)
            return True

        path = Path(path).expanduser().resolve()
        if not path.exists():
            return
        self.update(update_add)

    def cleanup(self):
        """Remove paths from the history which no longre exist"""

        def update_cleanup(_: int, history: PathHistory):
            updated = False
            for entry in list(history):
                exists = False
                try:
                    exists = entry.path.exists()
                except PermissionError:
                    pass
                if not exists:
                    history.entries.pop(entry.path)
                    updated = True
            return updated

        self.update(update_cleanup)


# class FileNode(NamedTuple):
#     name: str
#     is_dir: bool
#     children: Optional[

# class FileExplorer:
#     def __init__(self):
#         pass

#     def walk(self, path: Path):
#         pass


def collapse_path(path: Path) -> Path:
    """Collapse long paths with ellipsis"""
    home = Path.home().parts
    parts = path.parts
    if home == parts[: len(home)]:
        parts = ("~", *parts[len(home) :])
    if len(parts) > 5:
        parts = (parts[0], "\u2026") + parts[-4:]
    return Path().joinpath(*parts)


def candidates_path_key(path: Path):
    """Key used to order path candidates"""
    hidden = 1 if path.name.startswith(".") else 0
    not_dir = 0 if path.is_dir() else 1
    return (hidden, not_dir, path)


def candidates_from_path(
    root: Path,
    file_limit: Optional[int] = None,
) -> Iterator[Candidate]:
    """Build candidates list from provided root path

    `file_limit` - determines the depth of traversal once soft limit
    is reached none of the elements that are deeper will be returned
    """
    file_limit = DEAFULT_SOFT_LIMIT if file_limit is None else file_limit
    candidates_total = 0
    max_depth = None

    queue: Deque[Tuple[Path, int]] = deque([(root, 0)])
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
                queue.append((item, depth + 1))

                candidates_total += 1
                yield {"entry": f"{path_relative}{tag}", "path": path_relative}

            if candidates_total >= file_limit:
                max_depth = depth
        except PermissionError:
            pass


KEY_LIST = "path.search_in_directory"
KEY_PARENT = "path.parent_directory"  # only triggered when input is empty
KEY_HISTORY = "path.history"
KEY_OPEN = "path.current_direcotry"
KEY_ALL = {
    KEY_LIST: "ctrl+i",
    KEY_PARENT: "backspace",
    KEY_HISTORY: "alt+.",
    KEY_OPEN: "ctrl+o",
}


class PathSelector:
    def __init__(self, sweep: Sweep, history: PathHistoryStore):
        self.sweep = sweep
        self.history = history
        # None - history mode
        # Path - path mode
        self.path: Optional[Path] = None

    async def show_history(self):
        """Show history"""
        # load history items
        history = self.history.load()
        items: List[Tuple[int, int, Path]] = []
        count_max = 0
        for entry in history:
            items.append((entry.count, entry.atime, entry.path))
            count_max = max(count_max, entry.count)
        items.sort(reverse=True)
        count_align = len(str(count_max)) + 1

        # create candidates
        cwd = str(Path.cwd())
        candidates: List[Candidate] = [
            dict(entry=f"{' ' * count_align}{cwd}", path=cwd)
        ]
        for count, _, path in items:
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
        if self.path is None:
            return
        await self.sweep.niddle_set("")
        await self.sweep.prompt_set("󰥩  {}".format(collapse_path(self.path)))
        await self.sweep.candidates_clear()
        await self.sweep.candidates_extend(candidates_from_path(self.path))

    async def run(self):
        for name, key in KEY_ALL.items():
            await self.sweep.key_binding(key, name)

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
                    entry = cast(Dict[str, Any], entry)

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
                    entry = cast(Dict[str, Any], entry)

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

    path_history = PathHistoryStore()

    if opts.command == "add":
        path = opts.path or os.getcwd()
        path_history.add(Path(path))

    elif opts.command == "list":
        path_history.cleanup()
        items: List[Tuple[int, int, Path]] = []
        for entry in path_history.load():
            items.append((entry.count, entry.atime, entry.path))
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
