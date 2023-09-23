"""Application launcher

Lists all available desktop entries on the system
"""
# pyright: strict
from __future__ import annotations

import argparse
import asyncio
import shlex
import os
import io
from enum import Enum
from PIL import Image as PILImage
from PIL.Image import Resampling
from dataclasses import dataclass
from typing import Any, AsyncIterator, List, NamedTuple, Sequence, cast, Optional, Dict

from .. import Candidate, Icon, Sweep, Field, Image, Container, Align, Flex, Text
from . import sweep_default_cmd

# material-rocket-launch-outline
PROMPT_ICON = Icon(
    view_box=(0, 0, 128, 128),
    size=(1, 3),
    path="M52.02 33.84L88.80 22.49Q91.32 21.65 93.43 23.22Q95.53 24.80 95.53 27.53L95.53 27.53L95.53 81.55Q95.53 86.17 92.69 89.75Q89.85 93.32 85.44 94.37Q81.02 95.42 76.82 93.53Q72.62 91.64 70.62 87.54Q68.62 83.44 69.57 78.92Q70.52 74.40 73.98 71.57Q77.45 68.73 82.08 68.52Q86.70 68.31 90.27 71.25L90.27 71.25L90.27 43.09L53.49 54.65L53.49 92.06Q53.49 96.68 50.65 100.26Q47.82 103.83 43.40 104.88Q38.99 105.93 34.78 104.04Q30.58 102.15 28.58 98.05Q26.59 93.95 27.53 89.43Q28.48 84.91 31.95 82.08Q35.42 79.24 40.04 79.03Q44.66 78.82 48.24 81.76L48.24 81.76L48.24 38.88Q48.24 37.20 49.29 35.84Q50.34 34.47 52.02 33.84L52.02 33.84ZM53.49 38.88L53.49 49.18L90.27 37.62L90.27 27.32L53.49 38.88ZM40.46 84.28L40.46 84.28Q37.10 84.28 34.78 86.59Q32.47 88.91 32.47 92.16Q32.47 95.42 34.78 97.73Q37.10 100.05 40.35 100.05Q43.61 100.05 45.92 97.73Q48.24 95.42 48.24 92.16Q48.24 88.91 45.92 86.59Q43.61 84.28 40.46 84.28ZM74.51 81.76L74.51 81.55Q74.51 84.91 76.82 87.22Q79.13 89.54 82.39 89.54Q85.65 89.54 87.96 87.22Q90.27 84.91 90.27 81.66Q90.27 78.40 87.96 76.09Q85.65 73.77 82.39 73.77Q79.13 73.77 76.82 76.09Q74.51 78.40 74.51 81.76L74.51 81.76Z",
)


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

    def __eq__(self, other: Any) -> bool:
        return self.file == other.file

    def __hash__(self) -> int:
        return hash(self.file)

    def album_id(self) -> int:
        return abs(hash(self.attrs.get("MUSICBRAINZ_ALBUMID") or self.album or ""))

    def to_candidate(self) -> Candidate:
        result = Candidate()

        # target
        if self.title:
            result.target_push(self.title)
        else:
            result.target_push(os.path.basename(self.file))

        # right
        result.right_push(duration_fmt(self.duration))

        # preview
        if self.artist:
            result.preview_push(f"Artist: ", face="bold").preview_push(
                f"{self.artist}\n", active=True
            )
        if self.album:
            result.preview_push("Album : ", face="bold").preview_push(
                f"{self.album}\n", active=True
            )
        result.preview_push(ref=self.album_id()).preview_flex_set(1)
        return result


class MPDState(Enum):
    WAIT = 0
    IDLE = 1
    REQUEST = 2


class MPD:
    """MPD Client implementation"""

    __slots__ = [
        "_host",
        "_port",
        "_reader",
        "_writer",
        "_state",
        "_state_cond",
        "_idle_task",
        "_album_id_to_song",
    ]

    def __init__(self, host: str = "localhost", port: int = 6600):
        self._host = host
        self._port = port

        self._reader: Optional[asyncio.StreamReader] = None
        self._writer: Optional[asyncio.StreamWriter] = None

        self._state = MPDState.WAIT
        self._state_cond = asyncio.Condition()
        self._idle_task = asyncio.create_task(self._idle_coro(), name="mpd-idle")

        self._album_id_to_song: Dict[int, Song] = {}

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
            name, value = line.split(b": ", maxsplit=1)
            if name == b"binary":
                data = await self._reader.readexactly(int(value))
                yield MPDChunk("binary", data)
                await self._reader.readline()
            else:
                yield MPDChunk(name.decode(), value.decode())

    async def _idle_coro(self) -> None:
        """Client needs to be in IDLE state if there is no request to avoid timeout"""
        while True:
            await asyncio.sleep(1)
            async for _chunk in self.call("idle"):
                pass

    async def call(self, cmd: str, *args: str) -> AsyncIterator[MPDChunk]:
        print(cmd, args)
        async with self._state_cond:
            # interrupt idle state
            if self._state == MPDState.IDLE:
                await self._send_request("noidle", [])
            # wait for client to transition into WAIT state
            while self._state != MPDState.WAIT:
                await self._state_cond.wait()
            # change state
            self._state = MPDState.IDLE if cmd == "idle" else MPDState.REQUEST
            await self._send_request(cmd, args)
        try:
            async for chunk in self._recv_response():
                yield chunk
        finally:
            # transition to WAIT state and wake up other tasks
            async with self._state_cond:
                self._state = MPDState.WAIT
                self._state_cond.notify_all()

    def song_by_id(self, id: int) -> Optional[Song]:
        return self._album_id_to_song.get(id)

    async def playlistinfo(self) -> List[Song]:
        songs = await mpd_songs(self.call("playlistinfo"))
        for song in songs:
            self._album_id_to_song[song.album_id()] = song
        return songs

    async def listallinfo(self) -> List[Song]:
        songs = await mpd_songs(self.call("listallinfo"))
        for song in songs:
            self._album_id_to_song[song.album_id()] = song
        return songs

    async def readpicture(
        self, file: str, width: int = 500
    ) -> Optional[PILImage.Image]:
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
        img = PILImage.open(data)
        if img.width != width:
            img = img.resize(
                (width, round(width * img.height / img.width)),
                resample=Resampling.BILINEAR,
            )
        return img


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


def duration_fmt(duration: float) -> str:
    mins, secs = divmod(duration, 60)
    hours, mins = divmod(mins, 60)
    result = f"{mins:>02.0f}:{secs:>02.0f}"
    if hours:
        result = f"{hours:>02.0f}:{result}"
    return result


class MPDSweepView(Enum):
    PLAYLIST = 0
    ALL = 1
    MAX = 2


class MPDSweep:
    def __init__(self, mpd: MPD, sweep: Sweep[Song]) -> None:
        self.mpd = mpd
        self.sweep = sweep
        self.view = MPDSweepView.MAX

    async def run(self) -> None:
        self.sweep.field_resolver_set(self._field_resolver)
        await self.sweep.bind(
            key="ctrl+i",
            tag="mpd.tab",
            desc="switch between different views",
            handler=self._tab_handler,
        )
        await self.view_playlist()
        async for _event in self.sweep:
            pass

    async def view_switch(self, view: Optional[MPDSweepView] = None):
        match view:
            case None | MPDSweepView.MAX:
                view = MPDSweepView((self.view.value + 1) % MPDSweepView.MAX.value)
                await self.view_switch(view)
            case MPDSweepView.ALL:
                await self.view_all()
            case MPDSweepView.PLAYLIST:
                await self.view_playlist()

    async def view_playlist(self):
        if self.view == MPDSweepView.PLAYLIST:
            return
        self.view = MPDSweepView.PLAYLIST

        await self.sweep.footer_set(
            Text().push("Footer: ", face="bold").push("Playlist")
        )
        await self.sweep.prompt_set("Playlist")
        await self.sweep.items_clear()
        await self.sweep.items_extend(await self.mpd.playlistinfo())

    async def view_all(self):
        if self.view == MPDSweepView.ALL:
            return
        self.view = MPDSweepView.ALL

        await self.sweep.footer_set(Text().push("Footer: ", face="bold").push("All"))
        await self.sweep.prompt_set("All Songs")
        await self.sweep.items_clear()
        await self.sweep.items_extend(await self.mpd.listallinfo())

    async def _on_select(self, song: Song):
        match self.view:
            case MPDSweepView.PLAYLIST:
                pass
            case _:
                pass

    async def _field_resolver(self, ref: int) -> Field:
        song = self.mpd.song_by_id(ref)
        if song is None:
            return Field()
        cover = await self.mpd.readpicture(song.file)
        if cover is None:
            return Field()
        view = (
            Flex.col()
            # .push(Text(f"{cover.width}x{cover.height}"), align=Align.CENTER)
            .push(Container(Image(cover)).horizontal(Align.CENTER).margins(top=1))
        )
        return Field(view=view)

    async def _tab_handler(self, _sweep: Sweep[Song], tag: str) -> None:
        await self.view_switch()


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
        sweep_cmd.extend(["kitty", "--class", "org.aslpavel.sweep.mpd"])
    sweep_cmd.extend(shlex.split(opts.sweep) if opts.sweep else sweep_default_cmd())

    async with MPD() as mpd:
        async with Sweep[Song](
            sweep=sweep_cmd,
            scorer="substr",
            tty=opts.tty,
            theme=sweep_theme,
            log=opts.log,
            title="MPD Client",
            **sweep_args,
        ) as sweep:
            await MPDSweep(mpd, sweep).run()


if __name__ == "__main__":
    asyncio.run(main())
