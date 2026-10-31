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

    It has no worker, so `shutdown` joins nothing and only its stop half is
    under test.
    """
    srv = explore._Server(("127.0.0.1", 0), _StubVisualizer())
    port = srv.server_address[1]
    explore._RUNNING[port] = srv
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


def test_giving_up_on_a_server_that_never_served_leaves_it_able_to_serve(
    monkeypatch, unserved_server
):
    """`stopping` cuts a body write, so a `shutdown` that reports stopping
    nothing must not leave it set: every later response would go out with its
    headers and no body."""
    srv, port = unserved_server
    monkeypatch.setattr(explore, "_SHUTDOWN_START_SECONDS", 0.2)
    assert explore.shutdown(port) == []
    assert not srv.stopping.is_set()


class _BrokenStdout:
    def write(self, s: str) -> int:
        raise BrokenPipeError(32, "Broken pipe")

    def flush(self) -> None:
        pass


def test_a_url_print_that_raises_leaves_no_server_behind(monkeypatch):
    """A foreground server whose URL cannot be printed never serves, so it
    must not stay registered with its socket bound: every later `shutdown`,
    the one at exit included, would wait on it."""
    made: list[explore._Server] = []

    class _Recorded(explore._Server):
        def __init__(self, *args: Any, **kwargs: Any) -> None:
            super().__init__(*args, **kwargs)
            made.append(self)

    monkeypatch.setattr(explore, "_Server", _Recorded)
    before = set(explore._RUNNING)
    monkeypatch.setattr("sys.stdout", _BrokenStdout())
    with pytest.raises(BrokenPipeError):
        explore._serve(_StubVisualizer(), host="127.0.0.1", port=0)
    monkeypatch.undo()
    assert set(explore._RUNNING) == before
    assert made[0].socket.fileno() == -1, "the listening socket is still open"
