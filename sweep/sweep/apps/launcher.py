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
    view_box=(0, 0, 128, 128),
    size=(1, 3),
    path="M81.10 82.23L68.94 109.01L61.73 92.32Q71.62 88.62 81.10 82.23"
    "L81.10 82.23ZM35.99 66.57L35.99 66.57L19.30 59.37L46.08 47.21"
    "Q39.69 56.69 35.99 66.57ZM95.72 29.09L95.72 29.09Q97.99 29.09 99.02 29.29"
    "L99.02 29.29Q99.84 35.88 97.37 42.89L97.37 42.89Q94.07 52.98 84.39 62.46"
    "L84.39 62.46Q73.27 73.78 58.03 79.55L58.03 79.55L48.55 70.28"
    "Q54.73 54.83 65.85 43.92L65.85 43.92Q74.09 35.68 82.74 31.97L82.74 31.97"
    "Q89.34 29.09 95.72 29.09ZM95.72 20.43L95.72 20.43Q87.69 20.43 79.65 23.73"
    "L79.65 23.73Q69.36 28.06 59.67 37.74L59.67 37.74Q47.52 49.68 40.52 67.19"
    "L40.52 67.19Q39.49 69.66 40.11 72.14Q40.72 74.61 42.58 76.46L42.58 76.46"
    "L51.85 85.73Q54.52 88.41 58.03 88.41L58.03 88.41Q59.47 88.41 61.12 87.79"
    "L61.12 87.79Q78.01 81.41 90.57 68.63L90.57 68.63Q97.58 61.84 101.90 54.22"
    "L101.90 54.22Q105.40 48.04 106.85 41.44L106.85 41.44"
    "Q108.08 36.29 107.88 31.35L107.88 31.35Q107.88 27.64 107.05 24.55"
    "L107.05 24.55L106.23 22.08L103.34 21.26Q99.84 20.43 95.72 20.43ZM75.12 53.19"
    "L75.12 53.19Q72.65 50.51 72.65 46.90Q72.65 43.30 75.23 40.72"
    "Q77.80 38.15 81.41 38.15Q85.01 38.15 87.59 40.72Q90.16 43.30 90.16 46.90"
    "Q90.16 50.51 87.59 53.08Q85.01 55.66 81.41 55.66Q77.80 55.66 75.12 53.19Z"
    "M44.02 78.11L50.20 84.29L44.02 78.11ZM32.48 108.18L38.66 108.18L54.73 92.32"
    "Q52.46 91.71 50.40 90.26L50.40 90.26L32.48 108.18ZM20.12 102.00L20.12 108.18"
    "L26.30 108.18L47.32 87.38L40.93 81.20L20.12 102.00ZM20.12 89.65L20.12 95.82"
    "L38.05 77.90Q36.60 75.84 35.99 73.58L35.99 73.58L20.12 89.65Z",
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
        prompt="Launcher",
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
