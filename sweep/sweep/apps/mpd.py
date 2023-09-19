"""Application launcher

Lists all available desktop entries on the system
"""
# pyright: strict
from __future__ import annotations

import argparse
import asyncio
from dataclasses import dataclass
import shlex
import os
import io
from PIL import Image
from typing import Any, AsyncIterator, List, NamedTuple, Sequence, cast, Optional, Dict
from gi.repository import Gio  # type: ignore
from .. import Candidate, Icon, sweep, Field
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
FLATPAK_REF = 0

TERMINAL_ICON = Icon(
    size=(1, 3),
    view_box=(0, 0, 128, 128),
    fallback="[T]",
    path="M41.09 58.85L41.09 58.85Q41.93 58.01 42.98 58.01Q44.03 58.01 44.87 58.85L44.87 58.85L55.38 69.36Q56.22 69.99 56.22 71.15Q56.22 72.30 55.38 72.93L55.38 72.93L44.87 83.44Q44.03 84.28 42.98 84.28Q41.93 84.28 41.09 83.55Q40.25 82.81 40.25 81.66Q40.25 80.50 41.09 79.87L41.09 79.87L49.71 71.04L41.09 62.42Q40.25 61.79 40.25 60.64Q40.25 59.48 41.09 58.85ZM87.75 79.03L87.75 79.03L61.48 79.03Q60.22 79.03 59.48 79.76Q58.75 80.50 58.75 81.66Q58.75 82.81 59.48 83.55Q60.22 84.28 61.27 84.28L61.27 84.28L87.75 84.28Q88.80 84.28 89.54 83.55Q90.27 82.81 90.27 81.66Q90.27 80.50 89.54 79.76Q88.80 79.03 87.75 79.03ZM27.22 86.80L27.22 39.51Q27.22 34.26 31.11 30.37Q35.00 26.48 40.25 26.48L40.25 26.48L87.54 26.48Q93.00 26.48 96.89 30.37Q100.78 34.26 100.78 39.72L100.78 39.72L100.78 86.80Q100.78 92.27 96.89 96.16Q93.00 100.05 87.54 100.05L87.54 100.05L40.25 100.05Q35.00 100.05 31.11 96.16Q27.22 92.27 27.22 86.80L27.22 86.80ZM32.47 42.25L95.53 42.25L95.53 39.51Q95.53 36.36 93.22 34.05Q90.90 31.74 87.54 31.74L87.54 31.74L40.25 31.74Q37.10 31.74 34.78 34.05Q32.47 36.36 32.47 39.72L32.47 39.72L32.47 42.25ZM95.53 47.50L32.47 47.50L32.47 86.80Q32.47 90.17 34.78 92.48Q37.10 94.79 40.25 94.79L40.25 94.79L87.54 94.79Q90.90 94.79 93.22 92.48Q95.53 90.17 95.53 86.80L95.53 86.80L95.53 47.50Z",
)
TERMINAL_REF = 1


class MPDChunk(NamedTuple):
    """Single chunk of data returned as response"""

    name: str
    data: str | bytes

    def get_bytes(self) -> bytes:
        """Return bytes if it was binary data"""
        if self.name == "binary":
            return cast(bytes, self.data)
        return b""


@dataclass
class Song:
    file: str
    duration: float
    artist: Optional[str]
    album: Optional[str]
    title: Optional[str]
    attrs: Dict[str, str]

    def __init__(self, file: str) -> None:
        self.file = file
        self.duration = 0.0
        self.artist = None
        self.album = None
        self.title = None
        self.attrs = {}

    def to_candidate(self) -> Candidate:
        candidate = Candidate()
        if self.title:
            if self.artist:
                candidate.target_push(self.artist)
                candidate.target_push(" - ")
            candidate.target_push(self.title)
        else:
            candidate.target_push(os.path.basename(self.file))
        if self.album:
            candidate.right_push(self.album)
        return candidate


class MPD:
    """MPD Client implementation"""

    def __init__(self, host: str = "localhost", port: int = 6600):
        self._host = host
        self._port = port
        self._reader: Optional[asyncio.StreamReader] = None
        self._writer: Optional[asyncio.StreamWriter] = None

    async def __aenter__(self) -> MPD:
        self._reader, self._writer = await asyncio.open_connection(
            self._host, self._port
        )
        init = await self._reader.readline()
        init = init.strip()
        if not init.startswith(b"OK MPD"):
            raise RuntimeError(f"invalid initial response from the MPD: {init}")
        return self

    async def __aexit__(self, _et: Any, _eo: Any, _tb: Any) -> bool:
        if self._writer:
            self._writer.close()
        self._writer, self._reader = None, None
        return False

    async def _send_request(self, cmd: str, args: Sequence[str]) -> None:
        """Send MPD request"""
        if self._writer is None:
            raise RuntimeError("MPD is not connected")
        self._writer.write(cmd.encode())
        for arg in args:
            self._writer.write(b" ")
            self._writer.write(mpd_escape(arg).encode())
        self._writer.write(b"\n")
        await self._writer.drain()

    async def _recv_response(self) -> AsyncIterator[MPDChunk]:
        """Receive MPD response"""
        if self._reader is None:
            raise RuntimeError("MPD is not connected")
        while True:
            line = await self._reader.readline()
            line = line.strip()
            if line == b"OK":
                break
            elif line.startswith(b"ACK "):
                raise ValueError(line[4:].strip().decode())
            name, value = line.rsplit(b": ", maxsplit=1)
            if name == b"binary":
                data = await self._reader.readexactly(int(value))
                yield MPDChunk("binary", data)
                await self._reader.readline()
            else:
                yield MPDChunk(name.decode(), value.decode())

    async def call(self, cmd: str, *args: str) -> AsyncIterator[MPDChunk]:
        await self._send_request(cmd, args)
        async for chunk in self._recv_response():
            yield chunk

    async def playlistinfo(self) -> List[Song]:
        return await mpd_songs(self.call("playlistinfo"))

    async def readpicture(self, file: str) -> Optional[Image.Image]:
        """Read picture embedded in music file"""
        cmd = "readpicture"
        size = 0
        data = io.BytesIO()
        async for chunk in self.call(cmd, file, "0"):
            if chunk.name == "size":
                size = int(chunk.data)
            else:
                data.write(chunk.get_bytes())
        while data.tell() < size:
            async for chunk in self.call(cmd, file, str(data.tell())):
                data.write(chunk.get_bytes())
        if data.tell() == 0:
            return None
        data.seek(0)
        return Image.open(data)


def mpd_escape(value: str) -> str:
    if " " not in value and "'" not in value and not '"' in value:
        return value
    value_escaped = value.replace("\\", "\\\\").replace("'", "\\'").replace('"', '\\"')
    return f'"{value_escaped}"'


async def mpd_songs(chunks: AsyncIterator[MPDChunk]) -> List[Song]:
    """Parse songs from *info commands"""
    songs: List[Song] = []
    song = Song("")
    async for chunk in chunks:
        match chunk.name:
            case "file":
                if song.file:
                    songs.append(song)
                song = Song(cast(str, chunk.data))
            case "duration":
                song.duration = float(chunk.data)
            case "Artist":
                song.artist = cast(str, chunk.data)
            case "Album":
                song.album = cast(str, chunk.data)
            case "Title":
                song.title = cast(str, chunk.data)
            case "Time":
                continue
            case name:
                song.attrs[name] = cast(str, chunk.data)
    if song.file:
        songs.append(song)
    return songs


async def main(args: Optional[List[str]] = None) -> None:
    parser = argparse.ArgumentParser(
        formatter_class=argparse.RawDescriptionHelpFormatter,
        description=__doc__,
    )
    parser.add_argument("--theme", help="sweep theme")
    parser.add_argument("--sweep", help="path to the sweep command")
    parser.add_argument("--tty", help="path to the tty")
    parser.add_argument("--log", help="log file")
    parser.add_argument(
        "--no-window", action="store_true", help="do not create new terminal window"
    )
    parser.add_argument(
        "--action",
        choices=["print", "launch"],
        default="print",
        help="what to do with selected desktop entry",
    )
    opts = parser.parse_args(args)

    fields = {
        FLATPAK_REF: Field(glyph=FLATPAK_ICON),
        TERMINAL_REF: Field(glyph=TERMINAL_ICON),
    }

    sweep_theme = opts.theme
    sweep_args: Dict[str, Any] = {}
    sweep_cmd: List[str] = []
    if not opts.no_window:
        sweep_theme = sweep_theme or "dark"
        sweep_args.update(
            dict(
                altscreen=True,
                height=1024,
                tmp_socket=True,
                border=0,
            )
        )
        sweep_cmd.extend(["kitty", "--class", "org.aslpavel.sweep.launcher"])
    sweep_cmd.extend(shlex.split(opts.sweep) if opts.sweep else sweep_default_cmd())

    async with MPD() as mpd:
        # async for song in mpd.playlistinfo():
        #     print(song)
        # print(
        #     await mpd.readpicture(
        #         "Summoning/[2001] Let Mortal Heroes Sing Your Fame/08 Farewell.mp3"
        #     )
        # )

        entry = await sweep(
            (await mpd.playlistinfo()),
            sweep=sweep_cmd,
            fields=fields,
            scorer="substr",
            tty=opts.tty,
            theme=sweep_theme,
            prompt="Launch",
            prompt_icon=PROMPT_ICON,
            log=opts.log,
            title="Sweep Launcher",
            **sweep_args,
        )
        print(entry)


if __name__ == "__main__":
    asyncio.run(main())
