#!/usr/bin/env python3
"""Asynchronous JSON-RPC implementation to communicate with sweep command
"""
import asyncio
import json
import os
import socket
import sys
import tempfile
import time
import traceback
from asyncio.futures import Future
from asyncio.streams import StreamReader, StreamWriter
from asyncio.subprocess import Process
from collections import deque
from typing import (
    Any,
    Awaitable,
    Callable,
    Deque,
    Dict,
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
    ):
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
        self._io_sock = None
        self._last_id = 0
        self._read_events: Event[SweepRequest] = Event()
        self._read_requests: Dict[int, Future[Any]] = {}
        self._write_queue: Deque[SweepRequest] = deque()
        self._write_notify: Event[None] = Event()

    async def _worker_main(self, sock: socket.socket):
        """Main worker coroutine which reads and write data to/from sweep"""
        if self._proc is None:
            return
        try:
            reader, writer = await asyncio.open_unix_connection(sock=sock)
            await asyncio.gather(
                self._worker_writer(writer),
                self._worker_reader(reader),
            )
        except asyncio.CancelledError:
            pass
        except Exception:
            sys.stderr.write("sweep worker failed with error:\n")
            traceback.print_exc(file=sys.stderr)
        finally:
            await self.terminate()

    async def _worker_writer(self, writer: StreamWriter):
        """Write outging messages"""
        while self._proc is not None:
            if not self._write_queue:
                await self._write_notify
                continue
            writer.write(self._write_queue.popleft().encode())
            await writer.drain()
        raise asyncio.CancelledError()

    async def _worker_reader(self, reader: StreamReader):
        """Read and dispatch incomming messages from the reader"""
        while self._proc is not None:
            size = await reader.readline()
            if not size:
                break
            size = int(size.strip())
            data = await reader.read(size)
            if not data:
                break
            self._read_dispatch(json.loads(data))
        raise asyncio.CancelledError()

    def _read_dispatch(self, msg: Any):
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

    def _call(self, method: str, params: Optional[Any] = None) -> Future[Any]:
        future: Future[Any] = asyncio.get_running_loop().create_future()
        if self._proc is None:
            future.set_exception(RuntimeError("sweep process is not running"))
        else:
            self._last_id += 1
            self._write_queue.append(SweepRequest(method, params, self._last_id))
            self._write_notify(None)
            self._read_requests[self._last_id] = future
        return future

    async def __aenter__(self):
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

    async def __aexit__(self, *_):
        await self.terminate()

    def __aiter__(self):
        return self

    async def __anext__(self):
        if self._proc is None:
            raise StopAsyncIteration
        event = await self._read_events
        return event

    def on(self, name: str, handler: Callable[["SweepRequest"], bool]):
        """Regester handler that will be called on event with mathching name

        Handler should return `True` value to continue reciving events.
        If `name` arguments is None handler will receive all events.
        """

        def filtered_handler(event: SweepRequest):
            if name is not None and event.method != name:
                return True
            return handler(event)

        self._read_events.on(filtered_handler)

    async def terminate(self):
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

    def encode(self):
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


E = TypeVar("E")


class Event(Generic[E]):
    __slots__ = ["_handlers"]

    def __init__(self):
        self._handlers: Set[Callable[[E], bool]] = set()

    def __call__(self, event: E):
        handlers = self._handlers.copy()
        self._handlers.clear()
        for handler in handlers:
            if handler(event):
                self._handlers.add(handler)

    def on(self, handler: Callable[[E], bool]):
        self._handlers.add(handler)

    def __await__(self):
        def handler(event: E) -> bool:
            future.set_result(event)
            return False

        future: Future[E] = asyncio.get_running_loop().create_future()
        self.on(handler)
        value = yield from future
        return value


def unix_server_once(path: str) -> Future[socket.socket]:
    """Create unix server socket and accept one connection"""
    loop = asyncio.get_running_loop()
    if os.path.exists(path):
        os.unlink(path)
    server = socket.socket(socket.AF_UNIX)
    server.bind(path)
    server.listen()

    async def accept():
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


async def main():
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

    if args.json:
        candidates = cast(List[Candidate], json.load(args.input))
    else:
        candidates: List[Candidate] = []
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
