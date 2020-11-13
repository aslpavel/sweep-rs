#! /usr/bin/env python3
"""Very simple RPC interface around sweep command
"""
from collections import deque
from subprocess import Popen, PIPE, TimeoutExpired
from typing import Optional, List, Any, Tuple, Dict
import json
import select

__all__ = ("Sweep",)


NOT_USED = object()
SWEEP_SELECTED = "select"
SWEEP_KEYBINDING = "bind"


class Sweep:
    """RPC wrapper around sweep process

    DEBUGGING:
        - Load this file as python module.
        - Open other terminal window and execute `$ tty` command, then run something that
          will not steal characters for sweep process like `$ sleep 1000`.
        - Instantiate Sweep class with the tty device path of the other terminal.
        - Now you can call all the methods of the Sweep class in an interractive mode.
    """

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
        self.proc = Popen(
            [*sweep, "--rpc", *args],
            stdout=PIPE,
            stdin=PIPE,
            # this is usefull if you want to redirecte to different `--tty` for debugging
            # and you want to ignore Ctrl-C comming from this pythone process.
            # start_new_session=True,
        )
        self.events: List[Tuple[str, Any]] = deque()
        self.last_id = 0

    def candidates_extend(self, items: List[str]):
        """Extend candidates set"""
        return self.call("haystack_extend", items)

    def candidates_clear(self):
        """Clear all candidates"""
        return self.call("haystack_clear")

    def niddle_set(self, niddle: str):
        """Set new niddle"""
        return self.call("niddle_set", niddle)

    def key_binding(self, key: str, tag: Any):
        """Register new hotkey"""
        return self.call("key_binding", {"key": key, "tag": tag})

    def prompt_set(self, prompt: str):
        """Set sweep's prompt string"""
        return self.call("prompt_set", prompt)

    def current(self, timeout=None):
        """Currently selected element"""
        return self.call("current")

    def terminate(self):
        """Terminate underlying sweep process"""
        if not hasattr(self, "proc"):
            return
        if self.proc.poll() is None:
            self.call("terminate", timeout=1)
        try:
            self.proc.wait(1)
        except TimeoutExpired:
            self.proc.terminate()

    def call(
        self, method: str, params: Any = None, timeout: Optional[float] = None
    ) -> Any:
        """Call remote sweep method and wait for response"""
        self.last_id += 1
        rpc_encode(self.proc.stdin, method, params, id=self.last_id)
        return self.poll(timeout, self.last_id)

    def poll(self, timeout: Optional[float] = None, id=None) -> Any:
        """Wait for responses/events from the sweep process
        """
        if id is None and self.events:
            return self.events.popleft()
        while True:
            msg = rpc_decode(self.proc.stdout, timeout)
            if msg is None:
                return None

            # handle errors
            error = msg.get("error")
            if error is not None:
                if not isinstance(error, dict):
                    raise RPCError(
                        -32700, "Parse error", "Error must be an object: {}".format(msg)
                    )
                raise RPCError(
                    error.get("code", -32603),
                    error.get("message"),
                    error.get("data", ""),
                )

            # collect events events
            method = msg.get("method")
            if method:
                event = (method, msg.get("params"))
                if id is None:
                    return event
                self.events.append(event)

            # ignore all but matching events
            result = msg.get("result")
            if msg.get("id") == id:
                return result

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.terminate()
        return False

    def __del__(self):
        self.terminate()


class RPCError(Exception):
    def __init__(self, code, message, data):
        self.code = code
        self.message = message
        self.data = data

    def __str__(self):
        return "{} ({})".format(self.message, self.data)


def rpc_encode(output, method, params=NOT_USED, id=NOT_USED):
    """Encode JSON-RPC method"""
    message = {
        "jsonrpc": "2.0",
        "method": method,
    }
    if params is not NOT_USED:
        message["params"] = params
    if id is not NOT_USED:
        message["id"] = id
    data = json.dumps(message).encode()
    output.write(f"{len(data)}\n".encode())
    output.write(data)
    output.flush()


def rpc_decode(file, timeout=None):
    """Decode JSON-RPC message"""
    rlist, _, _ = select.select([file], [], [], timeout)
    if rlist:
        size = file.readline().strip()
        if not size:
            return None
        return json.loads(file.read(int(size)))
    return None


def sweep(chandidates: List[Any], **options: Dict[str, Any]) -> Any:
    """Convinience wrapper around `Sweep` when you only need to select one
    candidate from the list
    """
    with Sweep(**options) as sweep:
        sweep.candidates_extend(chandidates)
        while True:
            msg = sweep.poll()
            if msg is None:
                break
            msg_type, value = msg
            if msg_type == SWEEP_SELECTED:
                return value


def main():
    """Simple example of using Sweep class

    Walks directory tree. Returns path when file or empty directory is selected.
    `ctrl+b` is used to go one level up.
    """
    # noqa: import-outside-toplevel
    import sys
    import pathlib

    if len(sys.argv) == 1:
        path = pathlib.Path()
    else:
        path = pathlib.Path(sys.argv[1])
    path = path.resolve()

    with Sweep(
        sweep=["cargo", "run", "--"], height=16, prompt="WALK", nth="1.."
    ) as sweep:
        sweep.key_binding("ctrl+b", 0)
        while path.is_dir():
            paths = list(path.iterdir())
            if not paths:
                break

            candidates = []
            for item in paths:
                file_type = "D" if item.is_dir() else "F"
                candidates.append("{} {}".format(file_type, item.name))
            candidates.sort()

            sweep.prompt_set(f"WALK: {path}")
            sweep.candidates_clear()
            sweep.candidates_extend(candidates)
            sweep.niddle_set("")

            msg = sweep.poll()
            if msg is None:
                break
            msg_type, value = msg
            if msg_type == SWEEP_SELECTED:
                if value is None:
                    return
                _, name = value.split(maxsplit=1)
                path = path / name
            elif msg_type == SWEEP_KEYBINDING:
                if value == 0:
                    path = path.parent

    print(path)


if __name__ == "__main__":
    main()
