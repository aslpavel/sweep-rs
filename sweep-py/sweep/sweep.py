#!/usr/bin/env python3
"""Asynchronous JSON-RPC implementation to communicate with sweep command"""

# pyright: strict
from __future__ import annotations

import asyncio
import base64
import inspect
import json
import os
import socket
import sys
import tempfile
import time
import warnings
from abc import ABC, abstractmethod
from asyncio import CancelledError, Future, StreamReader, StreamWriter
from asyncio.subprocess import Process
from asyncio.tasks import Task
from collections import deque
from collections.abc import (
    AsyncGenerator,
    AsyncIterator,
    Awaitable,
    Callable,
    Coroutine,
    Generator,
    Iterable,
)
from contextlib import asynccontextmanager
from dataclasses import dataclass
from enum import Enum
from functools import partial
from typing import (
    Any,
    NamedTuple,
    Protocol,
    TypedDict,
    Unpack,
    cast,
    override,
    runtime_checkable,
)

__all__ = [
    "Align",
    "Bind",
    "BindHandler",
    "Candidate",
    "Container",
    "Direction",
    "Event",
    "Field",
    "Flex",
    "Icon",
    "IconFrame",
    "Image",
    "Justify",
    "Size",
    "sweep",
    "Sweep",
    "SweepBind",
    "SweepEvent",
    "SweepSelect",
    "SweepSize",
    "Text",
    "View",
    "ViewRef",
]

# ------------------------------------------------------------------------------
# Sweep
# ------------------------------------------------------------------------------


class Size(NamedTuple):
    height: int
    width: int

    @staticmethod
    def from_json(obj: Any) -> Size:
        height = None
        width = None
        if isinstance(obj, list):
            obj = cast(list[Any], obj)
            height, width = obj
        elif isinstance(obj, dict):
            obj = cast(dict[str, Any], obj)
            height = obj.get("height")
            width = obj.get("width")
        if (
            not isinstance(height, int)
            or not isinstance(width, int)
            or height < 0
            or width < 0
        ):
            raise ValueError(f"Invalid Size: {obj}")
        return Size(height, width)


class SweepBind(NamedTuple):
    """Event generated on bound key press"""

    tag: str
    key: str | None

    def __repr__(self):
        return f"SweepBind(tag={self.tag}, key={self.key})"


class SweepSize(NamedTuple):
    cells: Size
    pixels: Size
    pixels_per_cell: Size

    def cells_in_pixels(self, cells: Size) -> Size:
        return Size(
            height=self.pixels_per_cell.height * cells.height,
            width=self.pixels_per_cell.width * cells.width,
        )

    @staticmethod
    def from_json(obj: Any) -> SweepSize:
        if not isinstance(obj, dict):
            raise ValueError(f"Invalid SweepSize: {obj}")
        obj = cast(dict[str, Any], obj)
        cells = Size.from_json(obj.get("cells"))
        pixels = Size.from_json(obj.get("pixels"))
        pixels_per_cell = Size.from_json(obj.get("pixels_per_cell"))
        return SweepSize(cells, pixels, pixels_per_cell)


@dataclass
class SweepSelect[I]:
    """Event generated on item(s) select"""

    items: list[I]

    def __init__(self, items: list[I]):
        self.items = items


@dataclass
class Field:
    """Filed structure used to construct `Candidate`"""

    text: str = ""
    glyph: Icon | None = None
    view: View | None = None
    active: bool = True
    face: str | None = None
    ref: int | None = None

    def __repr__(self) -> str:
        attrs: list[str] = []
        if self.text:
            attrs.append(f"text={repr(self.text)}")
        if not self.active:
            attrs.append(f"active={self.active}")
        if self.glyph is not None:
            attrs.append(f"glyph={self.glyph}")
        if self.view is not None:
            attrs.append(f"view={self.view}")
        if self.face is not None:
            attrs.append(f"face={self.face}")
        if self.ref is not None:
            attrs.append(f"ref={self.ref}")
        return f'Field({", ".join(attrs)})'

    def to_json(self) -> dict[str, Any]:
        """Convert field to JSON"""
        obj: dict[str, Any] = {}
        if self.text:
            obj["text"] = self.text
        if not self.active:
            obj["active"] = False
        if self.glyph:
            obj["glyph"] = self.glyph.to_json()
        if self.view:
            obj["view"] = self.view.to_json()
        if self.face:
            obj["face"] = self.face
        if self.ref is not None:
            obj["ref"] = self.ref
        return obj

    @staticmethod
    def from_json(obj: Any) -> Field | None:
        """Create field from JSON object"""
        if not isinstance(obj, dict):
            return
        obj = cast(dict[str, Any], obj)
        active = obj.get("active")
        return Field(
            text=obj.get("text") or "",
            active=True if active is None else active,
            glyph=Icon.from_json(obj.get("glyph")),
            face=obj.get("face"),
            ref=obj.get("ref"),
        )


@runtime_checkable
class ToCandidate(Protocol):
    def to_candidate(self) -> Candidate: ...


@dataclass(slots=True)
class Candidate:
    """Convenient sweep item implementation"""

    target: list[Field] | None = None
    extra: dict[str, Any] | None = None
    right: list[Field] | None = None
    right_offset: int = 0
    right_face: str | None = None
    preview: list[Any] | None = None
    preview_flex: float = 0.0
    hotkey: str | None = None

    def to_candidate(self) -> Candidate:
        return self

    def target_push(
        self,
        text: str = "",
        active: bool = True,
        glyph: Icon | None = None,
        view: View | None = None,
        face: str | None = None,
        ref: int | None = None,
    ) -> Candidate:
        """Add field to the target (matchable left side text)"""
        if self.target is None:
            self.target = []
        self.target.append(Field(text, glyph, view, active, face, ref))
        return self

    def right_push(
        self,
        text: str = "",
        active: bool = False,
        glyph: Icon | None = None,
        view: View | None = None,
        face: str | None = None,
        ref: int | None = None,
    ) -> Candidate:
        """Add field to the right (unmatchable right side text)"""
        if self.right is None:
            self.right = []
        self.right.append(Field(text, glyph, view, active, face, ref))
        return self

    def right_offset_set(self, offset: int) -> Candidate:
        """Set offset for the right side text"""
        self.right_offset = offset
        return self

    def right_face_set(self, face: str) -> Candidate:
        """Set face used to fill right side text"""
        self.right_face = face
        return self

    def preview_push(
        self,
        text: str = "",
        active: bool = False,
        glyph: Icon | None = None,
        view: View | None = None,
        face: str | None = None,
        ref: int | None = None,
    ) -> Candidate:
        """Add field to the preview (text shown when item is highlighted)"""
        if self.preview is None:
            self.preview = []
        self.preview.append(Field(text or "", glyph, view, active, face, ref))
        return self

    def preview_flex_set(self, flex: float) -> Candidate:
        """Set preview flex value"""
        self.preview_flex = flex
        return self

    def extra_update(self, **entries: Any) -> Candidate:
        """Add entries to extra field"""
        if self.extra is None:
            self.extra = {}
        self.extra.update(entries)
        return self

    def hotkey_set(self, hotkey: str) -> Candidate:
        """Assign hotkey for this candidate"""
        self.hotkey = hotkey
        return self

    def wrap[V](self, value: V) -> CandidateWrapped[V]:
        return CandidateWrapped(value, self)

    def __repr__(self) -> str:
        attrs: list[str] = []
        if self.target is not None:
            attrs.append(f"target={self.target}")
        if self.extra is not None:
            attrs.append(f"extra={self.extra}")
        if self.right is not None:
            attrs.append(f"right={self.right}")
        if self.right_offset != 0:
            attrs.append(f"right_offset={self.right_offset}")
        if self.right_face:
            attrs.append(f"right_face={self.right_face}")
        if self.preview is not None:
            attrs.append(f"preview={self.preview}")
        if self.preview_flex != 0.0:
            attrs.append(f"preview_flex={self.preview_flex}")
        if self.hotkey is not None:
            attrs.append(f"hotkey={self.hotkey}")
        return f'Candidate({", ".join(attrs)})'

    def to_json(self) -> dict[str, Any]:
        """Convert candidate to JSON object"""
        obj: dict[str, Any] = self.extra.copy() if self.extra else {}
        if self.target:
            obj["target"] = [field.to_json() for field in self.target]
        if self.right:
            obj["right"] = [field.to_json() for field in self.right]
        if self.right_offset:
            obj["right_offset"] = self.right_offset
        if self.right_face:
            obj["right_face"] = self.right_face
        if self.preview:
            obj["preview"] = [field.to_json() for field in self.preview]
        if self.preview_flex != 0.0:
            obj["preview_flex"] = self.preview_flex
        if self.hotkey is not None:
            obj["hotkey"] = self.hotkey
        return obj

    @staticmethod
    def from_json(obj: Any) -> Candidate | None:
        """Construct candidate from JSON object"""
        if isinstance(obj, str):
            return Candidate().target_push(obj)
        if not isinstance(obj, dict):
            return

        def fields_from_json(fields_obj: Any) -> list[Field] | None:
            if not isinstance(fields_obj, list):
                return None
            fields: list[Field] = []
            for field_obj in cast(list[Any], fields_obj):
                field = Field.from_json(field_obj)
                if field is None:
                    continue
                fields.append(field)
            return fields or None

        obj = cast(dict[str, Any], obj)
        target = fields_from_json(obj.pop("target", None))
        right = fields_from_json(obj.pop("right", None))
        right_offset = obj.pop("offset", None) or 0
        right_face = obj.pop("right_face", None)
        preview = fields_from_json(obj.pop("preview", None))
        preview_flex = obj.pop("preview_flex", None) or 0.0
        hotkey = obj.pop("hotkey", None)
        return Candidate(
            target=target,
            extra=obj or None,
            right=right,
            right_offset=right_offset,
            right_face=right_face,
            preview=preview,
            preview_flex=preview_flex,
            hotkey=hotkey,
        )


@dataclass
class CandidateWrapped[V]:
    value: V
    candidate: Candidate

    def to_candidate(self) -> Candidate:
        return self.candidate


type SweepEvent[I] = SweepBind | SweepSize | SweepSelect[I]
type BindHandler[I] = Callable[[Sweep[I], str], Awaitable[I | None]]
type FiledResolver = Callable[[int], Awaitable[Field | None]]
type ViewResolver = Callable[[int], Awaitable[View | None]]


@dataclass
class Bind[I]:
    """Bind structure

    If handler returns not None then this value is returned as selected
    """

    key: str
    tag: str
    desc: str
    handler: BindHandler[I]

    @staticmethod
    def decorator(key: str, tag: str, desc: str) -> Callable[[BindHandler[I]], Bind[I]]:
        """Decorator to easier define binds

        >>> @Bind.decorator("ctrl+c", "my.action", "My awesome action")
        >>> async def my_action(_sweep, _tag):
        >>>     pass
        """

        def bind_decorator(handler: BindHandler[I]) -> Bind[I]:
            return Bind(key, tag, desc, handler)

        return bind_decorator


class SweepArgs(TypedDict, total=False):
    sweep: list[str] | None
    prompt: str
    preview: str | None
    query: str | None
    nth: str | None
    delimiter: str | None
    theme: str | None
    scorer: str | None
    tty: str | None
    log: str | None
    title: str | None
    keep_order: bool
    no_match: str | None
    layout: str | None
    tmp_socket: bool
    field_resolver: FiledResolver | None
    view_resolver: ViewResolver | None


async def sweep[I](
    items: Iterable[I],
    prompt_icon: Icon | str | None = None,
    binds: list[Bind[I]] | None = None,
    fields: dict[int, Any] | None = None,
    views: dict[int, View] | None = None,
    init: Callable[[Sweep[I]], Awaitable[None]] | None = None,
    **options: Unpack[SweepArgs],
) -> list[I]:
    """Convenience wrapper around `Sweep`

    Useful when you only need to select one candidate from a list of items
    """
    async with Sweep[I](**options) as sweep:
        # setup fields
        if fields:
            await sweep.field_register_many(fields)

        if views:
            for ref, view in views.items():
                await sweep.view_register(view, ref)

        # setup binds
        for bind in binds or []:
            await sweep.bind_struct(bind)

        # setup prompt
        if isinstance(prompt_icon, str):
            icon = Icon.from_str_or_file(prompt_icon)
        else:
            icon = prompt_icon
        if icon is not None:
            await sweep.prompt_set(prompt=options.get("prompt"), icon=icon)

        # send items
        await sweep.items_extend(items)

        # init
        if init is not None:
            await init(sweep)

        # wait events
        async for event in sweep:
            if isinstance(event, SweepSelect):
                return event.items

    return []


class Sweep[I]:
    """RPC wrapper around sweep process

    DEBUGGING:
        - Load this file as python module from `python -masyncio`.
        - Open other terminal window and execute `$ tty` command, then run something that
          will not steal characters for sweep process like `$ sleep 100000`.
        - Instantiate Sweep class with the tty device path of the other terminal.
        - Now you can call all the methods of the Sweep class in an interactive mode.
        - set RUST_LOG=debug
        - specify log file
    """

    __slots__ = [
        "_args",
        "_proc",
        "_io_sock",
        "_peer",
        "_peer_iter",
        "_tmp_socket",
        "_items",
        "_binds",
        "_field_resolver",
        "_field_resolved",
        "_view_resolver",
        "_view_resolved",
        "_size",
    ]

    def __init__(
        self,
        sweep: list[str] | None = None,
        prompt: str = "INPUT",
        preview: str | None = None,
        query: str | None = None,
        nth: str | None = None,
        delimiter: str | None = None,
        theme: str | None = None,
        scorer: str | None = None,
        tty: str | None = None,
        log: str | None = None,
        title: str | None = None,
        keep_order: bool = False,
        no_match: str | None = None,
        layout: str | None = None,
        tmp_socket: bool = False,
        field_resolver: FiledResolver | None = None,
        view_resolver: ViewResolver | None = None,
    ) -> None:
        args: list[str] = []
        args.extend(["--prompt", prompt])
        if query is not None:
            args.extend(["--query", query])
        if isinstance(nth, str):
            args.extend(["--nth", nth])
        if delimiter is not None:
            args.extend(["--delimiter", delimiter])
        if theme is not None:
            args.extend(["--theme", theme])
        if scorer is not None:
            args.extend(["--scorer", scorer])
        if tty is not None:
            args.extend(["--tty", tty])
        if log is not None:
            args.extend(["--log", log])
        if title:
            args.extend(["--title", title])
        if keep_order:
            args.append("--keep-order")
        if no_match:
            args.extend(["--no-match", no_match])
        if layout:
            args.extend(["--layout", layout])
        if preview:
            args.extend(["--preview", preview])
        sweep = sweep or ["sweep"]
        self._args: list[str] = [*sweep, "--rpc", *args]
        self._proc: Process | None = None
        self._io_sock: socket.socket | None = None
        self._tmp_socket: bool = tmp_socket  # use tmp socket instead of socket pair
        self._peer: RpcPeer = RpcPeer()
        self._peer_iter: AsyncIterator[RpcRequest] = aiter(self._peer)
        self._size: SweepSize | None = None
        self._items: list[I] = []
        self._binds: dict[str, BindHandler[I]] = {}
        self._field_resolver: FiledResolver | None = field_resolver
        self._field_resolved: set[int] = set()
        self._view_resolver: ViewResolver | None = view_resolver
        self._view_resolved: set[int] = set()

    async def __aenter__(self) -> Sweep[I]:
        if self._proc is not None:
            raise RuntimeError("sweep process is already running")

        if self._tmp_socket:
            self._io_sock = await self._proc_tmp_socket()
        else:
            self._io_sock = await self._proc_pair_socket()
        reader, writer = await asyncio.open_unix_connection(sock=self._io_sock)
        create_task(self._peer.serve(reader, writer), "sweep-rpc-peer")

        return self

    async def _proc_pair_socket(self) -> socket.socket:
        """Create sweep subprocess and connect via inherited socket pair"""
        remote, local = socket.socketpair()
        prog, *args = self._args
        self._proc = await asyncio.create_subprocess_exec(
            prog,
            *[*args, "--io-socket", str(remote.fileno())],
            pass_fds=[remote.fileno()],
        )
        remote.close()
        return local

    async def _proc_tmp_socket(self) -> socket.socket:
        """Create sweep subprocess and connect via on disk socket"""
        io_sock_path = os.path.join(
            tempfile.gettempdir(),
            f"sweep-io-{os.getpid()}.socket",
        )
        if os.path.exists(io_sock_path):
            os.unlink(io_sock_path)
        io_sock_accept = unix_server_once(io_sock_path)
        prog, *args = self._args
        self._proc = await asyncio.create_subprocess_exec(
            prog,
            *[*args, "--io-socket", io_sock_path],
        )
        return await io_sock_accept

    def _item_get(self, item: Any) -> I:
        """Return stored item if it was converted to Candidate"""
        if isinstance(item, dict):
            item_dict = cast(dict[str, Any], item)
            item_index: int | None = item_dict.get("_sweep_item_index")
            if item_index is not None and item_index < len(self._items):
                return self._items[item_index]  # type: ignore
        return cast(I, item)

    async def __aexit__(self, _et: Any, ev: Any, _tb: Any) -> bool:
        await self.terminate()
        if isinstance(ev, CancelledError):
            return True
        return False

    def __aiter__(self) -> AsyncIterator[SweepEvent[I]]:
        async def event_iter() -> AsyncGenerator[SweepEvent[I], None]:
            async for event in self._peer_iter:
                if not isinstance(event.params, dict):
                    continue
                if event.method == "select":
                    yield SweepSelect(
                        [self._item_get(item) for item in event.params.get("items", [])]
                    )
                elif event.method == "bind":
                    tag = event.params.get("tag", "")
                    handler = self._binds.get(tag)
                    if handler is None:
                        yield SweepBind(
                            event.params.get("tag", ""),
                            event.params.get("key", None),
                        )
                    else:
                        item = await handler(self, tag)
                        if item is not None:
                            yield SweepSelect([item])
                elif event.method == "resize":
                    size = SweepSize.from_json(event.params)
                    self._size = size
                    yield size
                elif event.method == "field_missing":
                    ref = event.params.get("ref")
                    if (
                        ref is None
                        or ref in self._field_resolved
                        or self._field_resolver is None
                    ):
                        continue
                    field = await self._field_resolver(ref)
                    if field is not None:
                        await self.field_register(field, ref)
                elif event.method == "view_missing":
                    ref = event.params.get("ref")
                    if (
                        ref is None
                        or ref in self._view_resolved
                        or self._view_resolver is None
                    ):
                        continue
                    view = await self._view_resolver(ref)
                    if view is not None:
                        await self.view_register(view, ref)

        return event_iter()

    async def terminate(self) -> None:
        """Terminate underlying sweep process"""
        proc, self._proc = self._proc, None
        io_sock, self._io_sock = self._io_sock, None
        self._peer.terminate()
        if io_sock is not None:
            io_sock.close()
        if proc is not None:
            await proc.wait()

    async def field_register_many(self, fields: dict[int, Field]) -> None:
        for field_ref, field in fields.items():
            await self.field_register(field, field_ref)

    async def field_register(self, field: Field, ref: int | None = None) -> int:
        """Register field that can later be reference by field with `ref` set"""
        ref_val = await self._peer.field_register(field.to_json(), ref)
        self._field_resolved.add(ref_val)
        return ref_val

    def field_resolver_set(
        self,
        field_resolver: FiledResolver | None,
    ) -> FiledResolver | None:
        """Set field resolver"""
        field_resolver, self._field_resolver = self._field_resolver, field_resolver
        return field_resolver

    async def view_register(self, view: View, ref: int | ViewRef | None = None) -> int:
        """Register view that can be later referenced by `ViewRef`"""
        ref_val = await self._peer.view_register(
            view.to_json(), ref.ref if isinstance(ref, ViewRef) else ref
        )
        self._view_resolved.add(ref_val)
        return ref_val

    async def size(self) -> SweepSize:
        """Get size of the Sweep ui"""
        while self._size is None:
            await self._peer.events
        return self._size

    async def items_extend(self, items: Iterable[I]) -> None:
        """Extend list of searchable items"""
        time_start = time.monotonic()
        time_limit = 0.05
        batch: list[I | dict[str, Any]] = []
        for item in items:
            if isinstance(item, ToCandidate):
                candidate = item.to_candidate()
                candidate.extra_update(_sweep_item_index=len(self._items))
                batch.append(candidate.to_json())
                self._items.append(item)
            else:
                batch.append(item)
                self._items.append(item)

            time_now = time.monotonic()
            if time_now - time_start >= time_limit:
                time_start = time_now
                time_limit *= 1.25
                await self._peer.items_extend(items=batch)
                batch.clear()
        if batch:
            await self._peer.items_extend(items=batch)

    async def item_update(self, index: int, item: I) -> None:
        """Update item by its index"""
        assert index >= 0, "index must be non-negative"
        if index >= len(self._items):
            raise IndexError(f"index {index} >= {len(self._items)}")
        self._items[index] = item
        if isinstance(item, ToCandidate):
            candidate = item.to_candidate()
            candidate.extra_update(_sweep_item_index=index)
            await self._peer.item_update(index=index, item=candidate.to_json())
        else:
            await self._peer.item_update(index=index, item=item)

    async def items_clear(self) -> None:
        """Clear list of searchable items"""
        self._items.clear()
        await self._peer.items_clear()

    async def items_current(self) -> I | None:
        """Get currently selected item if any"""
        return self._item_get(await self._peer.items_current())

    async def items_marked(self) -> list[I]:
        """Take currently marked items"""
        items = await self._peer.items_marked()
        return [self._item_get(item) for item in items]

    async def cursor_set(self, position: int) -> None:
        """Set cursor to specified position"""
        await self._peer.cursor_set(position=position)

    async def query_set(self, query: str) -> None:
        """Set query string used to filter items"""
        await self._peer.query_set(query=query)

    async def query_get(self) -> str:
        """Get query string used to filter items"""
        query: str = await self._peer.query_get()
        return query

    async def prompt_set(
        self,
        prompt: str | None = None,
        icon: Icon | None = None,
    ) -> None:
        """Set prompt label and icon"""
        attrs: dict[str, Any] = {}
        if prompt is not None:
            attrs["prompt"] = prompt
        if icon is not None:
            attrs["icon"] = icon.to_json()
        if attrs:
            await self._peer.prompt_set(**attrs)

    async def preview_set(self, value: bool | None) -> None:
        """Whether to show preview associated with the current item"""
        await self._peer.preview_set(value=value)

    async def footer_set(self, footer: View | None) -> None:
        """Set footer view"""
        if footer:
            await self._peer.footer_set(footer=footer.to_json())
        else:
            await self._peer.footer_set()

    async def bind_struct(self, bind: Bind[I]) -> None:
        await self.bind(bind.key, bind.tag, bind.desc, bind.handler)

    async def bind(
        self,
        key: str,
        tag: str,
        desc: str = "",
        handler: BindHandler[I] | None = None,
    ) -> None:
        """Assign new key binding

        Arguments:
            - `key` chord combination that triggers the bind
            - `tag` unique bind identifier if it is empty bind is removed
            - `description` of the bind shown in sweep help
            - `handler` callback if it no specified `SweepBind` event is generated
               otherwise, it called on key press
        """
        if tag and handler:
            self._binds[tag] = handler
        else:
            self._binds.pop(tag, None)
        await self._peer.bind(key=key, tag=tag, desc=desc)

    async def stack_push(self) -> None:
        """Push new empty state"""
        await self._peer.stack_push()

    async def stack_pop(self) -> None:
        """Pop previous state from the stack"""
        await self._peer.stack_pop()

    async def quick_select[H](
        self,
        items: Iterable[H],
        prompt: str | None = None,
        prompt_icon: Icon | None = None,
        keep_order: bool | None = None,
        theme: str | None = None,
        scorer: str | None = None,
    ) -> list[H]:
        """Create sub-sweep view to select from the list of items"""
        haystack: list[H | dict[str, Any]] = []
        haystack_index: dict[int, H] = {}
        for item in items:
            if isinstance(item, ToCandidate):
                index = len(haystack_index)
                haystack_index[index] = item
                candidate = item.to_candidate()
                candidate.extra_update(__sweep_item_index=index)
                haystack.append(candidate.to_json())
            else:
                haystack.append(item)
        selected = await self._peer.quick_select(
            items=haystack,
            prompt=prompt,
            prompt_icon=None if prompt_icon is None else prompt_icon.to_json(),
            keep_order=keep_order,
            theme=theme,
            scorer=scorer,
        )
        result: list[H] = []
        for item in selected:
            if isinstance(item, dict):
                item = cast(dict[str, Any], item)
                item_index = item.get("__sweep_item_index")
                if item_index is not None and (item := haystack_index.get(item_index)):
                    result.append(item)
            else:
                result.append(item)
        return result

    @asynccontextmanager
    async def render_suppress(self) -> AsyncIterator[None]:
        """Suppress rending to reduce flicker during batch updates"""
        try:
            await self._peer.render_suppress(True)
            yield None
        finally:
            if not self._peer.is_terminated:
                await self._peer.render_suppress(False)


def unix_server_once(path: str) -> Awaitable[socket.socket]:
    """Create unix server socket and accept one connection"""
    loop = asyncio.get_running_loop()
    if os.path.exists(path):
        os.unlink(path)
    server = socket.socket(socket.AF_UNIX)
    server.bind(path)
    server.listen()

    async def accept() -> socket.socket:
        try:
            accept = loop.create_future()
            loop.add_reader(server.fileno(), lambda: accept.set_result(None))
            await accept
            client, _ = server.accept()
            return client
        finally:
            loop.remove_reader(server.fileno())
            os.unlink(path)
            server.close()

    return create_task(accept(), "unix-server-once")


# ------------------------------------------------------------------------------
# JSON RPC
# ------------------------------------------------------------------------------
# Rpc request|response id
type RpcId = int | str | None


class RpcRequest(NamedTuple):
    method: str
    params: RpcParams
    id: RpcId

    def serialize(self) -> bytes:
        request: dict[str, Any] = {"method": self.method}
        if self.params is not None:
            request["params"] = self.params
        if self.id is not None:
            request["id"] = self.id
        return json.dumps(request).encode()

    @classmethod
    def deserialize(cls, obj: dict[str, Any]) -> RpcRequest | None:
        method = obj.get("method")
        if not isinstance(method, str):
            return None
        params = obj.get("params")
        if params is None or isinstance(params, (list, dict)):
            return cls(method, cast(RpcParams, params), obj.get("id"))
        return None


class RpcResult(NamedTuple):
    result: Any
    id: RpcId

    def serialize(self) -> bytes:
        response: dict[str, Any] = {"result": self.result}
        if self.id is not None:
            response["id"] = self.id
        return json.dumps(response).encode()

    @classmethod
    def deserialize(cls, obj: dict[str, Any]) -> RpcResult | None:
        if "result" not in obj:
            return None
        return RpcResult(obj.get("result"), obj.get("id"))


class RpcError(Exception):
    __slots__ = ["code", "message", "data", "id"]

    def __init__(self, code: int, message: str, data: str | None, id: RpcId) -> None:
        self.code: int = code
        self.message: str = message
        self.data: str | None = data
        self.id: RpcId = id

    def __str__(self) -> str:
        return f"{self.message}: {self.data}"

    def serialize(self) -> bytes:
        error = {
            "code": self.code,
            "message": self.message,
        }
        if self.data is not None:
            error["data"] = self.data
        response: dict[str, Any] = {"error": error}
        if self.id is not None:
            response["id"] = self.id
        return json.dumps(response).encode()

    @classmethod
    def deserialize(cls, obj: dict[str, Any]) -> RpcError | None:
        error: dict[str, Any] | None = obj.get("error")
        if error is None:
            return None
        code = error.get("code")
        if not isinstance(code, int):
            return None
        message = error.get("message")
        if not isinstance(message, str):
            return None
        return RpcError(code, message, error.get("data"), obj.get("id"))

    @classmethod
    def current(cls, *, data: str | None = None, id: RpcId = None) -> RpcError:
        """Create internal error from current exception"""
        etype, error, _ = sys.exc_info()
        if etype is None:
            return RpcError.internal_error(data=data, id=id)
        if isinstance(error, RpcError):
            return error
        if data is None:
            data = f"{error}"
        else:
            data = f"{data} {error}"
        return cls.internal_error(data=data, id=id)

    @classmethod
    def parse_error(cls, *, data: str | None = None, id: RpcId = None) -> RpcError:
        return RpcError(-32700, "Parse error", data, id)

    @classmethod
    def invalid_request(cls, *, data: str | None = None, id: RpcId = None) -> RpcError:
        return RpcError(-32600, "Invalid request", data, id)

    @classmethod
    def method_not_found(cls, *, data: str | None = None, id: RpcId = None) -> RpcError:
        return RpcError(-32601, "Method not found", data, id)

    @classmethod
    def invalid_params(cls, *, data: str | None = None, id: RpcId = None) -> RpcError:
        return RpcError(-32602, "Invalid params", data, id)

    @classmethod
    def internal_error(cls, *, data: str | None = None, id: RpcId = None) -> RpcError:
        return RpcError(-32603, "Internal error", data, id)


type RpcResponse = RpcError | RpcResult
type RpcMessage = RpcRequest | RpcResponse
type RpcParams = list[Any] | dict[str, Any] | None
type RpcHandler = Callable[..., Any]


class RpcPeer:
    __slots__ = [
        "_handlers",
        "_requests",
        "_requests_next_id",
        "_write_queue",
        "_write_notify",
        "_is_terminated",
        "_serve_task",
        "_events",
    ]

    def __init__(self) -> None:
        self._handlers: dict[str, RpcHandler] = {}  # registered handlers
        self._requests: dict[RpcId, Future[Any]] = {}  # unanswered requests
        self._requests_next_id: int = 0  # index used for next request
        self._write_queue: deque[RpcMessage] = deque()  # pending messages
        self._write_notify: Event[None] = Event()  # wake up writer
        self._is_terminated: bool = False  # whether peer has been terminated
        self._serve_task: Future[Any] | None = None  # read/write task
        self._events: Event[RpcRequest] = Event()  # received events (id = None)

    @property
    def events(self) -> Event[RpcRequest]:
        """Received events (requests with id = None)"""
        if self._is_terminated:
            raise StopAsyncIteration
        return self._events

    @property
    def is_terminated(self) -> bool:
        return self._is_terminated

    def register(self, method: str, handler: RpcHandler) -> RpcHandler:
        """Register handler for the provided method name"""
        if self._is_terminated:
            raise RuntimeError("peer has already been terminated")
        self._handlers[method] = handler
        return handler

    def notify(self, method: str, *args: Any, **kwargs: Any) -> None:
        """Send event to the other peer"""
        if self._is_terminated:
            raise RuntimeError("peer has already been terminated")

        params: RpcParams = None
        if args and kwargs:
            raise RpcError.invalid_params(data="cannot mix args and kwargs")
        elif args:
            params = list(args)
        elif kwargs:
            params = kwargs

        self._submit_message(RpcRequest(method, params, None))

    async def call(self, method: str, *args: Any, **kwargs: Any) -> Any:
        """Call remote method"""
        if self._is_terminated:
            raise RuntimeError("peer has already been terminated")

        future: Future[Any] = asyncio.get_running_loop().create_future()
        id = self._requests_next_id
        self._requests_next_id += 1

        params: RpcParams = None
        if args and kwargs:
            raise RpcError.invalid_params(data="cannot mix args and kwargs")
        elif args:
            params = list(args)
        elif kwargs:
            params = kwargs

        self._requests[id] = future
        self._submit_message(RpcRequest(method, params, id))
        return await future

    def __getattr__(self, method: str) -> Callable[..., Any]:
        """Convenient way to call remote methods"""
        return partial(self.call, method)

    def terminate(self) -> None:
        if self._is_terminated:
            return
        self._is_terminated = True
        # cancel requests and events
        requests = self._requests.copy()
        self._requests.clear()
        for request in requests.values():
            request.cancel()
        self._events.cancel()
        # cancel serve future
        if self._serve_task is not None:
            self._serve_task.cancel()

    async def serve(self, reader: StreamReader, writer: StreamWriter) -> None:
        """Start serving rpc peer over provided streams"""
        if self._is_terminated:
            raise RuntimeError("peer has already been terminated")
        if self._serve_task is not None:
            raise RuntimeError("serve can only be called once")

        try:
            self._serve_task = asyncio.gather(
                self._reader(reader),
                self._writer(writer),
            )
            await self._serve_task
        except (CancelledError, ConnectionResetError):
            pass
        finally:
            writer.close()
            self.terminate()

    def __aiter__(self) -> AsyncIterator[RpcRequest]:
        """Asynchronous iterator of events (requests with id = None)"""
        return RpcPeerIter(self)

    def _submit_message(self, message: RpcMessage) -> None:
        """Submit message for sending to the other peer"""
        self._write_queue.append(message)
        self._write_notify(None)

    def _handle_message(self, message: RpcMessage) -> None:
        """Handle incoming messages"""
        if isinstance(message, RpcRequest):
            # Events
            if message.id is None:
                self._events(message)

            # Requests
            handler = self._handlers.get(message.method)
            if handler is not None:
                create_task(
                    self._handle_request(message, handler),
                    f"rpc-handler-{message.method}",
                )
            elif message.id is not None:
                error = RpcError.method_not_found(
                    id=message.id, data=str(message.method)
                )
                self._submit_message(error)
        else:
            # Responses
            future = self._requests.pop(message.id, None)
            if isinstance(message, RpcError):
                if message.id is None:
                    raise message
                if future is not None and not future.done():
                    future.set_exception(message)
            elif future is not None and not future.done():
                future.set_result(message.result)

    async def _handle_request(self, request: RpcRequest, handler: RpcHandler) -> None:
        """Coroutine handling incoming request"""
        # convert params to either args or kwargs
        args: list[Any] = []
        kwargs: dict[str, Any] = {}
        if isinstance(request.params, list):
            args = request.params
        elif isinstance(request.params, dict):
            kwargs = request.params

        # execute handler
        id = request.id
        response: RpcResponse
        try:
            result = handler(*args, **kwargs)
            if inspect.isawaitable(result):
                result = await result
            response = RpcResult(result, id)
        except TypeError as error:
            response = RpcError.invalid_params(
                id=id,
                data=f"[{request.method}] {error}",
            )
        except Exception:
            response = RpcError.current(id=id, data=f"[{request.method}]")

        if request.id is not None:
            self._submit_message(response)

    async def _writer(self, writer: StreamWriter) -> None:
        """Write submitted messages to the output stream"""
        while not self._is_terminated:
            if not self._write_queue:
                # NOTE: we should never yield before waiting for notify
                #       and checking queue for emptiness. Otherwise we might block
                #       on non-empty write queue.
                await self._write_notify
                continue
            while self._write_queue:
                data = self._write_queue.popleft().serialize()
                writer.write(data)
                writer.write(b"\n")
            await writer.drain()
        raise CancelledError()

    async def _reader(self, reader: StreamReader) -> None:
        """Read and handle incoming messages"""
        while not self._is_terminated:
            # read json
            data = await reader.readline()
            if not data:
                break
            obj = json.loads(data)
            # deserialize
            message: RpcMessage | None = None
            message = (
                RpcRequest.deserialize(obj)
                or RpcError.deserialize(obj)
                or RpcResult.deserialize(obj)
            )
            if message is None:
                error = RpcError.invalid_request(
                    id=obj.get("id"),
                    data=data.decode(),
                )
                self._submit_message(error)
                continue
            # handle message
            self._handle_message(message)
        raise CancelledError()


class RpcPeerIter:
    __slots__ = ["peer", "events"]

    def __init__(self, peer: RpcPeer) -> None:
        self.peer: RpcPeer = peer
        self.events: deque[RpcRequest] = deque()
        self.peer.events.on(self._handler)

    def _handler(self, event: RpcRequest) -> bool:
        self.events.append(event)
        return True

    def __aiter__(self):
        return self

    async def __anext__(self) -> RpcRequest:
        if self.peer.is_terminated:
            raise StopAsyncIteration
        while not self.events:
            await self.peer.events
        return self.events.popleft()


def create_task[V](coro: Coroutine[Any, Any, V], name: str) -> Task[V]:
    task = asyncio.create_task(coro)
    task.set_name(name)
    return task


# ------------------------------------------------------------------------------
# Event
# ------------------------------------------------------------------------------


class Event[E]:
    __slots__ = ["_handlers", "_futures"]

    def __init__(self) -> None:
        self._handlers: set[Callable[[E], bool]] = set()
        self._futures: set[Future[E]] = set()

    def __call__(self, event: E) -> None:
        """Raise new event"""
        handlers = self._handlers.copy()
        self._handlers.clear()
        for handler in handlers:
            try:
                if handler(event):
                    self._handlers.add(handler)
            except Exception as error:
                warnings.warn(f"handler {handler} failed with error: {repr(error)}\n")
                pass
        futures = self._futures.copy()
        self._futures.clear()
        for future in futures:
            if future.done():
                continue
            future.set_result(event)

    def cancel(self) -> None:
        """Cancel all waiting futures"""
        futures = self._futures.copy()
        self._futures.clear()
        for future in futures:
            future.cancel()

    def on(self, handler: Callable[[E], bool]) -> None:
        """Register event handler

        Handler is kept subscribed as long as it returns True
        """
        self._handlers.add(handler)

    def __await__(self) -> Generator[E, None, E]:
        """Await for next event"""
        future: Future[E] = asyncio.get_running_loop().create_future()
        self._futures.add(future)
        return future.__await__()

    def __repr__(self) -> str:
        return f"Events(handlers={len(self._handlers)}, futures={len(self._futures)})"


# ------------------------------------------------------------------------------
# Views
# ------------------------------------------------------------------------------
class View(ABC):
    __slots__ = []

    @abstractmethod
    def to_json(self) -> dict[str, Any]: ...

    def trace_layout(self, msg: str) -> View:
        """Print debug message with constraints and calculated layout"""
        return TraceLayout(self, msg)

    def tag(self, tag: str) -> View:
        """Wrap view into Tag"""
        return Tag(tag, self)


class ViewRef(View):
    """Reference to a cached view"""

    __slots__ = ["ref"]

    def __init__(self, ref: int) -> None:
        self.ref = ref

    @override
    def to_json(self) -> dict[str, Any]:
        return {"type": "ref", "ref": self.ref}


class Direction(Enum):
    ROW = "horizontal"
    COL = "vertical"


class Justify(Enum):
    START = "start"
    CENTER = "center"
    END = "end"
    SPACE_BETWEEN = "space-between"
    SPACE_AROUND = "space-around"
    SPACE_EVENLY = "space-evenly"


class Align(Enum):
    START = "start"
    CENTER = "center"
    END = "end"
    EXPAND = "expand"
    SHRINK = "shrink"


type _4Float = tuple[float, float, float, float]


def _4float(
    a: float,
    b: float | None = None,
    c: float | None = None,
    d: float | None = None,
    /,
) -> _4Float:
    if b is None:
        return (a, a, a, a)
    elif c is None:
        return (a, b, a, b)
    elif d is None:
        return (a, b, c, b)
    return (a, b, c, d)


class IconFrame:
    __slots__ = [
        "_margin",
        "_border_width",
        "_border_radius",
        "_border_color",
        "_padding",
        "_fill_color",
    ]

    def __init__(
        self,
        margin: _4Float | None = None,
        border_width: _4Float | None = None,
        border_radius: _4Float | None = None,
        border_color: str | None = None,
        padding: _4Float | None = None,
        fill_color: str | None = None,
    ) -> None:
        self._margin = margin
        self._border_width = border_width
        self._border_radius = border_radius
        self._border_color = border_color
        self._padding = padding
        self._fill_color = fill_color

    def margin(
        self,
        top: float,
        right: float | None = None,
        bottom: float | None = None,
        left: float | None = None,
        /,
    ) -> IconFrame:
        self._margin = _4float(top, right, bottom, left)
        return self

    def border_width(
        self,
        top: float,
        right: float | None = None,
        bottom: float | None = None,
        left: float | None = None,
        /,
    ) -> IconFrame:
        self._border_width = _4float(top, right, bottom, left)
        return self

    def border_radius(
        self,
        a: float,
        b: float | None = None,
        c: float | None = None,
        d: float | None = None,
        /,
    ) -> IconFrame:
        self._border_radius = _4float(a, b, c, d)
        return self

    def border_color(self, color: str | None) -> IconFrame:
        self._border_color = color
        return self

    def padding(
        self,
        a: float,
        b: float | None = None,
        c: float | None = None,
        d: float | None = None,
        /,
    ) -> IconFrame:
        self._padding = _4float(a, b, c, d)
        return self

    def fill_color(self, color: str | None) -> IconFrame:
        self._fill_color = color
        return self

    def to_json(self) -> dict[str, Any]:
        obj = dict[str, Any]()
        if self._margin:
            obj["margin"] = self._margin
        if self._border_width:
            obj["border_width"] = self._border_width
        if self._border_radius:
            obj["border_radius"] = self._border_radius
        if self._border_color:
            obj["border_color"] = self._border_color
        if self._padding:
            obj["padding"] = self._padding
        if self._fill_color:
            obj["fill_color"] = self._fill_color
        return obj


class Icon(View):
    """SVG icon"""

    # only these characters are allowed to be in the svg path
    PATH_CHARS = set("+-e0123456789.,MmZzLlHhVvCcSsQqTtAa\r\t\n ")
    __slots__ = ["_path", "_view_box", "_fill_rule", "_size", "_fallback", "_frame"]

    def __init__(
        self,
        path: str,
        view_box: _4Float | None = None,
        fill_rule: str | None = None,
        size: tuple[int, int] | None = None,
        fallback: str | None = None,
        frame: IconFrame | None = None,
    ) -> None:
        self._path = path
        self._view_box = view_box
        self._fill_rule = fill_rule
        self._size = size
        self._fallback = fallback
        self._frame = frame

    def frame(self, frame: IconFrame) -> Icon:
        self._frame = frame
        return self

    @staticmethod
    def from_str_or_file(str_or_file: str) -> Icon | None:
        """Create sweep icon either by reading it from file or parsing from string"""
        if os.path.exists(str_or_file):
            with open(str_or_file) as file:
                str_or_file = file.read()
        try:
            return Icon.from_json(json.loads(str_or_file))
        except json.JSONDecodeError:
            return Icon.from_json(str_or_file)

    @staticmethod
    def from_json(obj: Any) -> Icon | None:
        """Create icon from JSON object"""

        def is_path(path: str) -> bool:
            if set(path) - Icon.PATH_CHARS:
                return False
            return True

        if isinstance(obj, dict):
            obj = cast(dict[str, Any], obj)
            path = obj.get("path")
            if isinstance(path, str) and is_path(path):
                return Icon(
                    path=path,
                    view_box=obj.get("view_box"),
                    fill_rule=obj.get("fill_rule"),
                    size=obj.get("size"),
                    fallback=obj.get("fallback"),
                )
        elif isinstance(obj, str) and is_path(obj):
            return Icon(obj)
        return None

    def to_json(self) -> dict[str, Any]:
        """Create JSON object out sweep icon struct"""
        obj: dict[str, Any] = dict(path=self._path, type="glyph")
        if self._view_box is not None:
            obj["view_box"] = self._view_box
        if self._fill_rule is not None:
            obj["fill_rule"] = self._fill_rule
        if self._size is not None:
            obj["size"] = self._size
        if self._fallback:
            obj["fallback"] = self._fallback
        if self._frame:
            obj["frame"] = self._frame.to_json()
        return obj


class TraceLayout(View):
    __slots__ = ["_view", "_msg"]

    def __init__(self, view: View, msg: str) -> None:
        self._view: View = view
        self._msg: str = msg

    def to_json(self) -> dict[str, Any]:
        return {
            "type": "trace-layout",
            "msg": self._msg,
            "view": self._view.to_json(),
        }


class FlexChild(NamedTuple):
    view: View
    flex: float | None
    face: str | None
    align: Align


class Flex(View):
    __slots__ = ["_children", "_justify", "_direction"]

    def __init__(self, direction: Direction) -> None:
        self._children: list[FlexChild] = []
        self._justify: Justify = Justify.START
        self._direction: Direction = direction

    @staticmethod
    def row() -> Flex:
        return Flex(Direction.ROW)

    @staticmethod
    def col() -> Flex:
        return Flex(Direction.COL)

    def justify(self, justify: Justify) -> Flex:
        self._justify = justify
        return self

    def push(
        self,
        child: View,
        flex: float | None = None,
        face: str | None = None,
        align: Align = Align.START,
    ) -> Flex:
        self._children.append(FlexChild(child, flex, face, align))
        return self

    def to_json(self) -> dict[str, Any]:
        children_json: list[dict[str, Any]] = []
        for child in self._children:
            child_json: dict[str, Any] = {}
            if child.flex is not None:
                child_json["flex"] = child.flex
            if child.align != Align.START:
                child_json["align"] = child.align.value
            if child.face is not None:
                child_json["face"] = child.face
            child_json["view"] = child.view.to_json()
            children_json.append(child_json)
        return {
            "type": "flex",
            "direction": self._direction.value,
            "justify": self._justify.value,
            "children": children_json,
        }


class Container(View):
    __slots__ = ["_child", "_face", "_vertical", "_horizontal", "_size", "_margins"]

    def __init__(self, child: View) -> None:
        self._child: View = child
        self._face: str | None = None
        self._vertical: Align = Align.START
        self._horizontal: Align = Align.START
        self._size: tuple[int, int] = (0, 0)
        self._margins: tuple[int, int, int, int] = (0, 0, 0, 0)

    def face(self, face: str) -> Container:
        self._face = face
        return self

    def horizontal(self, align: Align) -> Container:
        self._horizontal = align
        return self

    def vertical(self, align: Align) -> Container:
        self._vertical = align
        return self

    def margins(
        self,
        left: int | None = None,
        right: int | None = None,
        top: int | None = None,
        bottom: int | None = None,
    ) -> Container:
        left = left if left is not None else self._margins[0]
        right = right if right is not None else self._margins[1]
        top = top if top is not None else self._margins[2]
        bottom = bottom if bottom is not None else self._margins[3]
        self._margins = (left, right, top, bottom)
        return self

    def size(
        self,
        height: int | None = None,
        width: int | None = None,
    ) -> Container:
        height = height if height is not None else self._size[0]
        width = width if width is not None else self._size[1]
        self._size = (height, width)
        return self

    def to_json(self) -> dict[str, Any]:
        obj: dict[str, Any] = dict(type="container", child=self._child.to_json())
        if self._face is not None:
            obj["face"] = self._face
        if self._vertical != Align.START:
            obj["vertical"] = self._vertical.value
        if self._horizontal != Align.START:
            obj["horizontal"] = self._horizontal.value
        if self._size != (0, 0):
            obj["size"] = self._size
        if self._margins != (0, 0, 0, 0):
            obj["margins"] = self._margins
        return obj


class Tag(View):
    __slots__ = ["_tag", "_view"]

    def __init__(self, tag: str, view: View) -> None:
        self._tag: str = tag
        self._view: View = view

    def to_json(self) -> dict[str, Any]:
        return {
            "type": "tag",
            "tag": self._tag,
            "view": self._view.to_json(),
        }


class Text(View):
    __slots__ = ["_chunks", "_face", "_glyph"]

    def __init__(
        self,
        text: str = "",
        glyph: Icon | None = None,
        face: str | None = None,
    ):
        self._chunks: list[Text] | str = text
        self._face: str | None = face
        self._glyph: Icon | None = glyph

    def push(
        self,
        text: str = "",
        glyph: Icon | None = None,
        face: str | None = None,
    ) -> Text:
        chunk = Text(text, glyph, face)
        if isinstance(self._chunks, list):
            self._chunks.append(chunk)
        else:
            self._chunks = [Text(self._chunks, self._glyph), chunk]
            self._glyph = None
        return self

    def to_json(self) -> dict[str, Any]:
        def to_json_rec(text: Text) -> Any:
            chunks = (
                [to_json_rec(chunk) for chunk in text._chunks]
                if isinstance(text._chunks, list)
                else text._chunks
            )
            if text._glyph is None and text._face is None:
                return chunks
            obj: dict[str, Any] = dict(text=chunks)
            if text._glyph is not None:
                obj["glyph"] = text._glyph.to_json()
            if text._face is not None:
                obj["face"] = text._face
            return obj

        return {
            "type": "text",
            "text": to_json_rec(self),
        }


class Image(View):
    __slots__ = ["_size", "_channels", "_data"]

    def __init__(self, buff: Any):
        arr = getattr(buff, "__array_interface__")
        if isinstance(arr.get("data"), bytes):
            mem = memoryview(arr["data"]).cast("B", arr["shape"])
        else:
            mem = memoryview(buff)
        if mem.shape is None:
            raise ValueError("None image shape value")
        if mem.ndim == 3:
            channels = mem.shape[-1]
        elif mem.ndim == 2:
            channels = 1
        else:
            raise ValueError("Invalid image shape: {}", mem.shape)
        if channels not in {4, 3, 1}:
            raise ValueError("Invalid channel size: {}", channels)
        self._size: tuple[int, int] = (mem.shape[0], mem.shape[1])
        self._channels: int = channels
        self._data: str = base64.b64encode(mem).decode()

    def to_json(self) -> dict[str, Any]:
        return {
            "type": "image",
            "size": self._size,
            "channels": self._channels,
            "data": self._data,
        }

    def __repr__(self) -> str:
        return f"Image(size={self._size}, channels={self._channels}, data_size={len(self._data)})"


# ------------------------------------------------------------------------------
# Main
# ------------------------------------------------------------------------------
async def main(args: list[str] | None = None) -> None:
    import argparse
    import shlex

    parser = argparse.ArgumentParser(description="Sweep is a command line fuzzy finder")
    parser.add_argument(
        "-p",
        "--prompt",
        default="INPUT",
        help="override prompt string",
    )
    parser.add_argument(
        "--prompt-icon",
        default=None,
        help="set prompt icon",
    )
    parser.add_argument(
        "--query",
        help="start sweep with the given query",
    )
    parser.add_argument(
        "--nth",
        help="comma-seprated list of fields for limiting search",
    )
    parser.add_argument("--delimiter", help="filed delimiter")
    parser.add_argument("--theme", help="theme as a list of comma separated attributes")
    parser.add_argument("--scorer", help="default scorer")
    parser.add_argument("--tty", help="tty device path")
    parser.add_argument("--height", type=int, help="height in lines")
    parser.add_argument("--sweep", default="sweep", help="sweep binary")
    parser.add_argument(
        "--json",
        action="store_true",
        help="expect candidates in JSON format",
    )
    parser.add_argument(
        "--no-match",
        choices=["nothing", "input"],
        help="what is returned if there is no match on enter",
    )
    parser.add_argument(
        "--keep-order",
        action="store_true",
        help="keep order of elements (do not use ranking score)",
    )
    parser.add_argument(
        "--input",
        type=argparse.FileType("r"),
        default=sys.stdin,
        help="file from which input is read",
    )
    parser.add_argument(
        "--tmp-socket",
        action="store_true",
        help="create temporary socket in tmp for rpc",
    )
    parser.add_argument("--log", help="path to the log file")
    parser.add_argument(
        "--border",
        type=int,
        help="borders on the side of the sweep view",
    )
    opts = parser.parse_args(args)

    candidates: list[Any]
    if opts.json:
        candidates = json.load(opts.input)
    else:
        candidates = []
        for line in opts.input:
            candidates.append(line.strip())

    result = await sweep(
        candidates,
        sweep=shlex.split(opts.sweep),
        prompt=opts.prompt,
        prompt_icon=opts.prompt_icon,
        query=opts.query,
        nth=opts.nth,
        delimiter=opts.delimiter,
        theme=opts.theme,
        scorer=opts.scorer,
        tty=opts.tty,
        keep_order=opts.keep_order,
        no_match=opts.no_match,
        tmp_socket=opts.tmp_socket,
        log=opts.log,
    )

    if not result:
        pass
    elif opts.json:
        json.dump(result, sys.stdout)
    else:
        print(result)


if __name__ == "__main__":
    asyncio.run(main())
