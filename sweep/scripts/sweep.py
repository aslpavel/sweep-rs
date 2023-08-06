#!/usr/bin/env python3
"""Asynchronous JSON-RPC implementation to communicate with sweep command
"""
# pyright: strict
from __future__ import annotations

import asyncio
import inspect
import json
import os
import socket
import sys
import tempfile
import time
from asyncio import CancelledError, Future, StreamReader, StreamWriter
from asyncio.subprocess import Process
from asyncio.tasks import Task
from collections import deque
from functools import partial
from dataclasses import dataclass
from typing import (
    Any,
    AsyncGenerator,
    AsyncIterator,
    Awaitable,
    Callable,
    Coroutine,
    Deque,
    Dict,
    Generator,
    Generic,
    Iterable,
    List,
    NamedTuple,
    Optional,
    Set,
    Tuple,
    TypeVar,
    Union,
    cast,
)

__all__ = [
    "Sweep",
    "SweepSelect",
    "SweepBind",
    "SweepEvent",
    "SweepIcon",
    "sweep",
    "Candidate",
    "Field",
]

# ------------------------------------------------------------------------------
# Sweep
# ------------------------------------------------------------------------------
I = TypeVar("I")  # sweep item


class SweepBind(NamedTuple):
    """Event generated on bound key press"""

    tag: str

    def __repr__(self):
        return f'SweepBind("{self.tag}")'


@dataclass
class SweepSelect(Generic[I]):
    """Event generated on item select"""

    item: Optional[I]

    def __init__(self, item: Optional[I]):
        self.item = item


class SweepIcon(NamedTuple):
    """SVG icon"""

    # only these characters are allowed to be in the svg path
    PATH_CHARS = set("+-e0123456789.,MmZzLlHhVvCcSsQqTtAa\r\t\n ")

    path: str
    view_box: Optional[Tuple[float, float, float, float]] = None
    fill_rule: Optional[str] = None
    size: Optional[Tuple[int, int]] = None
    fallback: Optional[str] = None

    @staticmethod
    def from_str_or_file(str_or_file: str) -> Optional[SweepIcon]:
        """Create sweep icon either by reading it from file or parsing from string"""
        if os.path.exists(str_or_file):
            with open(str_or_file, "r") as file:
                str_or_file = file.read()
        try:
            return SweepIcon.from_json(json.loads(str_or_file))
        except json.JSONDecodeError:
            return SweepIcon.from_json(str_or_file)

    @staticmethod
    def from_json(obj: Any) -> Optional[SweepIcon]:
        """Create icon from JSON object"""

        def is_path(path: str) -> bool:
            if set(path) - SweepIcon.PATH_CHARS:
                return False
            return True

        if isinstance(obj, dict):
            obj = cast(Dict[str, Any], obj)
            path = obj.get("path")
            if isinstance(path, str) and is_path(path):
                return SweepIcon(
                    path=path,
                    view_box=obj.get("view_box"),
                    fill_rule=obj.get("fill_rule"),
                    size=obj.get("size"),
                    fallback=obj.get("fallback"),
                )
        elif isinstance(obj, str) and is_path(obj):
            return SweepIcon(obj)
        return None

    def to_json(self) -> Dict[str, Any]:
        """Create JSON object out sweep icon struct"""
        obj: Dict[str, Any] = dict(path=self.path)
        if self.view_box is not None:
            obj["view_box"] = self.view_box
        if self.fill_rule is not None:
            obj["fill_rule"] = self.fill_rule
        if self.size is not None:
            obj["size"] = self.size
        if self.fallback:
            obj["fallback"] = self.fallback
        return obj


@dataclass
class Field:
    """Filed structure used to construct `Candidate`"""

    text: str = ""
    active: bool = True
    glyph: Optional[SweepIcon] = None
    face: Optional[str] = None
    ref: Optional[int] = None

    def __repr__(self) -> str:
        attrs: List[str] = []
        if self.text:
            attrs.append(f"text={repr(self.text)}")
        if not self.active:
            attrs.append(f"active={self.active}")
        if self.glyph is not None:
            attrs.append(f"glyph={self.glyph}")
        if self.face is not None:
            attrs.append(f"face={self.face}")
        if self.ref is not None:
            attrs.append(f"ref={self.ref}")
        return f'Field({", ".join(attrs)})'

    def to_json(self) -> Dict[str, Any]:
        """Convert field to JSON"""
        obj: Dict[str, Any] = dict(text=self.text)
        if not self.active:
            obj["active"] = False
        if self.glyph:
            obj["glyph"] = self.glyph.to_json()
        if self.face:
            obj["face"] = self.face
        if self.ref is not None:
            obj["ref"] = self.ref
        return obj

    @staticmethod
    def from_json(obj: Any) -> Optional[Field]:
        """Create field from JSON object"""
        if not isinstance(obj, dict):
            return
        obj = cast(Dict[str, Any], obj)
        active = obj.get("active")
        return Field(
            text=obj.get("text") or "",
            active=True if active is None else active,
            glyph=SweepIcon.from_json(obj.get("glyph")),
            face=obj.get("face"),
            ref=obj.get("ref"),
        )


@dataclass
class Candidate:
    """Convenient sweep item implementation"""

    target: Optional[List[Field]] = None
    extra: Optional[Dict[str, Any]] = None
    right: Optional[List[Field]] = None
    right_offset: int = 0
    preview: Optional[List[Any]] = None
    preview_flex: float = 0.0

    def target_push(
        self,
        text: str = "",
        active: bool = True,
        glyph: Optional[SweepIcon] = None,
        face: Optional[str] = None,
        ref: Optional[int] = None,
    ) -> Candidate:
        """Add field to the target (matchable left side text)"""
        if self.target is None:
            self.target = []
        self.target.append(Field(text, active, glyph, face, ref))
        return self

    def right_push(
        self,
        text: str = "",
        active: bool = True,
        glyph: Optional[SweepIcon] = None,
        face: Optional[str] = None,
        ref: Optional[int] = None,
    ) -> Candidate:
        """Add field to the right (unmatchable right side text)"""
        if self.right is None:
            self.right = []
        self.right.append(Field(text, active, glyph, face, ref))
        return self

    def right_offset_set(self, offset: int) -> Candidate:
        """Set offset for the right side text"""
        self.right_offset = offset
        return self

    def preview_push(
        self,
        text: str = "",
        active: bool = True,
        glyph: Optional[SweepIcon] = None,
        face: Optional[str] = None,
        ref: Optional[int] = None,
    ) -> Candidate:
        """Add field to the preview (text shown when item is highlighted)"""
        if self.preview is None:
            self.preview = []
        self.preview.append(Field(text, active, glyph, face, ref))
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

    def __repr__(self) -> str:
        attrs: List[str] = []
        if self.target is not None:
            attrs.append(f"target={self.target}")
        if self.extra is not None:
            attrs.append(f"extra={self.extra}")
        if self.right is not None:
            attrs.append(f"right={self.right}")
        if self.right_offset != 0:
            attrs.append(f"right_offset={self.right_offset}")
        if self.preview is not None:
            attrs.append(f"preview={self.preview}")
        if self.preview_flex != 0.0:
            attrs.append(f"preview_flex={self.preview_flex}")
        return f'Candidate({", ".join(attrs)})'

    def to_json(self) -> Dict[str, Any]:
        """Convert candidate to JSON object"""
        obj: Dict[str, Any] = self.extra.copy() if self.extra else {}
        if self.target:
            obj["target"] = [field.to_json() for field in self.target]
        if self.right:
            obj["right"] = [field.to_json() for field in self.right]
        if self.right_offset:
            obj["offset"] = self.right_offset
        if self.preview:
            obj["preview"] = [field.to_json() for field in self.preview]
        if self.preview_flex != 0.0:
            obj["preview_flex"] = self.preview_flex
        return obj

    @staticmethod
    def from_json(obj: Any) -> Optional[Candidate]:
        """Construct candidate from JSON object"""
        if isinstance(obj, str):
            return Candidate().target_push(obj)
        if not isinstance(obj, dict):
            return

        def fields_from_json(fields_obj: Any) -> Optional[List[Field]]:
            if not isinstance(fields_obj, list):
                return None
            fields: List[Field] = []
            for field_obj in cast(List[Any], fields_obj):
                field = Field.from_json(field_obj)
                if field is None:
                    continue
                fields.append(field)
            return fields or None

        obj = cast(Dict[str, Any], obj)
        target = fields_from_json(obj.pop("target", None))
        right = fields_from_json(obj.pop("right", None))
        right_offset = obj.pop("offset", None) or 0
        preview = fields_from_json(obj.pop("preview", None))
        preview_flex = obj.pop("preview_flex", None) or 0.0
        return Candidate(
            target=target,
            extra=obj or None,
            right=right,
            right_offset=right_offset,
            preview=preview,
            preview_flex=preview_flex,
        )


SweepEvent = Union[SweepBind, SweepSelect[I]]


async def sweep(
    items: Iterable[I],
    prompt_icon: Optional[SweepIcon | str] = None,
    **options: Any,
) -> Optional[I]:
    """Convenience wrapper around `Sweep`

    Useful when you only need to select one candidate from a list of items
    """
    async with Sweep[I](**options) as sweep:
        if not isinstance(prompt_icon, (SweepIcon, type(None))):
            icon = SweepIcon.from_str_or_file(prompt_icon)
            if icon is None:
                raise ValueError(f"invalid prompt icon: {prompt_icon}")
            prompt_icon = icon
        await sweep.prompt_set(prompt=options.get("prompt"), icon=prompt_icon)
        await sweep.items_extend(items)
        async for event in sweep:
            if isinstance(event, SweepSelect):
                return event.item
    return None


class Sweep(Generic[I]):
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
        "_tmp_socket",
        "_seen_candidate",
    ]

    _args: List[str]
    _proc: Optional[Process]
    _io_sock: Optional[socket.socket]
    _peer: RpcPeer
    _tmp_socket: bool  # create tmp socket instead of communicating via socket-pair
    _seen_candidate: bool

    def __init__(
        self,
        sweep: List[str] = ["sweep"],
        prompt: str = "INPUT",
        query: Optional[str] = None,
        nth: Optional[str] = None,
        height: int = 11,
        delimiter: Optional[str] = None,
        theme: Optional[str] = None,
        scorer: Optional[str] = None,
        tty: Optional[str] = None,
        log: Optional[str] = None,
        title: Optional[str] = None,
        keep_order: bool = False,
        no_match: Optional[str] = None,
        altscreen: bool = False,
        tmp_socket: bool = False,
        border: Optional[int] = None,
    ) -> None:
        args: List[str] = []
        args.extend(["--prompt", prompt])
        args.extend(["--height", str(height)])
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
        if altscreen:
            args.append("--altscreen")
        if border is not None:
            args.extend(["--border", str(border)])

        self._args = [*sweep, "--rpc", *args]
        self._proc = None
        self._io_sock = None
        self._tmp_socket = tmp_socket
        self._peer = RpcPeer()
        self._seen_candidate = False

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

    async def __aexit__(self, _et: Any, ev: Any, _tb: Any) -> bool:
        await self.terminate()
        if isinstance(ev, CancelledError):
            return True
        return False

    def __aiter__(self) -> AsyncIterator[SweepEvent[I]]:
        async def event_iter() -> AsyncGenerator[SweepEvent[I], None]:
            async for event in self._peer:
                if not isinstance(event.params, dict):
                    continue
                if event.method == "select":
                    if self._seen_candidate:
                        item = event.params.get("item")
                        yield SweepSelect(cast(I, Candidate.from_json(item)) or item)
                    else:
                        yield SweepSelect(event.params.get("item"))
                elif event.method == "bind":
                    yield SweepBind(event.params.get("tag", ""))

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

    async def field_register(self, field: Any) -> int:
        return await self._peer.field_register(
            field.to_json() if isinstance(field, Field) else field
        )

    async def items_extend(self, items: Iterable[I]) -> None:
        """Extend list of searchable items"""
        time_start = time.monotonic()
        time_limit = 0.05
        batch: List[I | Dict[str, Any]] = []
        for item in items:
            if isinstance(item, Candidate):
                batch.append(item.to_json())
                self._seen_candidate = True
            else:
                batch.append(item)

            time_now = time.monotonic()
            if time_now - time_start >= time_limit:
                time_start = time_now
                time_limit *= 1.25
                await self._peer.items_extend(items=batch)
                batch.clear()
        if batch:
            await self._peer.items_extend(items=batch)

    async def items_clear(self) -> None:
        """Clear list of searchable items"""
        await self._peer.items_clear()

    async def items_current(self) -> Optional[I]:
        """Get currently selected item if any"""
        item: Optional[I] = await self._peer.items_current()
        if self._seen_candidate:
            return cast(I, Candidate.from_json(item))
        else:
            return item

    async def query_set(self, query: str) -> None:
        """Set query string used to filter items"""
        await self._peer.query_set(query=query)

    async def query_get(self) -> str:
        """Get query string used to filter items"""
        query: str = await self._peer.query_get()
        return query

    async def prompt_set(
        self,
        prompt: Optional[str] = None,
        icon: Optional[SweepIcon] = None,
    ) -> None:
        """Set prompt label and icon"""
        attrs: Dict[str, Any] = {}
        if prompt is not None:
            attrs["prompt"] = prompt
        if icon is not None:
            attrs["icon"] = icon.to_json()
        if attrs:
            await self._peer.prompt_set(**attrs)

    async def preview_set(self, value: Optional[bool]) -> None:
        """Whether to show preview associated with the current item"""
        await self._peer.preview_set(value=value)

    async def bind(self, key: str, tag: str, desc: str = "") -> None:
        """Assign new key binding"""
        await self._peer.bind(key=key, tag=tag, desc=desc)


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
RpcId = Union[int, str, None]


class RpcRequest(NamedTuple):
    method: str
    params: RpcParams
    id: RpcId

    def serialize(self) -> bytes:
        request: Dict[str, Any] = {
            "jsonrpc": "2.0",
            "method": self.method,
        }
        if self.params is not None:
            request["params"] = self.params
        if self.id is not None:
            request["id"] = self.id
        return json.dumps(request).encode()

    @classmethod
    def deserialize(cls, obj: Dict[str, Any]) -> Optional[RpcRequest]:
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
        response: Dict[str, Any] = {
            "jsonrpc": "2.0",
            "result": self.result,
        }
        if self.id is not None:
            response["id"] = self.id
        return json.dumps(response).encode()

    @classmethod
    def deserialize(cls, obj: Dict[str, Any]) -> Optional[RpcResult]:
        if "result" not in obj:
            return None
        return RpcResult(obj.get("result"), obj.get("id"))


class RpcError(Exception):
    __slots__ = ["code", "message", "data", "id"]

    code: int
    message: str
    data: Optional[str]
    id: RpcId

    def __init__(self, code: int, message: str, data: Optional[str], id: RpcId) -> None:
        self.code = code
        self.message = message
        self.data = data
        self.id = id

    def __str__(self) -> str:
        return f"{self.message}: {self.data}"

    def serialize(self) -> bytes:
        error = {
            "code": self.code,
            "message": self.message,
        }
        if self.data is not None:
            error["data"] = self.data
        response: Dict[str, Any] = {
            "jsonrpc": "2.0",
            "error": error,
        }
        if self.id is not None:
            response["id"] = self.id
        return json.dumps(response).encode()

    @classmethod
    def deserialize(cls, obj: Dict[str, Any]) -> Optional[RpcError]:
        error: Optional[Dict[str, Any]] = obj.get("error")
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
    def current(cls, *, data: Optional[str] = None, id: RpcId = None) -> RpcError:
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
    def parse_error(cls, *, data: Optional[str] = None, id: RpcId = None) -> RpcError:
        return RpcError(-32700, "Parse error", data, id)

    @classmethod
    def invalid_request(
        cls, *, data: Optional[str] = None, id: RpcId = None
    ) -> RpcError:
        return RpcError(-32600, "Invalid request", data, id)

    @classmethod
    def method_not_found(
        cls, *, data: Optional[str] = None, id: RpcId = None
    ) -> RpcError:
        return RpcError(-32601, "Method not found", data, id)

    @classmethod
    def invalid_params(
        cls, *, data: Optional[str] = None, id: RpcId = None
    ) -> RpcError:
        return RpcError(-32602, "Invalid params", data, id)

    @classmethod
    def internal_error(
        cls, *, data: Optional[str] = None, id: RpcId = None
    ) -> RpcError:
        return RpcError(-32603, "Internal error", data, id)


RpcResponse = Union[RpcError, RpcResult]
RpcMessage = Union[RpcRequest, RpcResponse]
RpcParams = Union[List[Any], Dict[str, Any], None]
RpcHandler = Callable[..., Any]


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

    _handlers: Dict[str, RpcHandler]  # registered handlers
    _requests: Dict[RpcId, Future[Any]]  # unanswered requests
    _requests_next_id: int  # index used for next request
    _write_queue: Deque[RpcMessage]  # messages to be send to the other peer
    _write_notify: Event[None]  # event used to wake up writer
    _is_terminated: bool  # whether peer was terminated
    _serve_task: Optional[Future[Any]]  # running serve task
    _events: Event[RpcRequest]  # received events (requests with id = None)

    def __init__(self) -> None:
        self._handlers = {}
        self._requests = {}
        self._requests_next_id = 0
        self._write_queue = deque()
        self._write_notify = Event()
        self._is_terminated = False
        self._serve_task = None
        self._events = Event()

    @property
    def events(self) -> Event[RpcRequest]:
        """Received events (requests with id = None)"""
        if self._is_terminated:
            raise RuntimeError("peer has already been terminated")
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
            if message.id is None:
                self._events(message)
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
        args: List[Any] = []
        kwargs: Dict[str, Any] = {}
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
                writer.write(f"{len(data)}\n".encode())
                writer.write(data)
            await writer.drain()
        raise CancelledError()

    async def _reader(self, reader: StreamReader) -> None:
        """Read and handle incoming messages"""
        while not self._is_terminated:
            # read json
            size_data = await reader.readline()
            if not size_data:
                break
            size = int(size_data.strip())
            data = await reader.readexactly(size)
            if not data:
                break
            obj = json.loads(data)
            # deserialize
            message: Optional[RpcMessage] = None
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

    peer: RpcPeer
    events: Deque[RpcRequest]

    def __init__(self, peer: RpcPeer) -> None:
        self.peer = peer
        self.events = deque()
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


V = TypeVar("V")


def create_task(coro: Coroutine[Any, Any, V], name: str) -> Task[V]:
    task = asyncio.create_task(coro)
    if sys.version_info >= (3, 8):
        task.set_name(name)
    return task


# ------------------------------------------------------------------------------
# Event
# ------------------------------------------------------------------------------
E = TypeVar("E")


class Event(Generic[E]):
    __slots__ = ["_handlers", "_futures"]

    def __init__(self) -> None:
        self._handlers: Set[Callable[[E], bool]] = set()
        self._futures: Set[Future[E]] = set()

    def __call__(self, event: E) -> None:
        """Raise new event"""
        handlers = self._handlers.copy()
        self._handlers.clear()
        for handler in handlers:
            try:
                if handler(event):
                    self._handlers.add(handler)
            except Exception as error:
                sys.stderr.write(
                    f"handler {handler} failed with error: {repr(error)}\n"
                )
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
# Main
# ------------------------------------------------------------------------------
async def main() -> None:
    import shlex
    import argparse

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
        "--altscreen",
        action="store_true",
        help="use alterniative screen",
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
    args = parser.parse_args()

    candidates: List[Any]
    if args.json:
        candidates = json.load(args.input)
    else:
        candidates = []
        for line in args.input:
            candidates.append(line.strip())

    result = await sweep(
        candidates,
        sweep=shlex.split(args.sweep),
        prompt=args.prompt,
        prompt_icon=args.prompt_icon,
        query=args.query,
        nth=args.nth,
        height=args.height or 11,
        delimiter=args.delimiter,
        theme=args.theme,
        scorer=args.scorer,
        tty=args.tty,
        keep_order=args.keep_order,
        no_match=args.no_match,
        altscreen=args.altscreen,
        tmp_socket=args.tmp_socket,
        log=args.log,
        border=args.border,
    )

    if result is None:
        pass
    elif args.json:
        json.dump(result, sys.stdout)
    else:
        print(result)


if __name__ == "__main__":
    asyncio.run(main())
