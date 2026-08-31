"""`visualize(background=True)`: the explorer serves on its own thread while
the calling thread keeps querying."""

import socket
import threading
import urllib.error
import urllib.parse
import urllib.request

import strider

from .conftest import fixture_path


def _get(port, path):
    with urllib.request.urlopen(f"http://127.0.0.1:{port}{path}", timeout=15) as r:
        return r.read().decode()


def _analyzed():
    lift = strider.lift.load_elf(str(fixture_path("x64", "switch")))
    return lift.analyze(lift.entry_point())


def test_background_serves_and_leaves_the_calling_thread_free():
    """The point of the mode: browse and query at the same time. Rendering runs
    on the server thread through a decoder of its own, so the caller's handle
    is never borrowed out from under it."""
    fn = _analyzed().function
    port = strider.explore.visualize(fn, background=True)
    try:
        entry = _get(port, "/entry").strip()
        # pretty rendering resolves register names, which is the path that
        # needs a decoder at all
        dot = _get(port, f"/dot?center={entry}&pretty=1")
        assert dot.startswith("digraph")
        # ...while this thread keeps using the original handle
        assert "digraph" in (fn.to_dot(pretty=True) or "")
        assert fn.node_count() > 0
    finally:
        assert strider.explore.shutdown(port) == [port]


def test_background_serves_a_cfg_too():
    cfg = _analyzed().cfg
    port = strider.explore.visualize(cfg, background=True)
    try:
        entry = _get(port, "/entry").strip()
        assert _get(port, f"/dot?center={entry}").startswith("digraph")
        assert "digraph" in (cfg.to_dot() or "")
    finally:
        assert strider.explore.shutdown(port) == [port]


def test_pattern_search_works_from_the_server_thread():
    """Matching reads the IR only, so it never needed a decoder."""
    fn = _analyzed().function
    port = strider.explore.visualize(fn, background=True)
    try:
        q = urllib.parse.quote("anything()")
        assert "highlight" in _get(port, f"/pattern?q={q}")
    finally:
        strider.explore.shutdown(port)


def test_concurrent_render_and_query_do_not_disturb_each_other():
    fn = _analyzed().function
    port = strider.explore.visualize(fn, background=True)
    entry = _get(port, "/entry").strip()
    errors: list[str] = []
    stop = threading.Event()

    def hammer():
        while not stop.is_set():
            try:
                _get(port, f"/dot?center={entry}&pretty=1")
            except Exception as exc:  # noqa: BLE001 - recorded, asserted below
                errors.append(f"render: {exc}")
                return

    t = threading.Thread(target=hammer, daemon=True)
    t.start()
    try:
        for _ in range(25):
            assert "digraph" in (fn.to_dot(pretty=True) or "")
    finally:
        stop.set()
        t.join(timeout=10)
        strider.explore.shutdown(port)
    assert not errors, errors


def test_rendering_works_while_the_caller_analyses():
    """The whole point of the mode, and the case CI missed.

    Rendering must not borrow the handle `analyze` holds. It holds it mutably
    with the GIL RELEASED, so anything on the render path that reaches for it
    fails for the entire length of an analysis -- which is exactly when someone
    is watching the page. Hammering with `to_dot` from the main thread does not
    catch this, because a shared borrow is fine; only a concurrent `analyze`
    does."""
    lift = strider.lift.load_elf(str(fixture_path("x64", "switch")))
    sym = next(iter(lift.functions()))
    result = lift.analyze(sym.address)
    # Every render path: the whole-graph one the page opens on, the
    # neighborhood one behind the toggle, and the Cfg explorer, which passes a
    # lifter unconditionally.
    for target, query in (
        (result.function, "pretty=1"),
        (result.function, "whole=0&pretty=1"),
        (result.cfg, "whole=0"),
    ):
        port = strider.explore.visualize(target, background=True)
        entry = _get(port, "/entry").strip()
        ok = 0
        failures: list[str] = []
        stop = threading.Event()

        def render(port=port, entry=entry, query=query, failures=failures):
            nonlocal ok
            while not stop.is_set():
                try:
                    _get(port, f"/dot?center={entry}&{query}")
                    ok += 1
                except Exception as exc:  # noqa: BLE001 - asserted below
                    failures.append(str(exc))
                    return

        t = threading.Thread(target=render, daemon=True)
        t.start()
        try:
            for _ in range(30):
                lift.analyze(sym.address)
        finally:
            stop.set()
            t.join(timeout=30)
            strider.explore.shutdown(port)
        assert not failures, (query, failures[:3])
        assert ok > 0, f"{query}: the render thread never completed a render"


def test_a_connection_that_sends_nothing_does_not_wedge_the_server():
    """A browser's speculative preconnect is a socket that sends nothing.

    The serve loop is single-threaded, so without a read timeout that one
    connection blocks every later request forever, `shutdown` cannot stop a
    loop parked in `finish_request`, and the interpreter's own join -- which
    has no timeout -- inherits the wait. The timeout must also stay under
    `_SHUTDOWN_JOIN_SECONDS`, or the connection outlives the join and the hang
    comes back one level up."""
    assert strider.explore._Handler.timeout is not None
    assert strider.explore._Handler.timeout < strider.explore._SHUTDOWN_JOIN_SECONDS

    lift = strider.lift.load_elf(str(fixture_path("x64", "switch")))
    fn = lift.analyze(next(iter(lift.functions())).address).function
    port = strider.explore.visualize(fn, background=True)
    quiet = socket.create_connection(("127.0.0.1", port))
    try:
        entry = _get(port, "/entry").strip()
        assert _get(port, f"/dot?center={entry}&pretty=1").startswith("digraph")
    finally:
        quiet.close()
        assert strider.explore.shutdown(port) == [port]


def test_blocking_mode_is_still_the_default():
    """`background` defaults off, so an existing caller still blocks."""
    import inspect

    sig = inspect.signature(strider.explore.visualize)
    assert sig.parameters["background"].default is False
