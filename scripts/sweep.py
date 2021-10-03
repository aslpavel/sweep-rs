#!/usr/bin/env python3
"""Asynchronous JSON-RPC implementation to communicate with sweep command
"""
# pyright: strict
import asyncio
from functools import partial
import json
import os
import socket
import sys
import tempfile
import time
import traceback
import unittest
import inspect
from asyncio import CancelledError, Future, StreamReader, StreamWriter
from asyncio.subprocess import Process
from collections import deque
from typing import (
    Any,
    AsyncIterator,
    Awaitable,
    Callable,
    Deque,
    Dict,
    Generator,
    Generic,
    Iterable,
    List,
    NamedTuple,
    Optional,
    Set,
    TypeVar,
    Union,
    cast,
)

__all__ = ["Sweep", "SweepError", "sweep", "SWEEP_SELECTED", "SWEEP_KEYBINDING"]

SWEEP_SELECTED = "select"
SWEEP_KEYBINDING = "bind"

Candidate = Union[str, Dict[str, Any]]


async def sweep(chandidates: List[Candidate], **options: Any) -> Any:
    """Convinience wrapper around `Sweep`

    Useful when you only need to select one candidate from a list of items
    """
    async with Sweep(**options) as sweep:
        await sweep.candidates_extend(chandidates)
        async for msg in sweep:
            if msg.method == SWEEP_SELECTED:
                return msg.params


class Sweep:
    """RPC wrapper around sweep process

    DEBUGGING:
        - Load this file as python module.
        - Open other terminal window and execute `$ tty` command, then run something that
          will not steal characters for sweep process like `$ sleep 1000`.
        - Instantiate Sweep class with the tty device path of the other terminal.
        - Now you can call all the methods of the Sweep class in an interractive mode.
    """

    __slots__ = [
        "_args",
        "_proc",
        "_io_sock",
        "_last_id",
        "_read_events",
        "_read_requests",
        "_write_queue",
        "_write_notify",
    ]

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
        debug: bool = False,
        title: Optional[str] = None,
        keep_order: bool = False,
        no_match: Optional[str] = None,
        altscreen: bool = False,
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
        if debug:
            args.append("--debug")
        if title:
            args.extend(["--title", title])
        if keep_order:
            args.append("--keep-order")
        if no_match:
            args.extend(["--no-match", no_match])
        if altscreen:
            args.append("--altscreen")

        self._args = [*sweep, "--rpc", *args]
        self._proc: Optional[Process] = None
        self._io_sock: Optional[socket.socket] = None
        self._last_id = 0
        self._read_events: Event[SweepRequest] = Event()
        self._read_requests: Dict[int, "Future[Any]"] = {}
        self._write_queue: Deque[SweepRequest] = deque()
        self._write_notify: Event[None] = Event()

    async def _worker_main(self, sock: socket.socket) -> None:
        """Main worker coroutine which reads and write data to/from sweep"""
        if self._proc is None:
            return
        try:
            reader, writer = await asyncio.open_unix_connection(sock=sock)
            await asyncio.gather(
                self._worker_writer(writer),
                self._worker_reader(reader),
            )
        except (asyncio.CancelledError, ConnectionResetError):
            pass
        except Exception:
            sys.stderr.write("sweep worker failed with error:\n")
            traceback.print_exc(file=sys.stderr)
        finally:
            await self.terminate()

    async def _worker_writer(self, writer: StreamWriter) -> None:
        """Write outging messages"""
        while self._proc is not None:
            if not self._write_queue:
                await self._write_notify
                continue
            writer.write(self._write_queue.popleft().encode())
            await writer.drain()
        raise asyncio.CancelledError()

    async def _worker_reader(self, reader: StreamReader) -> None:
        """Read and dispatch incomming messages from the reader"""
        while self._proc is not None:
            data_size = await reader.readline()
            if not data_size:
                break
            size = int(data_size.strip())
            data = await reader.readexactly(size)
            if not data:
                break
            self._read_dispatch(json.loads(data))
        raise asyncio.CancelledError()

    def _read_dispatch(self, msg: Any) -> None:
        """Handle incomming messages"""
        # handle events
        method = msg.get("method")
        if method:
            event = SweepRequest(method, msg.get("params"), None)
            self._read_events(event)
            return

        future = self._read_requests.pop(msg.get("id"), None)
        if future is None:
            return
        error = msg.get("error")
        if error is None:
            # handle results
            result = msg.get("result")
            future.set_result(result)
        else:
            # handle errors
            if isinstance(error, dict):
                error = cast(Dict[str, Any], error)
                error = SweepError(
                    error.get("code", -32603),
                    error.get("message", ""),
                    error.get("data", ""),
                )
            else:
                error = SweepError(
                    -32700, "Parse error", f"Error must be an object: {msg}"
                )
            future.set_exception(error)
            return

    def _call(self, method: str, params: Optional[Any] = None) -> "Future[Any]":
        future: Future[Any] = asyncio.get_running_loop().create_future()
        if self._proc is None:
            future.set_exception(RuntimeError("sweep process is not running"))
        else:
            self._last_id += 1
            self._write_queue.append(SweepRequest(method, params, self._last_id))
            self._write_notify(None)
            self._read_requests[self._last_id] = future
        return future

    async def __aenter__(self) -> "Sweep":
        if self._proc is not None:
            raise RuntimeError("sweep process is already running")

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

        self._io_sock = await io_sock_accept
        worker = asyncio.create_task(
            self._worker_main(self._io_sock),
            name="sweep_main",
        )
        worker.add_done_callback(lambda _: None)  # worker should not raise

        return self

    async def __aexit__(self, _et: Any, ev: Any, _tb: Any) -> bool:
        if isinstance(ev, CancelledError):
            return True
        await self.terminate()
        return False

    def __aiter__(self) -> AsyncIterator["SweepRequest"]:
        return self

    async def __anext__(self) -> "SweepRequest":
        if self._proc is None:
            raise StopAsyncIteration
        event = await self._read_events
        return event

    def on(self, name: str, handler: Callable[["SweepRequest"], bool]) -> None:
        """Regester handler that will be called on event with mathching name

        Handler should return `True` value to continue reciving events.
        If `name` arguments is None handler will receive all events.
        """

        def filtered_handler(event: SweepRequest) -> bool:
            if name is not None and event.method != name:
                return True
            return handler(event)

        self._read_events.on(filtered_handler)

    async def terminate(self) -> None:
        """Terminate underlying sweep process"""
        proc, self._proc = self._proc, None
        io_sock, self._io_sock = self._io_sock, None
        if io_sock:
            io_sock.close()
        if proc is None:
            return

        # resolve all futures
        requests = self._read_requests.copy()
        self._read_requests.clear()
        for request in requests.values():
            request.cancel("sweep process has terminated")
        self._read_events(SweepRequest("quit", None, None))

        await proc.wait()

    async def candidates_extend(self, items: Iterable[Candidate]) -> None:
        """Extend candidates set"""
        time_start = time.monotonic()
        time_limit = 0.05
        batch: List[Candidate] = []
        for item in items:
            batch.append(item)

            time_now = time.monotonic()
            if time_now - time_start >= time_limit:
                time_start = time_now
                time_limit *= 1.25
                await self._call("haystack_extend", batch)
                batch.clear()
        if batch:
            await self._call("haystack_extend", batch)

    def candidates_clear(self) -> Awaitable[None]:
        """Clear all candidates"""
        return self._call("haystack_clear")

    def niddle_set(self, niddle: str) -> Awaitable[None]:
        """Set new niddle"""
        return self._call("niddle_set", niddle)

    def niddle_get(self) -> Awaitable[str]:
        return self._call("niddle_get")

    def key_binding(self, key: str, tag: str) -> Awaitable[None]:
        """Register new hotkey"""
        return self._call("key_binding", {"key": key, "tag": tag})

    def prompt_set(self, prompt: str) -> Awaitable[None]:
        """Set sweep's prompt string"""
        return self._call("prompt_set", prompt)

    def current(self) -> Awaitable[Optional[Candidate]]:
        """Currently selected element"""
        return self._call("current")


class SweepError(Exception):
    def __init__(self, code: int, message: str, data: str):
        self.code = code
        self.message = message
        self.data = data

    def __str__(self) -> str:
        return self.data


class SweepRequest(NamedTuple):
    method: str
    params: Any
    id: Optional[int]

    def encode(self) -> bytes:
        message: Dict[str, Any] = {
            "jsonrpc": "2.0",
            "method": self.method,
        }
        if self.params is not None:
            message["params"] = self.params
        if self.id is not None:
            message["id"] = self.id
        data = json.dumps(message).encode()
        header = f"{len(data)}\n".encode()
        return header + data


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

    return asyncio.create_task(accept())


# ------------------------------------------------------------------------------
# JSON RPC
# ------------------------------------------------------------------------------
# Rpc request|response id
RpcId = Union[int, str, None]


class RpcRequest(NamedTuple):
    method: str
    params: "RpcParams"
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
    def deserialize(cls, obj: Dict[str, Any]) -> Optional["RpcRequest"]:
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
    def deserialize(cls, obj: Dict[str, Any]) -> Optional["RpcResult"]:
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
    def deserialize(cls, obj: Dict[str, Any]) -> Optional["RpcError"]:
        error: Optional[Dict[str, Any]] = obj.get("error")
        if not isinstance(error, dict):
            return None
        code = error.get("code")
        if not isinstance(code, int):
            return None
        message = error.get("message")
        if not isinstance(message, str):
            return None
        return RpcError(code, message, error.get("data"), obj.get("id"))

    @classmethod
    def current(cls, *, data: Optional[str] = None, id: RpcId = None) -> "RpcError":
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
    def parse_error(cls, *, data: Optional[str] = None, id: RpcId = None) -> "RpcError":
        return RpcError(-32700, "Parse error", data, id)

    @classmethod
    def invalid_request(
        cls, *, data: Optional[str] = None, id: RpcId = None
    ) -> "RpcError":
        return RpcError(-32600, "Invalid request", data, id)

    @classmethod
    def method_not_found(
        cls, *, data: Optional[str] = None, id: RpcId = None
    ) -> "RpcError":
        return RpcError(-32601, "Method not found", data, id)

    @classmethod
    def invalid_params(
        cls, *, data: Optional[str] = None, id: RpcId = None
    ) -> "RpcError":
        return RpcError(-32602, "Invalid params", data, id)

    @classmethod
    def internal_error(
        cls, *, data: Optional[str] = None, id: RpcId = None
    ) -> "RpcError":
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

    _handlers: Dict[str, RpcHandler]  # registred handlers
    _requests: Dict[RpcId, "Future[Any]"]  # unanswerd requests
    _requests_next_id: int  # index used for next request
    _write_queue: Deque[RpcMessage]  # messages to be send to the other peer
    _write_notify: "Event[None]"  # event used to wake up writer
    _is_terminated: bool  # whether peer was terminated
    _serve_task: Optional["Future[Any]"]  # running serve task
    _events: "Event[RpcRequest]"  # received events (requests with id = None)

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
    def events(self) -> "Event[RpcRequest]":
        """Received events (requests with id = None)"""
        if self._is_terminated:
            raise RuntimeError("peer has already been terminated")
        return self._events

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
        """Conviniet way to call remote methods"""
        return partial(self.call, method)

    def terminate(self) -> None:
        if self._is_terminated:
            return
        self._is_terminated = True
        # cancel reqeusts and events
        requests = self._requests.copy()
        self._requests.clear()
        for request in requests.values():
            request.cancel("rpc peer has terminated")
        self._events.cancel()
        # cancel serve future
        if self._serve_task is not None:
            self._serve_task.cancel("rpc peer has terminated")

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
        """Asynchronus iterator of events (requests with id = None)"""
        return self

    async def __anext__(self) -> RpcRequest:
        if self._is_terminated is None:
            raise StopAsyncIteration
        event = await self._events
        return event

    def _submit_message(self, message: RpcMessage) -> None:
        """Sumbit message for sending to the other peer"""
        self._write_queue.append(message)
        self._write_notify(None)

    def _handle_message(self, message: RpcMessage) -> None:
        """Handle incomming messages"""
        if isinstance(message, RpcRequest):
            if message.id is None:
                self._events(message)
            handler = self._handlers.get(message.method)
            if handler is not None:
                asyncio.create_task(
                    self._handle_request(message, handler),
                    name="rpc handler for {message.method}",
                )
            elif message.id is not None:
                error = RpcError.method_not_found(
                    id=message.id, data=str(message.method)
                )
                self._submit_message(error)
        else:
            future = self._requests.get(message.id)
            if isinstance(message, RpcError):
                if message.id is None:
                    raise message
                if future is not None:
                    future.set_exception(message)
            elif future is not None:
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
        """Write subitted messages to the output stream"""
        while not self._is_terminated:
            if not self._write_queue:
                await self._write_notify
                continue
            data = self._write_queue.popleft().serialize()
            writer.write(f"{len(data)}\n".encode())
            writer.write(data)
            await writer.drain()

    async def _reader(self, reader: StreamReader) -> None:
        """Read and handle incomming messages"""
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
            except Exception:
                pass
        futures = self._futures.copy()
        self._futures.clear()
        for future in futures:
            future.set_result(event)

    def cancel(self):
        """Canel all waiting futures"""
        futures = self._futures.copy()
        self._futures.clear()
        for future in futures:
            future.cancel()

    def on(self, handler: Callable[[E], bool]) -> None:
        """Register event handler

        Handler is kept subscribed as long as it returns True
        """
        self._handlers.add(handler)

    def __await__(self) -> Generator[Any, None, E]:
        """Await for next event"""
        future: Future[E] = asyncio.get_running_loop().create_future()
        self._futures.add(future)
        return future.__await__()

    def __repr__(self) -> str:
        return f"Events(handlers={len(self._handlers)}, futures={len(self._futures)})"


# ------------------------------------------------------------------------------
# Tests
# ------------------------------------------------------------------------------
class Tests(unittest.IsolatedAsyncioTestCase):
    async def test_event(self) -> None:
        total: int = 0
        once: int = 0
        bad_count: int = 0

        def total_handler(value: int) -> bool:
            nonlocal total
            total += value
            return True

        def once_handler(value: int) -> bool:
            nonlocal once
            once += value
            return False

        def bad_handler(_: int) -> bool:
            nonlocal bad_count
            bad_count += 1
            raise RuntimeError()

        event = Event[int]()
        event.on(total_handler)
        event.on(once_handler)
        event.on(bad_handler)

        event(5)
        self.assertEqual(5, total)
        self.assertEqual(5, once)
        self.assertEqual(1, bad_count)
        event(3)
        self.assertEqual(8, total)
        self.assertEqual(5, once)
        self.assertEqual(1, bad_count)

        f = asyncio.ensure_future(event)
        await asyncio.sleep(0.01)  # yield
        self.assertFalse(f.done())
        event(6)
        await asyncio.sleep(0.01)  # yield
        self.assertEqual(14, total)
        self.assertTrue(f.done())
        self.assertEqual(6, await f)

    async def test_rpc(self) -> None:
        def send_handler(value: int) -> int:
            send(value)
            return value

        async def sleep() -> str:
            await asyncio.sleep(0.01)
            return "done"

        a = RpcPeer()
        a.register("name", lambda: "a")
        a.register("add", lambda a, b: a + b)
        send = Event[int]()
        a.register("send", send_handler)
        a.register("sleep", sleep)

        b = RpcPeer()
        b.register("name", lambda: "b")

        # connect
        a_sock, b_sock = socket.socketpair()
        a_serve = a.serve(*(await asyncio.open_unix_connection(sock=a_sock)))
        b_serve = b.serve(*(await asyncio.open_unix_connection(sock=b_sock)))
        serve = asyncio.gather(a_serve, b_serve)

        # events iter
        events: List[RpcRequest] = []
        async def event_iter():
            async for event in a:
                events.append(event)
        events_task = asyncio.ensure_future(event_iter())
        event = asyncio.ensure_future(a.events)
        await asyncio.sleep(0.01) # yield

        # errors
        with self.assertRaisesRegex(RpcError, "Method not found.*"):
            await b.call("blablabla")
        with self.assertRaisesRegex(RpcError, "Invalid params.*"):
            await b.call("name", 1)
        with self.assertRaisesRegex(RpcError, "cannot mix.*"):
            await b.call("add", 1, b=2)

        # basic calls
        self.assertEqual("a", await b.call("name"))
        self.assertEqual("b", await a.call("name"))
        self.assertFalse(event.done())

        # mixed calls with args and kwargs
        self.assertEqual(3, await b.call("add", 1, 2))
        self.assertEqual(3, await b.call("add", a=1, b=2))
        self.assertEqual("ab", await b.add(a="a", b="b"))

        # events
        s = asyncio.ensure_future(send)
        self.assertEqual(127, await b.send(value=127))
        self.assertEqual(127, await s)
        s = asyncio.ensure_future(send)
        b.notify("send", 17)
        self.assertEqual(17, await s)
        self.assertTrue(event.done())
        send_event = RpcRequest("send", [17], None)
        self.assertEqual(send_event, await event)
        self.assertEqual([send_event], events)
        b.notify("other", arg="something")
        other_event = RpcRequest("other", {"arg": "something"}, None)
        await asyncio.sleep(0.01)  # yield
        self.assertEqual([send_event, other_event], events)
        self.assertFalse(events_task.done())

        # asynchronous handler
        self.assertEqual("done", await b.call("sleep"))

        # terminate peers
        a.terminate()
        b.terminate()
        await asyncio.sleep(0.01)  # yield
        self.assertTrue(events_task.cancelled())
        await serve


# ------------------------------------------------------------------------------
# Main
# ------------------------------------------------------------------------------
async def main() -> None:
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
    args = parser.parse_args()

    candidates: List[Candidate]
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
    )

    if result is None:
        pass
    elif args.json:
        json.dump(result, sys.stdout)
    else:
        print(result)


if __name__ == "__main__":
    asyncio.run(main())
