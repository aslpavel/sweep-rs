#!/usr/bin/env python3
"""Asynchronous JSON-RPC implementation to communicate with sweep command
"""
from collections import deque
from typing import Optional, List, Any, NamedTuple, Dict
import asyncio
import json
import os
import socket
import sys
import traceback

__all__ = ["Sweep", "SweepError", "sweep", "SWEEP_SELECTED", "SWEEP_KEYBINDING"]

SWEEP_SELECTED = "select"
SWEEP_KEYBINDING = "bind"


async def sweep(chandidates: List[Any], **options: Dict[str, Any]) -> Any:
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
        sweep=["sweep"],
        prompt="INPUT",
        nth: Optional[str] = None,
        height: int = 11,
        delimiter: Optional[str] = None,
        theme: Optional[str] = None,
        scorer: Optional[str] = None,
        tty: Optional[str] = None,
        debug: bool = False,
        title: Optional[str] = None,
        keep_order=False,
        no_match: Optional[str] =None,
    ):
        args = []
        args.extend(["--prompt", prompt])
        args.extend(["--height", str(height)])
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

        self._args = [*sweep, "--rpc", *args]
        self._proc = None
        self._io_sock = None
        self._last_id = 0
        self._read_events = Event()
        self._read_requests = {}
        self._write_queue = deque()
        self._write_notify = Event()

    async def _worker_main(self, sock):
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

    async def _worker_writer(self, writer):
        """Write outging messages"""
        while self._proc is not None:
            if not self._write_queue:
                await self._write_notify
                continue
            writer.write(self._write_queue.popleft())
            await writer.drain()
        raise asyncio.CancelledError()

    async def _worker_reader(self, reader):
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

    def _read_dispatch(self, msg):
        """Handle incomming messages"""
        future = self._read_requests.pop(msg.get("id"), None)

        # handle errors
        error = msg.get("error")
        if error is not None:
            if not isinstance(error, dict):
                error = SweepError(
                    -32700, "Parse error", f"Error must be an object: {msg}"
                )
            else:
                error = SweepError(
                    error.get("code", -32603),
                    error.get("message"),
                    error.get("data", ""),
                )
            if future is None:
                raise error
            else:
                future.set_exception(error)
            return

        # handle events
        method = msg.get("method")
        if method:
            event = SweepRequest(method, msg.get("params"), None)
            self._read_events(event)
            return

        # handle results
        result = msg.get("result")
        future.set_result(result)

    def _call(self, method, params=None):
        future = asyncio.get_running_loop().create_future()
        if self._proc is None:
            future.set_exception(RuntimeError("sweep process is not running"))
        else:
            self._last_id += 1
            request = SweepRequest(method, params, self._last_id)
            self._write_queue.append(request.encode())
            self._write_notify(None)
            self._read_requests[self._last_id] = future
        return future

    async def __aenter__(self):
        if self._proc is not None:
            raise RuntimeError("sweep process is already running")

        io_sock_path = "/tmp/sweep-io-{}.socket".format(os.getpid())
        io_sock_future = asyncio.create_task(unix_server_once(io_sock_path))

        prog, *args = self._args
        self._proc = await asyncio.create_subprocess_exec(
            prog,
            *[*args, "--io-socket", io_sock_path],
        )

        self._io_sock = await asyncio.wait_for(io_sock_future, 1.0)
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

    def on(self, name, handler):
        """Regester handler that will be called on event with mathching name

        Handler should return `True` value to continue reciving events.
        If `name` arguments is None handler will receive all events.
        """

        def filtered_handler(event):
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
        requests, self._read_requests = self._read_requests, {}
        for request in requests.values():
            request.cancel("sweep process has terminated")
        self._read_events(SweepRequest("quit", None, None))

        await proc.wait()

    def candidates_extend(self, items: List[str]):
        """Extend candidates set"""
        return self._call("haystack_extend", items)

    def candidates_clear(self):
        """Clear all candidates"""
        return self._call("haystack_clear")

    def niddle_set(self, niddle: str):
        """Set new niddle"""
        return self._call("niddle_set", niddle)

    def key_binding(self, key: str, tag: Any):
        """Register new hotkey"""
        return self._call("key_binding", {"key": key, "tag": tag})

    def prompt_set(self, prompt: str):
        """Set sweep's prompt string"""
        return self._call("prompt_set", prompt)

    def current(self, timeout=None):
        """Currently selected element"""
        return self._call("current")


class SweepError(Exception):
    def __init__(self, code, message, data):
        self.code = code
        self.message = message
        self.data = data

    def __str__(self):
        return self.data


class SweepRequest(NamedTuple):
    method: str
    params: Any
    id: Optional[int]

    def encode(self):
        message = {
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


class Event:
    __slots__ = ["_handlers"]

    def __init__(self):
        self._handlers = set()

    def __call__(self, event):
        handlers, self._handlers = self._handlers, set()
        for handler in handlers:
            if handler(event):
                self._handlers.add(handler)

    def on(self, handler):
        self._handlers.add(handler)

    def __await__(self):
        future = asyncio.get_running_loop().create_future()
        self.on(future.set_result)
        value = yield from future
        return value


async def unix_server_once(path):
    """Create unix server socket and accept one connection"""
    loop = asyncio.get_running_loop()
    if os.path.exists(path):
        os.unlink(path)
    server = socket.socket(socket.AF_UNIX)
    server.bind(path)
    server.listen()
    try:
        accept = loop.create_future()
        loop.add_reader(server.fileno(), lambda: accept.set_result(None))
        await accept
        (client, _address) = server.accept()
        return client
    finally:
        loop.remove_reader(server.fileno())
        os.unlink(path)
        server.close()


async def main():
    import argparse

    parser = argparse.ArgumentParser(description="Sweep is a command line fuzzy finder")
    parser.add_argument(
        "-p", "--prompt", default="INPUT", help="override prompt string"
    )
    parser.add_argument(
        "--nth", help="comma-seprated list of fields for limiting search"
    )
    parser.add_argument("--delimiter", help="filed delimiter")
    parser.add_argument("--theme", help="theme as a list of comma separated attributes")
    parser.add_argument("--scorer", help="default scorer")
    parser.add_argument("--tty", help="tty device path")
    parser.add_argument("--height", type=int, help="height in lines")
    parser.add_argument(
        "--keep-order", help="keep order of elements (do not use ranking score)"
    )
    args, _unkown = parser.parse_known_args()

    candidates = []
    for line in sys.stdin:
        candidates.append(line.strip())

    result = await sweep(
        candidates,
        prompt=args.prompt,
        nth=args.nth,
        height=args.height or 11,
        delimiter=args.delimiter,
        theme=args.theme,
        scorer=args.scorer,
        tty=args.tty,
        keep_order=args.keep_order,
    )
    print(result)


if __name__ == "__main__":
    asyncio.run(main())
