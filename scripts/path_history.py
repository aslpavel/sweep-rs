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
    Optional,
    Tuple,
    cast,
)
from dataclasses import dataclass

sys.path.insert(0, str(Path(__file__).expanduser().resolve().parent))
from sweep import Sweep, SWEEP_SELECTED, SWEEP_KEYBINDING, Candidate


PATH_HISTORY_FILE = "~/.path_history"
DEFAULT_SOFT_LIMIT = 65536
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


class FileNode:
    __slots__ = ["path", "is_dir", "_children"]

    path: Path
    is_dir: bool
    _children: Optional[Dict[str, "FileNode"]]

    def __init__(self, path: Path):
        self.path = path
        self.is_dir = self.path.is_dir()
        self._children = None if self.is_dir else {}

    @property
    def children(self) -> Dict[str, "FileNode"]:
        if self._children is not None:
            return self._children
        self._children = {}
        try:
            for path in self.path.iterdir():
                if DEFAULT_IGNORE.match(path.name):
                    continue
                self._children[path.name] = FileNode(path)
        except PermissionError:
            pass
        return self._children

    def get(self, name: str) -> Optional["FileNode"]:
        return self.children.get(name)

    def find(self, path: Path) -> Optional["FileNode"]:
        node = self
        for name in path.parts:
            node = node.get(name)
            if node is None:
                return None
        return node

    def candidates(self, limit: Optional[int] = None) -> Iterator[Candidate]:
        limit = DEFAULT_SOFT_LIMIT if limit is None else limit
        parts_len = len(self.path.parts)
        max_depth = None
        count = 0

        queue: Deque[Tuple[FileNode, int]] = deque([(self, 0)])
        while queue:
            node, depth = queue.popleft()
            if max_depth and depth > max_depth:
                break
            for item in sorted(node.children.values(), key=FileNode._sort_key):
                tag = "/" if item.is_dir else ""
                path_relative = "/".join(item.path.parts[parts_len:])
                queue.append((item, depth + 1))

                count += 1
                yield {"entry": f"{path_relative}{tag}", "path": path_relative}

            if count >= limit:
                max_depth = depth

    def _sort_key(self):
        hidden = 1 if self.path.name.startswith(".") else 0
        not_dir = 0 if self.is_dir else 1
        return (hidden, not_dir, self.path)

    def __str__(self):
        return str(self.path)

    def __repr__(self):
        return 'FileNode("{}")'.format(self.path)


def collapse_path(path: Path) -> Path:
    """Collapse long paths with ellipsis"""
    home = Path.home().parts
    parts = path.parts
    if home == parts[: len(home)]:
        parts = ("~", *parts[len(home) :])
    if len(parts) > 5:
        parts = (parts[0], "\u2026") + parts[-4:]
    return Path().joinpath(*parts)


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
        self.path_cache = FileNode(Path("/"))

    async def show_history(self, reset_niddle: bool = True):
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
        if reset_niddle:
            await self.sweep.niddle_set("")
        await self.sweep.candidates_clear()
        await self.sweep.candidates_extend(candidates)

    async def show_path(self, reset_niddle: bool = True):
        """Show current path"""
        if self.path is None:
            return
        if reset_niddle:
            await self.sweep.niddle_set("")
        await self.sweep.prompt_set("󰥩  {}".format(collapse_path(self.path)))
        node = self.path_cache.find(self.path.relative_to("/"))
        if node is not None:
            await self.sweep.candidates_clear()
            await self.sweep.candidates_extend(node.candidates())

    async def run(self, path: Optional[Path] = None) -> Optional[Path]:
        """Run path selelector

        If path is provided it will start in path mode otherwise in history mode
        """
        for name, key in KEY_ALL.items():
            await self.sweep.key_binding(key, name)

        if path and path.is_dir():
            self.path = path
            await self.show_path(reset_niddle=False)
        else:
            await self.show_history(reset_niddle=False)

        async for event in self.sweep:
            if event.method == SWEEP_SELECTED:
                path = Path(event.params["path"])
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


class ReadLine:
    """Extract reqdline info from bash READLINE_{LINE|POINT}"""

    readline: str
    readpoint: int
    prefix: str
    suffix: str
    query: Optional[str]
    path: Optional[Path]

    def __init__(self, readline: str, point: int):
        self.readline = readline
        self.readpoint = point

        start = readline.rfind(" ", 0, point) + 1
        end = readline.find(" ", point)
        end = end if end > 0 else len(readline)

        self.prefix = readline[:start]
        self.suffix = readline[end:]
        parts = list(Path(readline[start:end]).parts)

        # path is a longest leading directory
        query: List[str] = []
        path = Path()
        while parts:
            path = Path(*parts).expanduser()
            if path.is_dir():
                break
            query.append(parts.pop())
        self.path = path.resolve()
        self.query = str(os.path.sep.join(reversed(query)))

    def format(self, path: Optional[Path]) -> str:
        if path is not None:
            readline = f"{self.prefix} " if self.prefix else ""
            readline += str(path)
            readline += f" {self.suffix}" if self.suffix else ""
            point = len(self.prefix)
        else:
            readline = self.readline
            point = self.readpoint
        return f'READLINE_LINE="{readline}"\nREADLINE_POINT={point}\n'


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
    parser_select.add_argument("--query", help="initial query")
    parser_select.add_argument("--tty", help="path to the tty")
    parser_select.add_argument(
        "--readline",
        action="store_true",
        help="complete based on readline variable",
    )
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

        if opts.readline:
            readline = ReadLine(
                os.environ.get("READLINE_LINE", ""),
                int(os.environ.get("READLINE_POINT", "0")),
            )
            query = readline.query
            path = readline.path
        else:
            readline = None
            query = opts.query
            path = None

        result = None
        async with Sweep(
            sweep=[opts.sweep],
            theme=opts.theme,
            title="path history",
            tty=opts.tty,
            query=query,
        ) as sweep:
            selector = PathSelector(sweep, path_history)
            result = await selector.run(path)

        if readline is not None:
            print(readline.format(result))
        elif result is not None:
            print(result)


if __name__ == "__main__":
    asyncio.run(main())
