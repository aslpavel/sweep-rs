#!/usr/bin/env python3
# pyright: strict
"""Application launcher

Lists all available desktop entries on the system
"""
from __future__ import annotations

import argparse
import asyncio
import shlex
from typing import Any, List, NamedTuple, cast, Optional
from gi.repository import Gio  # type: ignore
from .. import Candidate, Icon, sweep, sweep_default_cmd


ICON = Icon(
    view_box=(0, 0, 24, 24),
    size=(1, 3),
    path="M13.13 22.19L11.5 18.36C13.07 17.78 14.54 17 15.9 16.09L13.13 22.19"
    "M5.64 12.5L1.81 10.87L7.91 8.1C7 9.46 6.22 10.93 5.64 12.5M19.22 4"
    "C19.5 4 19.75 4 19.96 4.05C20.13 5.44 19.94 8.3 16.66 11.58"
    "C14.96 13.29 12.93 14.6 10.65 15.47L8.5 13.37C9.42 11.06 10.73 9.03 12.42 7.34"
    "C15.18 4.58 17.64 4 19.22 4M19.22 2C17.24 2 14.24 2.69 11 5.93"
    "C8.81 8.12 7.5 10.53 6.65 12.64C6.37 13.39 6.56 14.21 7.11 14.77L9.24 16.89"
    "C9.62 17.27 10.13 17.5 10.66 17.5C10.89 17.5 11.13 17.44 11.36 17.35"
    "C13.5 16.53 15.88 15.19 18.07 13C23.73 7.34 21.61 2.39 21.61 2.39"
    "S20.7 2 19.22 2M14.54 9.46C13.76 8.68 13.76 7.41 14.54 6.63"
    "S16.59 5.85 17.37 6.63C18.14 7.41 18.15 8.68 17.37 9.46"
    "C16.59 10.24 15.32 10.24 14.54 9.46M8.88 16.53L7.47 15.12L8.88 16.53"
    "M6.24 22L9.88 18.36C9.54 18.27 9.21 18.12 8.91 17.91L4.83 22H6.24M2 22"
    "H3.41L8.18 17.24L6.76 15.83L2 20.59V22M2 19.17L6.09 15.09"
    "C5.88 14.79 5.73 14.47 5.64 14.12L2 17.76V19.17Z",
)


class DesktopEntry(NamedTuple):
    app_info: Any  # Gio.AppInfo https://lazka.github.io/pgi-docs/#Gio-2.0/classes/DesktopAppInfo.html#Gio.DesktopAppInfo

    def to_candidate(self) -> Candidate:
        return Candidate().target_push(self.app_info.get_display_name())

    @staticmethod
    def get_all() -> List[DesktopEntry]:
        apps: List[DesktopEntry] = []
        for app_info in cast(List[Any], Gio.AppInfo.get_all()):  # type: ignore
            if not app_info.should_show():
                continue
            apps.append(DesktopEntry(app_info))
        apps.sort(key=lambda entry: entry.app_info.get_display_name())
        return apps


async def main(args: Optional[List[str]] = None) -> None:
    parser = argparse.ArgumentParser(
        formatter_class=argparse.RawDescriptionHelpFormatter,
        description=__doc__,
    )
    parser.add_argument("--theme", help="sweep theme")
    parser.add_argument("--sweep", help="path to the sweep command")
    parser.add_argument("--tty", help="path to the tty")
    parser.add_argument(
        "--action",
        choices=["print", "launch"],
        default="print",
        help="what to do with selected desktop entry",
    )
    opts = parser.parse_args(args)

    entry = await sweep(
        DesktopEntry.get_all(),
        sweep=[
            "kitty",
            "--title",
            "Sweep Launcher",
            "--class",
            "org.sweep.launcher",
            *(shlex.split(opts.sweep) if opts.sweep else sweep_default_cmd()),
        ],
        tty=opts.tty,
        theme=opts.theme or "dark",
        prompt="Launch",
        prompt_icon=ICON,
        height=1024,
        altscreen=True,
        tmp_socket=True,
        border=0,
    )
    if entry is None:
        return
    match opts.action:
        case "print":
            print(entry.app_info.get_commandline())
        case "launch":
            entry.app_info.launch()
        case _:
            pass


if __name__ == "__main__":
    asyncio.run(main())
