#! /usr/bin/env python3
"""Very simple RPC interface around sweep command
"""
from subprocess import Popen, PIPE
from typing import Optional, List, Any, Tuple
import json
import select

__all__ = ("Sweep",)


SWEEP_SELECTED = "selected"
SWEEP_KEYBINDING = "key_binding"


class Sweep:
    """RPC wrapper around sweep process

    DEBUGGING:
        - Load this file a python module.
        - Open other terminal window and execute `$ tty` command, then something which
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
        self.proc = Popen(
            [*sweep, "--rpc", *args],
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

    def prompt_set(self, prompt: str):
        """Set sweep's prompt string"""
        rpc_encode(self.proc.stdin, "prompt_set", prompt=prompt)

    def terminate(self):
        """Terminate underlying sweep process"""
        if self.proc.poll() is None:
            rpc_encode(self.proc.stdin, "terminate")
        self.proc.wait()

    def poll(self, timeout: Optional[float] = None) -> Optional[Tuple[str, Any]]:
        """Wait for events from the sweep process"""
        msg = rpc_decode(self.proc.stdout, timeout)
        if msg is None:
            return None
        error = msg.get("error")
        if error is not None:
            raise RuntimeError("remote error: {}".format(error))
        for msg_type in (SWEEP_SELECTED, SWEEP_KEYBINDING):
            result = msg.get(msg_type)
            if result is not None:
                return msg_type, result
        raise RuntimeError("unknonw message type: {}".format(msg))

    def __enter__(self):
        return self

    def __exit__(self, *_):
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


def rpc_decode(file, timeout=None):
    """Decode RPC message"""
    rlist, _, _ = select.select([file], [], [], timeout)
    if rlist:
        size = file.readline().strip()
        if not size:
            return None
        return json.loads(file.read(int(size)))
    return None


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
