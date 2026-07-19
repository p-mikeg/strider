"""Explorer HTTP surface, plus the shutdown contract that keeps it from
crashing the interpreter.

Never let the interpreter exit with an explorer still serving: a thread
still inside `visualize` at shutdown aborts the whole process (SIGABRT),
after every test has already passed. Always call
`strider.explore.shutdown(port)` and join the thread. It only bites
about one run in seven, so it looks like a flake rather than a leak.

The join assertion in `_stop` IS the regression test: a leaked explorer
thread fails here deterministically instead.
"""

import json
import threading
import time
import urllib.request

import strider
import strider.explore


def _serve_bg(target_kind, port):
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

    # Non-daemon on purpose: a daemon thread lets the interpreter exit with
    # the server still running (the abort above). Non-daemon turns that leak
    # into a hang at exit, which is loud and debuggable rather than
    # intermittent and fatal.
    t = threading.Thread(target=run, daemon=False)
    t.start()
    return t


def _stop(port, thread):
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
        # The raw view only exists on `Function` (it is keyed by IR node id,
        # not CFG region index), so the explorer's "raw" toggle is a no-op on
        # a Cfg: both modes render the same pretty neighborhood.
        raw = _get(port, f"/dot?center={entry}&depth=1&raw=1")
        assert "#ffcc00" in pretty              # center highlighted
        assert raw == pretty
        # address search centers the containing block
        res = json.loads(_get(port, "/pattern?q=0x1000"))
        assert res.get("center") is not None or res.get("highlight") is not None
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
    """`shutdown` must be callable defensively (in a `finally`, twice over,
    or for a port that never served) without raising."""
    assert strider.explore.shutdown(9999) == []
    assert strider.explore.shutdown() == []


def test_no_explorer_survives_this_module():
    """Nothing may still be serving once this module's tests are done.

    A live explorer at interpreter exit is what aborts the process, so any
    test that forgets to shut its server down fails here instead.
    """
    assert strider.explore.shutdown() == [], "an explorer server is still running"
