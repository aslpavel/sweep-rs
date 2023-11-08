"""Simple tool to maintain and navigate visited path history
"""
# pyright: strict
from __future__ import annotations
from collections import deque
from datetime import datetime
from pathlib import Path
import argparse
import asyncio
import fcntl
import io
import os
import re
import shlex
import time
from typing import (
    Callable,
    Deque,
    Dict,
    Iterator,
    List,
    Optional,
    Tuple,
    TypedDict,
)
from dataclasses import dataclass
from .. import Sweep, SweepBind, Icon, SweepSelect
from . import sweep_default_cmd


PATH_HISTORY_FILE = "~/.path_history"
DEFAULT_SOFT_LIMIT = 65536
DEFAULT_IGNORE = re.compile(
    "|".join(
        [
            "\\.git",
            "\\.hg",
            "\\.venv",
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

# folder-clock-outline (Material Design)
HISTORY_ICON = Icon(
    path="M15,12H16.5V16.25L19.36,17.94L18.61,19.16L15,17V12M19,8H3V18H9.29"
    "C9.1,17.37 9,16.7 9,16A7,7 0 0,1 16,9C17.07,9 18.09,9.24 19,9.67V8"
    "M3,20C1.89,20 1,19.1 1,18V6A2,2 0 0,1 3,4H9L11,6H19A2,2 0 0,1 21,8"
    "V11.1C22.24,12.36 23,14.09 23,16A7,7 0 0,1 16,23C13.62,23 11.5,21.81 10.25,20"
    "H3M16,11A5,5 0 0,0 11,16A5,5 0 0,0 16,21A5,5 0 0,0 21,16A5,5 0 0,0 16,11Z",
    view_box=(0, 0, 24, 24),
    size=(1, 3),
    fallback=" ",
)

# folder-search-outline (Material Design)
SEARCH_ICON = Icon(
    path="M16.5,12C19,12 21,14 21,16.5C21,17.38 20.75,18.21 20.31,18.9L23.39,22"
    "L22,23.39L18.88,20.32C18.19,20.75 17.37,21 16.5,21C14,21 12,19 12,16.5"
    "C12,14 14,12 16.5,12M16.5,14A2.5,2.5 0 0,0 14,16.5A2.5,2.5 0 0,0 16.5,19"
    "A2.5,2.5 0 0,0 19,16.5A2.5,2.5 0 0,0 16.5,14M19,8H3V18H10.17"
    "C10.34,18.72 10.63,19.39 11,20H3C1.89,20 1,19.1 1,18V6C1,4.89 1.89,4 3,4"
    "H9L11,6H19A2,2 0 0,1 21,8V11.81C20.42,11.26 19.75,10.81 19,10.5V8Z",
    view_box=(0, 0, 24, 24),
    size=(1, 3),
    fallback=" ",
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

    def __iter__(self) -> Iterator[PathHistoryEntry]:
        return iter(self.entries.values())


class PathHistoryStore:
    """Access and modify path history"""

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
            count_str, timestamp, path_str = line.split("\t")
            count = int(count_str)
            date = int(timestamp)
            path = Path(path_str.strip("\n"))
            paths[path] = PathHistoryEntry(path, count, date)
        return PathHistory(mtime, paths)

    def update(self, update: Callable[[int, PathHistory], bool]) -> None:
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

    def add(self, path: Path) -> None:
        """Add/Update path in the history"""

        def update_add(now: int, history: PathHistory) -> bool:
            entry = history.entries.get(path, PathHistoryEntry(path, 0, now))
            if history.mtime == entry.atime:
                # last update was for the same path, do not update
                return False
            history.entries[path] = PathHistoryEntry(path, entry.count + 1, now)
            return True

        path = Path(path).expanduser().resolve()
        if path.exists():
            self.update(update_add)

    def cleanup(self) -> None:
        """Remove paths from the history which no longer exist"""

        def update_cleanup(_: int, history: PathHistory) -> bool:
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


class PathItem(TypedDict):
    entry: List[Tuple[str, bool]]
    path: str


class FileNode:
    __slots__ = ["path", "is_dir", "_children"]

    path: Path
    is_dir: bool
    _children: Optional[Dict[str, FileNode]]

    def __init__(self, path: Path) -> None:
        self.path = path
        self.is_dir = self.path.is_dir()
        self._children = None if self.is_dir else {}

    @property
    def children(self) -> Dict[str, FileNode]:
        if self._children is not None:
            return self._children
        self._children = {}
        try:
            for path in self.path.iterdir():
                if DEFAULT_IGNORE.match(path.name):
                    continue
                self._children[path.name] = FileNode(path)
        except (PermissionError, FileNotFoundError):
            pass
        return self._children

    def get(self, name: str) -> Optional[FileNode]:
        return self.children.get(name)

    def find(self, path: Path) -> Optional[FileNode]:
        node = self
        for name in path.parts:
            node_next = node.get(name)
            if node_next is None:
                return None
            node = node_next
        return node

    def candidates(self, limit: Optional[int] = None) -> Iterator[PathItem]:
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
                yield PathItem(
                    entry=[(f"{path_relative}{tag}", True)], path=path_relative
                )

            if count >= limit:
                max_depth = depth

    def _sort_key(self) -> Tuple[int, int, Path]:
        hidden = 1 if self.path.name.startswith(".") else 0
        not_dir = 0 if self.is_dir else 1
        return (hidden, not_dir, self.path)

    def __str__(self) -> str:
        return str(self.path)

    def __repr__(self) -> str:
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
KEY_ALL: Dict[str, Tuple[List[str], str]] = {
    KEY_LIST: (["ctrl+i", "tab"], "Navigate to currently pointed path"),
    KEY_PARENT: (["backspace"], "Go to the parent directory"),
    KEY_HISTORY: (["alt+."], "Open path history"),
    KEY_OPEN: (["ctrl+o"], "Return currently listed directory"),
}


class PathSelector:
    __slots__ = ["sweep", "history", "path", "path_cache", "show_path_task"]
    sweep: Sweep[PathItem]
    history: PathHistoryStore
    path: Optional[Path]
    path_cache: FileNode
    show_path_task: Optional[asyncio.Task[None]]

    def __init__(self, sweep: Sweep[PathItem], history: PathHistoryStore) -> None:
        self.sweep = sweep
        self.history = history
        # None - history mode
        # Path - path mode
        self.path: Optional[Path] = None
        self.path_cache = FileNode(Path("/"))
        self.show_path_task = None

    async def show_history(self, reset_needle: bool = True) -> None:
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
        candidates: List[PathItem] = [
            PathItem(entry=[(f"{' ' * count_align}{cwd}", True)], path=cwd)
        ]
        for count, _, path in items:
            path_str = str(path)
            if path_str == cwd:
                continue
            item = PathItem(
                entry=[(str(count).ljust(count_align), False), (path_str, True)],
                path=path_str,
            )
            candidates.append(item)

        # update sweep
        await self.sweep.prompt_set("PATH HISTORY", HISTORY_ICON)
        if reset_needle:
            await self.sweep.query_set("")
        await self.sweep.items_clear()
        await self.sweep.items_extend(candidates)

    async def show_path(self, reset_needle: bool = True) -> None:
        """Show current path"""
        if self.path is None:
            return
        if self.show_path_task is not None:
            self.show_path_task.cancel()
        if reset_needle:
            await self.sweep.query_set("")
        await self.sweep.prompt_set(str(collapse_path(self.path)), SEARCH_ICON)
        node = self.path_cache.find(self.path.relative_to("/"))
        if node is not None:
            await self.sweep.items_clear()
            # extending items without blocking
            loop = asyncio.get_running_loop()
            self.show_path_task = loop.create_task(
                self.sweep.items_extend(node.candidates())
            )

    async def run(self, path: Optional[Path] = None) -> Optional[Path]:
        """Run path selector

        If path is provided it will start in path mode otherwise in history mode
        """
        for name, (keys, desc) in KEY_ALL.items():
            for key in keys:
                await self.sweep.bind(key, name, desc)

        if path and path.is_dir():
            self.path = path
            await self.show_path(reset_needle=False)
        else:
            await self.show_history(reset_needle=False)

        async for event in self.sweep:
            if isinstance(event, SweepSelect) and event.items:
                path = Path(event.items[0]["path"])
                if self.path is None:
                    return path
                return self.path / path

            elif isinstance(event, SweepBind):
                # list directory under cursor
                if event.tag == KEY_LIST:
                    needle = (await self.sweep.query_get()).strip()
                    entry = await self.sweep.items_current()
                    if (
                        needle.startswith("/")
                        or needle.startswith("~")
                        or entry is None
                    ):
                        self.path, needle = get_path_and_query(needle)
                        await self.sweep.query_set(needle)
                        await self.show_path(reset_needle=False)
                    else:
                        path = Path(entry["path"])
                        if self.path is None:
                            self.path = path
                            await self.show_path()
                        elif (self.path / path).is_dir():
                            self.path /= path
                            await self.show_path()

                # list parent directory, list current directory in history mode
                elif event.tag == KEY_PARENT:
                    if self.path is None:
                        self.path = Path.cwd()
                    else:
                        self.path = self.path.parent
                    await self.show_path()

                # switch to history mode
                elif event.tag == KEY_HISTORY:
                    self.path = None
                    await self.show_history()

                # return current directory
                elif event.tag == KEY_OPEN:
                    if self.path:
                        # open current directory
                        return self.path
                    # history mode
                    entry = await self.sweep.items_current()
                    if entry is None:
                        continue
                    path = Path(entry["path"])
                    if path.is_dir():
                        return path
        return None


def get_path_and_query(input: str) -> Tuple[Path, str]:
    """Find longest existing prefix path and remaining query"""
    parts = list(Path(input).parts)
    query: List[str] = []
    path = Path()
    while parts:
        path = Path(*parts).expanduser()
        if path.is_dir():
            break
        query.append(parts.pop())
    return path.resolve(), str(os.path.sep.join(reversed(query)))


class ReadLine:
    """Extract required info from bash READLINE_{LINE|POINT}"""

    readline: str
    readpoint: int
    prefix: str
    suffix: str
    query: Optional[str]
    path: Optional[Path]

    def __init__(self, readline: str, point: int) -> None:
        self.readline = readline
        self.readpoint = point

        start = readline.rfind(" ", 0, point) + 1
        end = readline.find(" ", point)
        end = end if end > 0 else len(readline)

        self.prefix = readline[:start]
        self.suffix = readline[end:]
        self.path, self.query = get_path_and_query(readline[start:end])

    def format(self, path: Optional[Path]) -> str:
        if path is not None:
            path_str = str(path)
            readline = f"{self.prefix} " if self.prefix else ""
            readline += path_str
            readline += f" {self.suffix}" if self.suffix else ""
            point = len(self.prefix)
            mark = point + len(path_str)
        else:
            readline = self.readline
            point = self.readpoint
            mark = self.readpoint
        return f'READLINE_LINE="{readline}"\nREADLINE_POINT={point}\nREADLINE_MARK={mark}\n'


async def main(args: Optional[List[str]] = None) -> None:
    """Maintain and navigate visited path history"""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--history-file",
        default=PATH_HISTORY_FILE,
        help="path history file",
    )
    subparsers = parser.add_subparsers(dest="command", required=True)
    parser_add = subparsers.add_parser("add", help="add/update path in the history")
    parser_add.add_argument("path", nargs="?", help="target path")
    subparsers.add_parser("list", help="list all entries in the history")
    subparsers.add_parser("cleanup", help="cleanup history by checking they exist")
    parser_select = subparsers.add_parser(
        "select", help="interactively select path from the history or its subpaths"
    )
    parser_select.add_argument("--theme", help="sweep theme")
    parser_select.add_argument("--sweep", help="path to the sweep command")
    parser_select.add_argument("--tty", help="path to the tty")
    parser_select.add_argument("--query", help="initial query")
    parser_select.add_argument("--log", help="path to the log file")
    parser_select.add_argument(
        "--readline",
        action="store_true",
        help="complete based on readline variable",
    )
    opts = parser.parse_args(args)

    path_history = PathHistoryStore(opts.history_file)

    if opts.command == "add":
        path = opts.path or os.getcwd()
        path_history.add(Path(path))

    elif opts.command == "cleanup":
        path_history.cleanup()

    elif opts.command == "list":
        items: List[Tuple[int, int, Path]] = []
        for entry in path_history.load():
            items.append((entry.count, entry.atime, entry.path))
        items.sort(reverse=True)
        for count, timestamp, path in items:
            date = datetime.fromtimestamp(timestamp).strftime("[%F %T]")
            print("{:<5} {} {}".format(count, date, path))

    elif opts.command == "select":
        readline: Optional[ReadLine]
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
        async with Sweep[PathItem](
            sweep=shlex.split(opts.sweep) if opts.sweep else sweep_default_cmd(),
            tty=opts.tty,
            theme=opts.theme,
            title="path history",
            query=query,
            log=opts.log,
        ) as sweep:
            selector = PathSelector(sweep, path_history)
            result = await selector.run(path)

        if readline is not None:
            print(readline.format(result))
        elif result is not None:
            print(result)


if __name__ == "__main__":
    asyncio.run(main())
