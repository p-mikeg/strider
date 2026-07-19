"""Explorer HTTP surface, plus the shutdown contract that keeps it from
crashing the interpreter.

These tests used to start `visualize` on a daemon thread and never stop
it. That leaks a thread parked inside the Rust `PyLifter::visualize`
frame; when the interpreter finalizes, CPython kills the daemon thread
via `pthread_exit`, and the resulting glibc forced unwind has to cross
PyO3's `catch_unwind` trampoline, which converts rather than rethrows it.
glibc's `__pthread_unwind` then aborts the process — SIGABRT at exit,
after every test had already passed. It reproduced roughly one run in
seven: it needs a 500ms `serve_forever` poll tick to land inside the
finalization window, so only a teardown as slow as a full pytest session
is long enough to get hit.

Every test here now shuts its server down and joins the thread, and the
join assertion IS the regression test: a leaked explorer thread fails
deterministically here instead of aborting the interpreter one run in
seven.
"""

import json
import threading
import time
import urllib.request

import strider
import strider.explore


def _serve_bg(target_kind, port):
    """Start the explorer on a background thread; return the thread."""

    def run():
        lift = strider.lift.lifter(
            strider.sleigh.SleighArch.x86_64(),
            strider.reader.BufferReader(0x1000, b"\x75\x01\x90\xc3"),
        )
        cfg = lift.build_cfg(0x1000)
        if target_kind == "cfg":
            target = cfg
        else:
            target = lift.analyze(
                0x1000, strider.sleigh.CallingConvention.x86_64_systemv()
            ).function
        lift.visualize(target, port=port)

    # Non-daemon on purpose. A daemon thread lets the interpreter exit with
    # the server still parked in the Rust frame — the abort above.
    # Non-daemon turns that leak into a hang at exit, which is loud and
    # debuggable rather than intermittent and fatal.
    t = threading.Thread(target=run, daemon=False)
    t.start()
    return t


def _stop(port, thread):
    """Shut the server down and require the thread to actually die."""
    assert strider.explore.shutdown(port) == [port], f"no explorer on port {port}"
    thread.join(timeout=10)
    assert not thread.is_alive(), (
        "explorer thread survived shutdown() — a thread left parked inside "
        "the Rust visualize frame aborts the interpreter at finalization"
    )


def _get(port, path):
    for _ in range(40):
        try:
            return (
                urllib.request.urlopen(f"http://127.0.0.1:{port}{path}", timeout=8)
                .read()
                .decode()
            )
        except Exception:
            time.sleep(0.3)
    raise RuntimeError("server never came up")


def test_visualize_cfg_serves_neighborhood_and_search():
    port = 8931
    t = _serve_bg("cfg", port)
    try:
        entry = int(_get(port, "/entry"))
        pretty = _get(port, f"/dot?center={entry}&depth=1&raw=0")
        # `Cfg` dropped its raw (structure-faithful) neighborhood view as part
        # of the renderer-method unification (`Cfg.raw_neighborhood_dot` was
        # removed — the raw view lives on `Function`, over IR node ids, a
        # different id space than a CFG region index). The explorer's "raw"
        # toggle is now a no-op for a Cfg-backed visualizer: both modes render
        # the same pretty neighborhood.
        raw = _get(port, f"/dot?center={entry}&depth=1&raw=1")
        assert "#ffcc00" in pretty              # center highlighted
        assert raw == pretty
        # address search centers the containing block
        res = json.loads(_get(port, "/pattern?q=0x1000"))
        assert res.get("center") is not None or res.get("highlight") is not None
        # frontend loads
        assert "viz" in _get(port, "/").lower()
    finally:
        _stop(port, t)


def test_visualize_ir_still_works():
    port = 8932
    t = _serve_bg("ir", port)
    try:
        entry = int(_get(port, "/entry"))
        dot = _get(port, f"/dot?center={entry}&depth=2&raw=0")
        assert "#ffcc00" in dot   # center highlighted => a real neighborhood
    finally:
        _stop(port, t)


def test_shutdown_is_safe_when_nothing_is_running():
    """`shutdown` is the documented way to stop an explorer, so it has to be
    callable defensively — in a `finally`, twice over, or for a port that
    never served — without raising."""
    assert strider.explore.shutdown(9999) == []
    assert strider.explore.shutdown() == []


def test_no_explorer_survives_this_module():
    """Nothing may still be serving once this module's tests are done.

    The abort needed a live explorer at interpreter finalization, so
    asserting the registry is empty pins the defect directly: any test
    that forgets to shut its server down fails here, deterministically.
    """
    assert strider.explore.shutdown() == [], "an explorer server is still running"
