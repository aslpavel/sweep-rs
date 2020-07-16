#! /usr/bin/env python3
from subprocess import Popen, PIPE
from typing import Optional, List, Any
import json
import select

__all__ = ("Sweep",)


SWEEP_SELECTED = "selected"
SWEEP_KEYBINDING = "key_binding"


class Sweep:
    """RPC wrapper around sweep process
    """

    def __init__(
        self,
        prompt="INPUT",
        nth: Optional[str] = None,
        height: int = 11,
        delimiter: Optional[str] = None,
        theme: Optional[str] = None,
        scorer: Optional[str] = None,
        tty: Optional[str] = None,
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
        self.proc = Popen(
            ["cargo", "run", "--", "--rpc", *args],
            # ["sweep", "--rpc", *args],
            stdout=PIPE,
            stdin=PIPE,
            # this is usefull if you want to redirecte to different `--tty` for debugging
            # and you want to ignore Ctrl-C comming from this pythone process.
            # start_new_session=True,
        )

    def candidates_extend(self, items: List[str]):
        """Extend candidates set"""
        rpc_encode(self.proc.stdin, "candidates_extend", items=items)

    def candidates_clear(self):
        """Clear all candidates"""
        rpc_encode(self.proc.stdin, "candidates_clear")

    def niddle_set(self, niddle: str):
        """Set new niddle"""
        rpc_encode(self.proc.stdin, "niddle_set", niddle=niddle)

    def key_binding(self, key: str, tag: Any):
        """Register new hotkey"""
        rpc_encode(self.proc.stdin, "key_binding", key=key, tag=tag)

    def terminate(self):
        """Terminate underlying sweep process"""
        if self.proc.poll() is None:
            rpc_encode(self.proc.stdin, "terminate")
        self.proc.wait()

    def poll(self, timeout: Optional[float] = None):
        """Wait for events from the sweep process"""
        message = rpc_decode(self.proc.stdout, timeout)
        if message is None:
            return
        error = message.get("error")
        if error is not None:
            raise RuntimeError("remote error: {}".format(error))
        for type in (SWEEP_SELECTED, SWEEP_KEYBINDING):
            result = message.get(type)
            if result is not None:
                return type, result
        raise RuntimeError("unknonw message type: {}".format(message))

    def __enter__(self):
        return self

    def __exit__(self, et, oe, tb):
        self.terminate()
        return False

    def __del__(self):
        self.terminate()


def rpc_encode(output, method, **args):
    """Encode RPC method"""
    message = {
        "method": method,
        **args,
    }
    data = json.dumps(message).encode()
    output.write(f"{len(data)}\n".encode())
    output.write(data)
    output.flush()


def rpc_decode(input, timeout=None):
    """Decode RPC message"""
    rlist, _, _ = select.select([input], [], [], timeout)
    if rlist:
        size = input.readline().strip()
        if not size:
            return
        return json.loads(input.read(int(size)))


def main():
    """Dir traversal demo
    """
    import sys
    import pathlib

    if len(sys.argv) == 1:
        path = pathlib.Path()
    else:
        path = pathlib.Path(sys.argv[1])

    with Sweep(prompt="WALK", nth="1..") as sweep:
        sweep.key_binding("ctrl+b", 0)
        while path.is_dir():

            items = list(path.iterdir())
            if not items:
                break
            sweep.candidates_clear()
            candidates = []
            for item in items:
                file_type = "D" if item.is_dir() else "F"
                candidates.append("{} {}".format(file_type, item.name))
            sweep.candidates_extend(candidates)
            sweep.niddle_set("")

            result = sweep.poll()
            if result is None:
                break
            type, value = result
            if type == SWEEP_SELECTED:
                if value is None:
                    return
                _, name = value.split(maxsplit=1)
                path = path / name
            elif type == SWEEP_KEYBINDING:
                if value == 0:
                    path = path.parent

    print(path)


if __name__ == "__main__":
    main()
