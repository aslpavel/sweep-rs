#! /usr/bin/env python3
from subprocess import Popen, PIPE
import json
import select


class Sweep:
    def __init__(self, args=None):
        args = [] if args is None else args
        self.proc = Popen(
            ["sweep", "--rpc", *args],
            stdout=PIPE,
            stdin=PIPE,
            # this is usefull if you want to redirecte to different `--tty` for debugging
            # and you want to ignore Ctrl-C comming from this pythone process.
            # start_new_session=True,
        )

    def candidates_extend(self, items):
        rpc_encode(self.proc.stdin, "candidates_extend", items=items)

    def candidates_clear(self):
        rpc_encode(self.proc.stdin, "candidates_clear")

    def niddle_set(self, niddle):
        rpc_encode(self.proc.stdin, "niddle_set", niddle=niddle)

    def terminate(self):
        if self.proc.poll() is None:
            rpc_encode(self.proc.stdin, "terminate")
        self.proc.wait()

    def poll(self, timeout=None):
        message = rpc_decode(self.proc.stdout, timeout)
        if message is None:
            return
        error = message.get("error")
        if error is not None:
            raise RuntimeError("remote error: {}".format(error))
        selected = message.get("selected")
        if selected is not None:
            return selected
        raise RuntimeError("unknonw message type: {}".format(message))

    def __enter__(self):
        return self

    def __exit__(self, et, oe, tb):
        self.terminate()
        return False

    def __del__(self):
        self.terminate()


def rpc_encode(output, method, **args):
    message = {
        "method": method,
        **args,
    }
    data = json.dumps(message).encode()
    output.write(f"{len(data)}\n".encode())
    output.write(data)
    output.flush()


def rpc_decode(input, timeout=None):
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

    with Sweep(["--nth", "1.."]) as sweep:
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
            item = sweep.poll()
            if item is None:
                return
            _, name = item.split(maxsplit=1)
            path = path / name

    print(path)


if __name__ == "__main__":
    main()
