from __future__ import annotations

import asyncio
import socket
import unittest
import warnings
from typing import List, Any

from .sweep import Event, RpcError, RpcPeer, RpcRequest


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

        with warnings.catch_warnings():
            warnings.simplefilter("ignore")
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

        async def add(a: Any, b: Any) -> Any:
            return a + b

        a = RpcPeer()
        a.register("name", lambda: "a")
        a.register("add", add)
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

        async def event_iter() -> None:
            async for event in a:
                events.append(event)

        events_task = asyncio.ensure_future(event_iter())
        event = asyncio.ensure_future(a.events)
        await asyncio.sleep(0.01)  # yield

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


if __name__ == "__main__":
    unittest.main()
