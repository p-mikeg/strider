"""`shutdown` must not park forever on a server that never began serving.

`_serve` publishes into `_RUNNING` before entering `serve_forever`, and
CPython's `BaseServer.shutdown` waits on an event only the serve loop sets, so
a `shutdown` landing in that window, the `threading._register_atexit` hook
included, has nothing to wake it.
"""

from __future__ import annotations

import threading
from typing import Any

import pytest

from strider import explore


class _StubVisualizer:
    """The `_Visualizer` shape `_Server.__init__` reads, no IR behind it."""

    def entry(self) -> int:
        return 0

    def controls(self) -> list[dict[str, Any]]:
        return [{"name": "depth", "kind": "int", "default": 5}]

    def dot(self, center: int, params: dict[str, Any]) -> str:
        return "digraph {}"

    def search(self, query: str) -> dict[str, Any]:
        return {"highlight": []}

    def completions(self) -> list[str]:
        return []


@pytest.fixture
def unserved_server():
    """A bound server registered as running, which nothing ever serves.

    The registered thread has already finished, so `shutdown`'s join is a
    no-op and only its stop half is under test.
    """
    srv = explore._Server(("127.0.0.1", 0), _StubVisualizer())
    port = srv.server_address[1]
    finished = threading.Thread(target=lambda: None)
    finished.start()
    finished.join()
    explore._RUNNING[port] = (srv, finished)
    try:
        yield srv, port
    finally:
        explore._RUNNING.pop(port, None)
        srv.visualizer = None
        srv.server_close()


def test_shutdown_returns_when_serving_never_started(monkeypatch, unserved_server):
    _srv, port = unserved_server
    monkeypatch.setattr(explore, "_SHUTDOWN_START_SECONDS", 0.2)
    done = threading.Event()
    # Daemon: without the fix this thread parks forever, and a non-daemon one
    # would hang the interpreter at exit.
    threading.Thread(
        target=lambda: (explore.shutdown(port), done.set()), daemon=True
    ).start()
    assert done.wait(5.0), "shutdown blocked on a server that never served"


def test_serving_flag_starts_clear(unserved_server):
    srv, _port = unserved_server
    assert not srv.started.is_set()
