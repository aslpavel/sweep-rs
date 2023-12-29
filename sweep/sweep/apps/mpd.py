"""Application launcher

Lists all available desktop entries on the system
"""
# pyright: strict
from __future__ import annotations

import argparse
import asyncio
import re
import shlex
import os
import io
import traceback
from enum import Enum
from datetime import datetime
from PIL import Image as PILImage
from PIL.Image import Resampling
from dataclasses import dataclass
from typing import (
    Any,
    AsyncIterator,
    Awaitable,
    Callable,
    Iterable,
    List,
    NamedTuple,
    Sequence,
    cast,
    Optional,
    Dict,
)

from sweep import BindHandler, Event, SweepSelect

from .. import Candidate, Icon, Sweep, Field, Image, Container, Align, Flex
from . import sweep_default_cmd

# material-rocket-launch-outline
PROMPT_ICON = Icon(
    view_box=(0, 0, 128, 128),
    size=(1, 3),
    path="M52.02 33.84L88.80 22.49Q91.32 21.65 93.43 23.22Q95.53 24.80 95.53 27.53L95.53 27.53L95.53 81.55Q95.53 86.17 92.69 89.75Q89.85 93.32 85.44 94.37Q81.02 95.42 76.82 93.53Q72.62 91.64 70.62 87.54Q68.62 83.44 69.57 78.92Q70.52 74.40 73.98 71.57Q77.45 68.73 82.08 68.52Q86.70 68.31 90.27 71.25L90.27 71.25L90.27 43.09L53.49 54.65L53.49 92.06Q53.49 96.68 50.65 100.26Q47.82 103.83 43.40 104.88Q38.99 105.93 34.78 104.04Q30.58 102.15 28.58 98.05Q26.59 93.95 27.53 89.43Q28.48 84.91 31.95 82.08Q35.42 79.24 40.04 79.03Q44.66 78.82 48.24 81.76L48.24 81.76L48.24 38.88Q48.24 37.20 49.29 35.84Q50.34 34.47 52.02 33.84L52.02 33.84ZM53.49 38.88L53.49 49.18L90.27 37.62L90.27 27.32L53.49 38.88ZM40.46 84.28L40.46 84.28Q37.10 84.28 34.78 86.59Q32.47 88.91 32.47 92.16Q32.47 95.42 34.78 97.73Q37.10 100.05 40.35 100.05Q43.61 100.05 45.92 97.73Q48.24 95.42 48.24 92.16Q48.24 88.91 45.92 86.59Q43.61 84.28 40.46 84.28ZM74.51 81.76L74.51 81.55Q74.51 84.91 76.82 87.22Q79.13 89.54 82.39 89.54Q85.65 89.54 87.96 87.22Q90.27 84.91 90.27 81.66Q90.27 78.40 87.96 76.09Q85.65 73.77 82.39 73.77Q79.13 73.77 76.82 76.09Q74.51 78.40 74.51 81.76L74.51 81.76Z",
)
# fluent-play
PLAY_ICON_REF = 1
PLAY_ICON = Icon(
    view_box=(0, 0, 128, 128),
    size=(1, 3),
    path="M49.50 27.53L102.04 56.33Q103.72 57.38 104.88 59.27Q106.04 61.16 106.04 63.26Q106.04 65.37 104.88 67.26Q103.72 69.15 102.04 70.20L102.04 70.20L49.50 99.00Q47.61 100.05 45.50 100.05Q43.40 100.05 41.62 99.00Q39.83 97.94 38.78 96.05Q37.73 94.16 37.73 92.06L37.73 92.06L37.73 34.47Q37.73 32.37 38.78 30.48Q39.83 28.58 41.62 27.53Q43.40 26.48 45.50 26.48Q47.61 26.48 49.50 27.53L49.50 27.53ZM46.98 94.37L99.31 65.58Q100.78 64.74 100.78 63.26Q100.78 61.79 99.31 60.95L99.31 60.95L46.98 32.16Q45.50 31.32 44.24 32.05Q42.98 32.79 42.98 34.47L42.98 34.47L42.98 92.06Q42.98 93.74 44.24 94.48Q45.50 95.21 46.98 94.37L46.98 94.37Z",
)
# fluent-pause
PAUSE_ICON_REF = 2
PAUSE_ICON = Icon(
    view_box=(0, 0, 128, 128),
    size=(1, 3),
    path="M48.24 21.23L37.73 21.23Q33.31 21.23 30.27 24.28Q27.22 27.32 27.22 31.74L27.22 31.74L27.22 94.79Q27.22 99.21 30.27 102.25Q33.31 105.30 37.73 105.30L37.73 105.30L48.24 105.30Q52.65 105.30 55.70 102.25Q58.75 99.21 58.75 94.79L58.75 94.79L58.75 31.74Q58.75 27.32 55.70 24.28Q52.65 21.23 48.24 21.23L48.24 21.23ZM32.47 94.79L32.47 31.74Q32.47 29.64 34.05 28.06Q35.63 26.48 37.73 26.48L37.73 26.48L48.24 26.48Q50.34 26.48 51.91 28.06Q53.49 29.64 53.49 31.74L53.49 31.74L53.49 94.79Q53.49 96.89 51.91 98.47Q50.34 100.05 48.24 100.05L48.24 100.05L37.73 100.05Q35.63 100.05 34.05 98.47Q32.47 96.89 32.47 94.79L32.47 94.79ZM90.27 21.23L79.76 21.23Q75.35 21.23 72.30 24.28Q69.25 27.32 69.25 31.74L69.25 31.74L69.25 94.79Q69.25 99.21 72.30 102.25Q75.35 105.30 79.76 105.30L79.76 105.30L90.27 105.30Q94.69 105.30 97.73 102.25Q100.78 99.21 100.78 94.79L100.78 94.79L100.78 31.74Q100.78 27.32 97.73 24.28Q94.69 21.23 90.27 21.23L90.27 21.23ZM74.51 94.79L74.51 31.74Q74.51 29.64 76.09 28.06Q77.66 26.48 79.76 26.48L79.76 26.48L90.27 26.48Q92.37 26.48 93.95 28.06Q95.53 29.64 95.53 31.74L95.53 31.74L95.53 94.79Q95.53 96.89 93.95 98.47Q92.37 100.05 90.27 100.05L90.27 100.05L79.76 100.05Q77.66 100.05 76.09 98.47Q74.51 96.89 74.51 94.79L74.51 94.79Z",
)
# fluent-window-play
PLAYLIST_ICON_REF = 3
PLAYLIST_ICON = Icon(
    view_box=(0, 0, 128, 128),
    size=(1, 3),
    path="M100.78 61.16L100.78 39.51Q100.78 34.26 96.89 30.37Q93.00 26.48 87.54 26.48L87.54 26.48L40.46 26.48Q35.00 26.48 31.11 30.37Q27.22 34.26 27.22 39.72L27.22 39.72L27.22 86.80Q27.22 92.27 31.11 96.16Q35.00 100.05 40.25 100.05L40.25 100.05L61.90 100.05Q60.64 97.52 59.80 94.79L59.80 94.79L40.46 94.79Q37.10 94.79 34.78 92.48Q32.47 90.17 32.47 86.80L32.47 86.80L32.47 47.50L95.53 47.50L95.53 59.06Q98.26 59.90 100.78 61.16L100.78 61.16ZM40.46 31.74L40.46 31.74L87.75 31.74Q90.90 31.74 93.22 34.05Q95.53 36.36 95.53 39.72L95.53 39.72L95.53 42.25L32.47 42.25L32.47 39.51Q32.47 36.36 34.78 34.05Q37.10 31.74 40.46 31.74ZM111.29 86.80L111.29 86.80Q111.29 93.32 108.14 98.78Q104.99 104.25 99.52 107.40Q94.06 110.56 87.65 110.56Q81.23 110.56 75.77 107.40Q70.31 104.25 67.15 98.78Q64 93.32 64 86.91Q64 80.50 67.15 75.03Q70.31 69.57 75.77 66.42Q81.23 63.26 87.65 63.26Q94.06 63.26 99.52 66.42Q104.99 69.57 108.14 75.03Q111.29 80.50 111.29 86.80ZM99.31 84.70L99.31 84.70L83.76 75.88Q82.29 75.24 81.02 75.98Q79.76 76.72 79.76 78.19L79.76 78.19L79.76 95.63Q79.76 97.10 81.02 97.84Q82.29 98.57 83.76 97.94L83.76 97.94L99.31 89.12Q100.57 88.49 100.57 86.91Q100.57 85.33 99.31 84.70Z",
)
# fluent-drawer-play
DATABASE_ICON_REF = 4
DATABASE_ICON = Icon(
    view_box=(0, 0, 128, 128),
    size=(1, 3),
    path="M100.78 62.00L100.78 62.00L100.78 89.54Q100.78 96.05 96.16 100.68Q91.53 105.30 85.02 105.30L85.02 105.30L42.98 105.30Q36.47 105.30 31.84 100.68Q27.22 96.05 27.22 89.54L27.22 89.54L27.22 47.50Q27.22 40.99 31.84 36.36Q36.47 31.74 42.98 31.74L42.98 31.74L54.54 31.74Q53.91 34.26 53.70 36.99L53.70 36.99L42.98 36.99Q38.57 36.99 35.52 40.04Q32.47 43.09 32.47 47.50L32.47 47.50L54.54 47.50Q55.38 50.23 56.64 52.76L56.64 52.76L32.47 52.76L32.47 68.52L50.97 68.52Q52.02 68.52 52.76 69.25Q53.49 69.99 53.49 71.04L53.49 71.04Q53.49 75.45 56.54 78.50Q59.59 81.55 64 81.55Q68.41 81.55 71.46 78.50Q74.51 75.45 74.51 71.04L74.51 71.04Q74.51 69.99 75.24 69.25Q75.98 68.52 77.03 68.52L77.03 68.52L95.53 68.52L95.53 65.37Q98.26 63.89 100.78 62.00ZM42.98 100.05L85.02 100.05Q89.43 100.05 92.48 97.00Q95.53 93.95 95.53 89.54L95.53 89.54L95.53 73.77L79.55 73.77Q78.50 79.45 74.09 83.23Q69.67 87.01 64 87.01Q58.33 87.01 53.91 83.23Q49.50 79.45 48.45 73.77L48.45 73.77L32.47 73.77L32.47 89.54Q32.47 93.95 35.52 97.00Q38.57 100.05 42.98 100.05L42.98 100.05ZM82.29 63.26L82.50 63.26Q88.80 63.26 94.27 60.11Q99.73 56.96 102.88 51.49Q106.04 46.03 106.04 39.62Q106.04 33.21 102.88 27.74Q99.73 22.28 94.27 19.13Q88.80 15.97 82.39 15.97Q75.98 15.97 70.52 19.13Q65.05 22.28 61.90 27.74Q58.75 33.21 58.75 39.62Q58.75 46.03 61.90 51.49Q65.05 56.96 70.52 60.11Q75.98 63.26 82.29 63.26L82.29 63.26ZM78.50 28.58L78.50 28.58L94.06 37.41Q95.32 38.04 95.32 39.62Q95.32 41.20 94.06 41.83L94.06 41.83L78.50 50.65Q77.03 51.28 75.77 50.55Q74.51 49.81 74.51 48.34L74.51 48.34L74.51 30.90Q74.51 29.43 75.77 28.69Q77.03 27.95 78.50 28.58Z",
)
DATE_RE = re.compile("(\\d{4})-?(\\d{2})?-?(\\d{2})?")


class PlayState(Enum):
    PAUSE = "pause"
    PLAY = "play"
    STOP = "stop"


@dataclass
class Song:
    file: str
    duration: float
    artist: Optional[str]
    album: Optional[str]
    title: Optional[str]
    date: Optional[datetime]
    track: Optional[int]
    attrs: Dict[str, str]
    pos: Optional[int]  # position in the playlist
    id: Optional[int]  # song id in the playlist
    current: Optional[MPDStatus]  # if song is currently playing

    def __init__(self, file: str) -> None:
        self.file = file
        self.duration = 0.0
        self.artist = None
        self.album = None
        self.title = None
        self.date = None
        self.track = None
        self.attrs = {}
        self.pos = None
        self.id = None
        self.current = None

    def __eq__(self, other: Any) -> bool:
        if not isinstance(other, Song):
            return False
        return self.file == other.file

    def __hash__(self) -> int:
        return hash(self.file)

    def album_id(self) -> int:
        return abs(hash(self.attrs.get("MUSICBRAINZ_ALBUMID") or self.album or ""))

    @staticmethod
    async def from_chunks(chunks: AsyncIterator[MPDChunk]) -> AsyncIterator[Song]:
        """Parse songs from *info commands"""
        song = Song("")
        async for chunk in chunks:
            match chunk.name:
                case "file":
                    if song.file:
                        yield song
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
                case "Date" | "OriginalDate":
                    match = DATE_RE.match(cast(str, chunk.data).strip())
                    if match is None:
                        continue
                    year = int(match.group(1))
                    month = int(match.group(2)) if match.group(2) else 1
                    if month > 12 or month < 1:
                        month = 1
                    day = int(match.group(3)) if match.group(3) else 1
                    if day > 31 or day < 1:
                        day = 1
                    if song.date and chunk.name == "Date":
                        continue
                    song.date = datetime(year, month, day)
                case "Track":
                    song.track = int(chunk.data)
                case "Pos":
                    song.pos = int(chunk.data)
                case "Id":
                    song.id = int(chunk.data)
                case name:
                    song.attrs[name] = cast(str, chunk.data)
        if song.file:
            yield song

    def to_candidate(self) -> Candidate:
        result = Candidate()

        # target
        face = "underline" if self.current is not None else None
        if self.track:
            result.target_push(f"{self.track:>02}. ", active=False)
        if self.title:
            result.target_push(self.title, face=face)
        else:
            result.target_push(os.path.basename(self.file), face=face)

        # right
        if self.current is not None:
            if self.current.state == PlayState.PAUSE:
                result.right_push(ref=PAUSE_ICON_REF)
            elif self.current.state == PlayState.PLAY:
                result.right_push(ref=PLAY_ICON_REF)
        result.right_push(duration_fmt(self.duration))

        # preview
        if self.artist:
            result.preview_push("Artist: ", face="bold").preview_push(
                f"{self.artist}\n", active=True
            )
        if self.album:
            result.preview_push("Album : ", face="bold")
            date = self.date
            if date is not None:
                result.preview_push(f"[{date.year}] ")
            result.preview_push(f"{self.album}\n", active=True)
        result.preview_push(ref=self.album_id()).preview_flex_set(1)

        return result


@dataclass
class Album:
    name: str
    date: Optional[datetime]
    songs: List[Song]

    def __init__(self, name: str, date: Optional[datetime]) -> None:
        self.name = name
        self.date = date
        self.songs = []


@dataclass
class Database:
    artists: Dict[str, Dict[str, Album]]

    def __init__(self) -> None:
        self.artists = {}

    def add(self, song: Song) -> None:
        artist = song.artist or ""
        album_name = song.album or ""
        albums = self.artists.get(artist)
        if albums is None:
            albums = dict[str, Album]()
            self.artists[artist] = albums
        album = albums.get(album_name)
        if album is None:
            album = Album(album_name, song.date)
            albums[album_name] = album
        album.songs.append(song)

    def songs(
        self,
        artist: Optional[str] = None,
        album: Optional[str] = None,
    ) -> List[Song]:
        artists: Iterable[Dict[str, Album]]
        if artist is None:
            artists = self.artists.values()
        else:
            artist_albums = self.artists.get(artist)
            if not artist_albums:
                return []
            artists = [artist_albums]
        albums: Iterable[Album] = (
            album for albums in artists for album in albums.values()
        )
        if album is not None:
            albums = (album_obj for album_obj in albums if album_obj.name == album)
        return [song for album in albums for song in album.songs]


class MPDEvent(Enum):
    DATABASE = "database"
    UPDATE = "update"
    STORED_PLAYLIST = "stored_playlist"
    PLAYLIST = "playlist"
    PLAYER = "player"
    MIXER = "mixer"
    OUTPUT = "output"
    OPTIONS = "options"
    PARTITION = "partition"
    STICKER = "sticker"
    SUBSCRIPTION = "subscription"
    MESSAGE = "message"
    NEIGHBOUR = "neighbour"
    MOUNT = "mount"


class MPDStatus(NamedTuple):
    volume: int
    repeat: bool
    random: bool
    single: bool
    playlist_version: int
    playlist_length: int
    state: PlayState
    elapsed: Optional[float]
    duration: Optional[float]
    playlist_song: Optional[int]
    playlist_song_id: Optional[int]


class MPDState(Enum):
    WAIT = 0
    IDLE = 1
    REQUEST = 2


class MPDChunk(NamedTuple):
    """Single chunk of data returned as response"""

    name: str
    data: str | bytes

    def get_bytes(self) -> bytes:
        """Return bytes if it was binary data"""
        if self.name == "binary":
            return cast(bytes, self.data)
        return b""

    def __repr__(self) -> str:
        if self.name == "binary":
            return f"binary={len(self.data)}"
        return f"{self.name}={self.data}"


class MPD:
    """MPD Client implementation"""

    __slots__ = [
        "events",
        "_host",
        "_port",
        "_database",
        "_reader",
        "_writer",
        "_state",
        "_state_cond",
        "_idle_task",
        "_album_id_to_song",
    ]

    def __init__(self, host: str = "localhost", port: int = 6600):
        self.events = Event[MPDEvent]()
        self._host = host
        self._port = port
        self._database: Optional[Database] = None

        self._reader: Optional[asyncio.StreamReader] = None
        self._writer: Optional[asyncio.StreamWriter] = None

        self._state = MPDState.WAIT
        self._state_cond = asyncio.Condition()
        self._idle_task: Optional[asyncio.Task[None]] = None

        self._album_id_to_song: Dict[int, Song] = {}

    async def __aenter__(self) -> MPD:
        self._reader, self._writer = await asyncio.open_connection(
            self._host, self._port
        )
        init = await self._reader.readline()
        init = init.strip()
        if not init.startswith(b"OK MPD"):
            raise RuntimeError(f"invalid initial response from the MPD: {init}")
        self._idle_task = asyncio.create_task(self._idle_coro(), name="mpd-idle")
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
        while self._reader is not None and self._writer is not None:
            await asyncio.sleep(0.1)
            async for chunk in self._call("idle"):
                if chunk.name == "changed":
                    try:
                        event = MPDEvent(chunk.data)
                        if event == MPDEvent.DATABASE:
                            self._database = None
                        self.events(event)
                    except ValueError:
                        pass

    async def _call(self, cmd: str, *args: str) -> AsyncIterator[MPDChunk]:
        """Issue MPD command"""
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

    async def _call_dict(self, cmd: str, *args: str) -> Dict[str, str]:
        """Issue MPD command and collect result to a dictionary"""
        attrs: Dict[str, str] = {}
        async for chunk in self._call(cmd, *args):
            attrs[chunk.name] = cast(str, chunk.data)
        return attrs

    def song_by_id(self, id: int) -> Optional[Song]:
        return self._album_id_to_song.get(id)

    async def database(self) -> Database:
        if self._database is not None:
            return self._database
        database = Database()
        async for song in Song.from_chunks(self._call("listallinfo")):
            database.add(song)
            self._album_id_to_song[song.album_id()] = song
        self._database = database
        return database

    async def play(self, song: Song) -> None:
        if song.id is None:
            return
        await self._call_dict("playid", str(song.id))

    async def pause(self, pause: Optional[bool] = None) -> None:
        """Pause/Resume playback, if pause is not set then toggle"""
        if pause is None:
            await self._call_dict("pause")
        else:
            await self._call_dict("pause", str(int(pause)))

    async def add(
        self,
        song: Song,
        pos: Optional[int] = None,
        relative: bool = False,
        allow_dup: bool = False,
    ) -> Optional[int]:
        """Add song to the playlist"""
        if not allow_dup:
            files = {song.file for song in await self.playlistinfo()}
            if song.file in files:
                return

        cmd = "addid"
        if pos is None:
            attrs = await self._call_dict(cmd, song.file)
        elif relative:
            pos_str = str(pos) if pos < 0 else f"+{pos}"
            attrs = await self._call_dict(cmd, song.file, pos_str)
        else:
            if pos < 0:
                raise ValueError("position must positivie if relative is not set")
            attrs = await self._call_dict(cmd, song.file, str(pos))
        return int(attrs["Id"])

    async def delete(self, song: Song):
        """Remove song from the playlist"""
        if song.id is None:
            return
        await self._call_dict("deleteid", str(song.id))

    async def move(self, song: Song, pos: int, relative: bool = True):
        if song.pos is None:
            return
        pos = song.pos + pos if relative else pos
        status = await self.status()
        pos = min(max(0, pos), status.playlist_length - 1)
        await self._call_dict("move", str(song.pos), str(pos))

    async def status(self) -> MPDStatus:
        attrs = await self._call_dict("status")
        elapsed_opt = attrs.get("elapsed")
        duration_opt = attrs.get("duration")
        playlist_song_opt = attrs.get("song")
        playlist_song_id_opt = attrs.get("songid")
        return MPDStatus(
            volume=int(attrs["volume"]),
            repeat=bool(attrs["repeat"]),
            random=bool(attrs["random"]),
            single=bool(attrs["single"]),
            playlist_version=int(attrs["playlist"]),
            playlist_length=int(attrs["playlistlength"]),
            state=PlayState(attrs["state"]),
            elapsed=float(elapsed_opt) if elapsed_opt else None,
            duration=float(duration_opt) if duration_opt else None,
            playlist_song=int(playlist_song_opt) if playlist_song_opt else None,
            playlist_song_id=int(playlist_song_id_opt)
            if playlist_song_id_opt
            else None,
        )

    async def currentsong(self) -> Optional[Song]:
        async for song in Song.from_chunks(self._call("currentsong")):
            return song

    async def playlistinfo(self) -> List[Song]:
        status = await self.status()
        songs: List[Song] = []
        async for song in Song.from_chunks(self._call("playlistinfo")):
            if song.id == status.playlist_song_id:
                song.current = status
            self._album_id_to_song[song.album_id()] = song
            songs.append(song)
        return songs

    async def listallinfo(self) -> List[Song]:
        database = await self.database()
        return database.songs()

    async def readpicture(
        self, file: str, width: int = 500
    ) -> Optional[PILImage.Image]:
        """Read picture embedded in music file"""
        cmd = "readpicture"
        size = 0
        data = io.BytesIO()
        async for chunk in self._call(cmd, file, "0"):
            if chunk.name == "size":
                size = int(chunk.data)
            else:
                data.write(chunk.get_bytes())
        while data.tell() < size:
            async for chunk in self._call(cmd, file, str(data.tell())):
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
    if " " not in value and "'" not in value and '"' not in value:
        return value
    value_escaped = value.replace("\\", "\\\\").replace("'", "\\'").replace('"', '\\"')
    return f'"{value_escaped}"'


def duration_fmt(duration: float) -> str:
    mins, secs = divmod(duration, 60)
    hours, mins = divmod(mins, 60)
    result = f"{mins:>02.0f}:{secs:>02.0f}"
    if hours:
        result = f"{hours:>02.0f}:{result}"
    return result


class MPDSweepView(Enum):
    PLAYLIST = 0
    SONGS = 1
    MAX = 2


class MPDSweep:
    def __init__(self, mpd: MPD, sweep: Sweep[Song]) -> None:
        self._mpd = mpd
        self._sweep = sweep
        self._view = MPDSweepView.MAX
        self._events_queue = asyncio.Queue[MPDEvent]()

    async def run(self) -> None:
        # fields
        await self._sweep.field_register(Field(glyph=PLAY_ICON), PLAY_ICON_REF)
        await self._sweep.field_register(Field(glyph=PAUSE_ICON), PAUSE_ICON_REF)
        await self._sweep.field_register(Field(glyph=PLAYLIST_ICON), PLAYLIST_ICON_REF)
        await self._sweep.field_register(Field(glyph=DATABASE_ICON), DATABASE_ICON_REF)
        self._sweep.field_resolver_set(self._field_resolver)

        # binds
        def handler(fn: Callable[[], Awaitable[None]]) -> BindHandler[Song]:
            return lambda _sweep, _tag: fn()

        await self._sweep.bind(
            key="ctrl+i",
            tag="mpd.switch.view",
            desc="Switch between different views",
            handler=handler(self.view_switch),
        )
        await self._sweep.bind(
            key="alt+g r",
            tag="mpd.goto.artist",
            desc="Go to artist",
            handler=handler(self._goto_artist),
        )
        await self._sweep.bind(
            key="alt+g a",
            tag="mpd.goto.album",
            desc="Go to album",
            handler=handler(self._goto_album),
        )
        await self._sweep.bind(
            key="alt+d",
            tag="mpd.song.delete",
            desc="Delete song from the playlist",
            handler=handler(self._playlist_song_delete),
        )
        await self._sweep.bind(
            key="shift+up",
            tag="mpd.song.moveup",
            desc="Move song up in the playlist",
            handler=handler(self._playlist_song_move_up),
        )
        await self._sweep.bind(
            key="shift+down",
            tag="mpd.song.movedown",
            desc="Move song down in the playlist",
            handler=handler(self._playlist_song_move_down),
        )

        # events enqueue
        @self._mpd.events.on
        def _(event: MPDEvent) -> bool:
            self._events_queue.put_nowait(event)
            return True

        update_task = asyncio.create_task(self._updater_coro())
        await self.view_playlist()
        try:
            async for event in self._sweep:
                if isinstance(event, SweepSelect):
                    await self._on_select(event.items)
        finally:
            update_task.cancel()

    async def view_switch(self, view: Optional[MPDSweepView] = None) -> None:
        match view:
            case None | MPDSweepView.MAX:
                view = MPDSweepView((self._view.value + 1) % MPDSweepView.MAX.value)
                await self.view_switch(view)
            case MPDSweepView.SONGS:
                await self.view_songs()
            case MPDSweepView.PLAYLIST:
                await self.view_playlist()

    async def view_playlist(self) -> None:
        """Switch to Playlist view"""
        self._view = MPDSweepView.PLAYLIST
        await self._sweep.prompt_set("Playlist", icon=PLAYLIST_ICON)
        await self._sweep.items_clear()
        await self._sweep.query_set("")
        songs = await self._mpd.playlistinfo()
        await self._sweep.items_extend(songs)
        status = await self._mpd.status()
        if status.playlist_song is not None and songs:
            # wait for ranker to complete
            while (await self._sweep.items_current()) is None:
                pass
            await self._sweep.cursor_set(status.playlist_song)

    async def view_songs(
        self,
        songs: Optional[Sequence[Song]] = None,
        prompt: Optional[str] = None,
    ) -> None:
        """Switch to set view"""
        self._view = MPDSweepView.SONGS
        await self._sweep.prompt_set(prompt or "Songs", icon=DATABASE_ICON)
        await self._sweep.items_clear()
        await self._sweep.query_set("")
        if songs is None:
            songs = await self._mpd.listallinfo()
        await self._sweep.items_extend(songs)

    async def _on_select(self, songs: List[Song]) -> None:
        match self._view:
            case MPDSweepView.PLAYLIST:
                if len(songs) != 1:
                    return
                song = songs[0]
                if song.id is not None:
                    current = await self._mpd.currentsong()
                    if current == song:
                        await self._mpd.pause()
                    else:
                        await self._mpd.play(song)
            case MPDSweepView.SONGS:
                for song in songs:
                    await self._mpd.add(song)
            case _:
                pass

    async def _updater_coro(self) -> None:
        try:
            while True:
                event = await self._events_queue.get()
                if self._view == MPDSweepView.PLAYLIST:
                    if event not in {MPDEvent.PLAYER, MPDEvent.PLAYLIST}:
                        continue
                    await self.view_playlist()
        except Exception:
            await self._sweep.terminate()
            traceback.print_exc()

    async def _field_resolver(self, ref: int) -> Field:
        song = self._mpd.song_by_id(ref)
        if song is None:
            return Field()
        cover = await self._mpd.readpicture(song.file)
        if cover is None:
            return Field()
        view = Flex.col().push(
            Container(Image(cover)).horizontal(Align.CENTER).margins(top=1)
        )
        return Field(view=view)

    async def _playlist_song_delete(self) -> None:
        if self._view != MPDSweepView.PLAYLIST:
            return
        songs = await self._sweep.items_marked() or [await self._sweep.items_current()]
        for song in songs:
            if song is None or song.id is None:
                return
            await self._mpd.delete(song)

    async def _playlist_song_move_up(self) -> None:
        if self._view != MPDSweepView.PLAYLIST:
            return
        song = await self._sweep.items_current()
        if song is None:
            return
        await self._mpd.move(song, -1)

    async def _playlist_song_move_down(self) -> None:
        if self._view != MPDSweepView.PLAYLIST:
            return
        song = await self._sweep.items_current()
        if song is None:
            return
        await self._mpd.move(song, 1)

    async def _goto_artist(self) -> None:
        song = await self._sweep.items_current()
        if song is None:
            return
        db = await self._mpd.database()
        await self.view_songs(db.songs(artist=song.artist or ""), song.artist)

    async def _goto_album(self) -> None:
        song = await self._sweep.items_current()
        if song is None:
            return
        db = await self._mpd.database()
        await self.view_songs(
            db.songs(artist=song.artist or "", album=song.album or ""),
            song.album,
        )


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
        "--term",
        default="kitty",
        choices=["kitty", "foot", "none"],
        help="terminal window used to show UI",
    )
    parser.add_argument(
        "--no-window", action="store_true", help="do not create new terminal window"
    )
    opts = parser.parse_args(args)

    sweep_theme = opts.theme
    sweep_args: Dict[str, Any] = {}
    sweep_cmd: List[str] = []
    if opts.term != "none" and opts.tty is None:
        sweep_theme = sweep_theme or "dark"
        sweep_args.update(
            dict(
                altscreen=True,
                height=1024,
                tmp_socket=True,
                border=0,
            )
        )
        if opts.term == "kitty":
            sweep_cmd.extend(["kitty", "--class", "org.aslpavel.sweep.mpd"])
        elif opts.term == "foot":
            sweep_cmd.extend(["foot", "--app-id", "org.aslpavel.sweep.mpd"])
    sweep_cmd.extend(shlex.split(opts.sweep) if opts.sweep else sweep_default_cmd())

    async with MPD() as mpd:
        async with Sweep[Song](
            sweep=sweep_cmd,
            scorer="substr",
            tty=opts.tty,
            theme=sweep_theme,
            log=opts.log,
            title="MPD Client",
            keep_order=True,
            **sweep_args,
        ) as sweep:
            await MPDSweep(mpd, sweep).run()


if __name__ == "__main__":
    asyncio.run(main())
