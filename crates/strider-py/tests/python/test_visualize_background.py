"""`visualize(background=True)`: the explorer serves on its own thread while
the calling thread keeps querying."""

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


def test_blocking_mode_is_still_the_default():
    """`background` defaults off, so an existing caller still blocks."""
    import inspect

    sig = inspect.signature(strider.explore.visualize)
    assert sig.parameters["background"].default is False
