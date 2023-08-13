"""Application launcher

Lists all available desktop entries on the system
"""
# pyright: strict
from __future__ import annotations

import argparse
import asyncio
import shlex
import re
from typing import Any, List, NamedTuple, cast, Optional
from gi.repository import Gio  # type: ignore
from .. import Candidate, Icon, sweep
from . import sweep_default_cmd

# material-rocket-launch-outline
PROMPT_ICON = Icon(
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
# fluent-box-multiple
FLATPAK_ICON = Icon(
    size=(1, 3),
    view_box=(0, 0, 128, 128),
    fallback="[F]",
    path="M100.99 30.27L82.71 23.12Q77.24 21.02 71.57 23.33L71.57 23.33L53.28 30.27Q50.97 31.11 49.60 33.10Q48.24 35.10 48.24 37.62L48.24 37.62L48.24 43.30Q50.76 43.09 53.49 43.30L53.49 43.30L53.49 37.62Q53.49 35.73 55.17 35.10L55.17 35.10L73.46 28.16Q77.24 26.69 80.81 28.16L80.81 28.16L99.10 35.10Q100.78 35.73 100.78 37.62L100.78 37.62L100.78 78.40Q100.78 80.29 99.10 80.92L99.10 80.92L85.02 86.38L85.02 88.91Q85.02 90.59 84.60 92.06L84.60 92.06L100.99 85.75Q103.30 84.91 104.67 82.92Q106.04 80.92 106.04 78.40L106.04 78.40L106.04 37.62Q106.04 35.10 104.67 33.10Q103.30 31.11 100.99 30.27L100.99 30.27ZM95.95 38.46L95.95 38.46Q95.53 37.62 94.58 37.10Q93.64 36.57 92.58 36.99L92.58 36.99L78.08 42.67Q77.03 43.09 76.19 42.67L76.19 42.67L61.69 36.99Q60.22 36.36 59.06 37.41Q57.90 38.46 58.11 39.93Q58.33 41.41 59.80 42.04L59.80 42.04L74.30 47.50Q77.03 48.55 79.97 47.50L79.97 47.50L94.48 42.04Q95.53 41.62 95.95 40.56Q96.37 39.51 95.95 38.46ZM69.67 64.74L69.67 64.74Q69.25 63.89 68.31 63.37Q67.36 62.84 66.31 63.26L66.31 63.26L50.97 69.36L35.42 63.26Q34.36 62.84 33.42 63.37Q32.47 63.89 32.05 64.84Q31.63 65.79 32.05 66.84Q32.47 67.89 33.52 68.31L33.52 68.31L48.24 73.77L48.24 87.01Q48.24 88.07 48.97 88.80Q49.71 89.54 50.86 89.54Q52.02 89.54 52.76 88.80Q53.49 88.07 53.49 87.01L53.49 87.01L53.49 73.77L68.20 68.31Q69.25 67.89 69.67 66.84Q70.10 65.79 69.67 64.74ZM74.72 56.54L56.43 49.39Q50.76 47.29 45.29 49.39L45.29 49.39L27.01 56.54Q24.70 57.38 23.33 59.38Q21.96 61.37 21.96 63.89L21.96 63.89L21.96 88.91Q21.96 91.43 23.33 93.43Q24.70 95.42 27.01 96.26L27.01 96.26L45.29 103.41Q50.76 105.51 56.43 103.41L56.43 103.41L74.72 96.26Q77.03 95.42 78.40 93.43Q79.76 91.43 79.76 88.91L79.76 88.91L79.76 63.89Q79.76 61.37 78.40 59.38Q77.03 57.38 74.72 56.54L74.72 56.54ZM28.90 61.37L47.19 54.44Q50.97 52.97 54.54 54.44L54.54 54.44L72.83 61.37Q74.51 62.00 74.51 63.89L74.51 63.89L74.51 88.91Q74.51 90.80 72.83 91.43L72.83 91.43L54.54 98.36Q50.97 99.84 47.19 98.36L47.19 98.36L28.90 91.43Q27.22 90.80 27.22 88.91L27.22 88.91L27.22 63.89Q27.22 62.00 28.90 61.37L28.90 61.37Z",
)


class DesktopEntry(NamedTuple):
    CLEANUP_RE = re.compile("@@[a-zA-Z]?")
    URL_RE = re.compile("%[uUfF]")

    app_info: Any  # Gio.AppInfo https://lazka.github.io/pgi-docs/#Gio-2.0/classes/DesktopAppInfo.html#Gio.DesktopAppInfo

    def commandline(self, path: Optional[str] = None, term: str = "kitty") -> str:
        """Get command line required to launch app"""
        cmd: str = self.app_info.get_commandline()
        cmd = self.CLEANUP_RE.sub("", cmd)
        cmd = self.URL_RE.sub(path or "", cmd).strip()
        if self.requires_terminal():
            return f"{term} {cmd}"
        return cmd

    def requires_terminal(self) -> bool:
        """Whether app needs to be run in a terminal"""
        return self.app_info.get_boolean("Terminal")

    def app_id(self) -> str:
        """Return app_id"""
        return self.app_info.get_id().strip().removesuffix(".desktop")

    def description(self) -> str:
        """Return app description"""
        return self.app_info.get_description() or ""

    def is_flatpak(self) -> bool:
        """Whether app is a flatpak app"""
        return self.commandline().find("flatpak") >= 0

    def to_candidate(self) -> Candidate:
        candidate = (
            Candidate()
            .target_push(self.app_info.get_display_name())
            .preview_flex_set(1.0)
            .preview_push(f" {self.description() or 'No description'}\n", face="bg=#d3869b30")
            .preview_push("\n")
            .preview_push(f"cmd     : {self.commandline()}\n")
            .preview_push(f"icon    : {self.app_info.get_icon()}\n")
            .preview_push(f"id      : {self.app_id()}\n")
            .preview_push(f"name    : {self.app_info.get_name()}\n")
            .preview_push(f"term    : {self.requires_terminal()}\n")
            .preview_push(f"file    : {self.app_info.get_filename()}\n")
            .preview_push(f"gname   : {self.app_info.get_generic_name()}\n")
            .preview_push(f"kw      : {' '.join(self.app_info.get_keywords())}\n")
            .preview_push(f"actions : {self.app_info.list_actions()}\n")
        )
        if self.is_flatpak():
            candidate.right_push(glyph=FLATPAK_ICON)
        return candidate

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
        prompt_icon=PROMPT_ICON,
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
