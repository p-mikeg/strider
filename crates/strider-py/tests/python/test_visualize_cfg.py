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

import gc
import inspect
import json
import re
import threading
import time
import urllib.error
import urllib.request

import strider
import strider.explore

#: add rax,rbx / add rax,rcx / add rax,rdx / sub rax,rbx / add rax,rbx /
#: test rax,rax / je out / imul rax,rbx / jmp back / ret. Big enough that the
#: render controls visibly bite: several nodes with fan-in above 1 and a loop.
_BUSY = bytes.fromhex("4801d84801c84801d04829d84801d84885c07405480fafc3ebf5c3")


def _serve_bg(target_kind, port, code=b"\x75\x01\x90\xc3", **kw):
    def run():
        lift = strider.lift.lifter(
            strider.sleigh.SleighArch.x86_64(),
            strider.reader.BufferReader(0x1000, code),
        )
        cfg = lift.build_cfg(0x1000)
        if target_kind == "cfg":
            target = cfg
        else:
            target = lift.analyze(
                0x1000, strider.sleigh.CallingConvention.x86_64_systemv()
            ).function
        lift.visualize(target, port=port, **kw)

    # Non-daemon: a daemon thread lets the interpreter exit with the server
    # still running (the abort above), while a non-daemon one turns that leak
    # into a hang at exit, which is loud and debuggable.
    t = threading.Thread(target=run, daemon=False)
    t.start()
    return t


def _stop(port, thread):
    assert strider.explore.shutdown(port) == [port], f"no explorer on port {port}"
    thread.join(timeout=10)
    assert not thread.is_alive(), (
        "explorer thread survived shutdown(); a thread left parked inside "
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


def _status(port, path):
    """`(code, body)` for a request the server is expected to refuse."""
    try:
        r = urllib.request.urlopen(f"http://127.0.0.1:{port}{path}", timeout=8)
        return r.status, r.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()


#: DOT node declarations (`"12" [label=...`), not the `"a" -> "b" [label=...`
#: edges that carry labels too.
_NODE_LINE = re.compile(r'^\s*"[^"]+" \[label=', re.M)


def _nodes(dot):
    return len(_NODE_LINE.findall(dot))


def _controls(port):
    return {c["name"]: c for c in json.loads(_get(port, "/controls"))}


def _query(controls, **over):
    """The full control query string, taking each advertised default unless
    overridden."""
    vals = {n: over.get(n, c["default"]) for n, c in controls.items()}
    return "".join(
        f"&{n}={int(v) if isinstance(v, bool) else v}" for n, v in vals.items()
    )


def test_ir_controls_report_the_renderer_defaults():
    """Every knob the renderer takes is offered, with the renderer's own
    default. A number restated here instead would drift the moment the Rust
    signature changed."""
    port = 8934
    t = _serve_bg("ir", port)
    try:
        ctrls = _controls(port)
        assert list(ctrls) == [
            "depth",
            "hub_cap",
            "max_nodes",
            "count_producers",
            "pretty",
        ]
        sig = inspect.signature(strider.ir.Function.neighborhood_dot).parameters
        for name in ("depth", "hub_cap", "max_nodes", "count_producers"):
            assert ctrls[name]["default"] == sig[name].default, name
        # The page opens on the readable view; the binding opens on the raw one.
        assert ctrls["pretty"]["default"] is True
        # usable without the source
        for c in ctrls.values():
            assert c["label"] and c["help"]
        assert "not expanded" in ctrls["hub_cap"]["help"]
    finally:
        _stop(port, t)


def test_cfg_controls_are_the_ones_the_cfg_renderer_takes():
    port = 8935
    t = _serve_bg("cfg", port)
    try:
        ctrls = _controls(port)
        assert list(ctrls) == ["depth", "max_nodes"]
        sig = inspect.signature(strider.cfg.Cfg.neighborhood_dot).parameters
        for name in ctrls:
            assert ctrls[name]["default"] == sig[name].default, name
    finally:
        _stop(port, t)


def test_visualize_depth_seeds_the_depth_control():
    """`visualize(depth=)` moves the control's default, so the page starts
    there and an absent `depth` still means the same thing."""
    port = 8939
    t = _serve_bg("ir", port, code=_BUSY, depth=2)
    try:
        entry = int(_get(port, "/entry"))
        assert _controls(port)["depth"]["default"] == 2
        assert _get(port, f"/dot?center={entry}") == _get(
            port, f"/dot?center={entry}&depth=2"
        )
    finally:
        _stop(port, t)


def test_absent_control_falls_back_to_the_strider_default():
    port = 8936
    t = _serve_bg("ir", port, code=_BUSY)
    try:
        entry = int(_get(port, "/entry"))
        bare = _get(port, f"/dot?center={entry}")
        spelled = _get(port, f"/dot?center={entry}{_query(_controls(port))}")
        assert bare == spelled
    finally:
        _stop(port, t)


def test_every_control_changes_the_rendered_dot():
    port = 8937
    t = _serve_bg("ir", port, code=_BUSY)
    try:
        entry = int(_get(port, "/entry"))
        ctrls = _controls(port)

        def dot(**over):
            return _get(port, f"/dot?center={entry}{_query(ctrls, **over)}")

        assert _nodes(dot(depth=1)) < _nodes(dot(depth=8, max_nodes=2000))
        assert _nodes(dot(max_nodes=3)) < _nodes(dot(max_nodes=2000, depth=8))
        # a hub is drawn but not expanded, so a tight cap keeps nodes out
        wide = dot(hub_cap=500, depth=4, max_nodes=2000)
        assert _nodes(dot(hub_cap=1, depth=4, max_nodes=2000)) < _nodes(wide)
        # folding producers into the hub degree makes more nodes count as hubs
        tight = dict(hub_cap=2, depth=4, max_nodes=2000)
        assert _nodes(dot(count_producers=True, **tight)) < _nodes(dot(**tight))
        assert dot(pretty=True) != dot(pretty=False)
    finally:
        _stop(port, t)


def test_nonsense_control_values_are_clamped_or_refused():
    """A bad query must not reach the renderer as a 500."""
    port = 8938
    t = _serve_bg("ir", port, code=_BUSY)
    try:
        entry = int(_get(port, "/entry"))
        ctrls = _controls(port)
        top = ctrls["depth"]["max"]
        assert _get(port, f"/dot?center={entry}&depth=99999") == _get(
            port, f"/dot?center={entry}&depth={top}"
        )
        assert _get(port, f"/dot?center={entry}&max_nodes=-4") == _get(
            port, f"/dot?center={entry}&max_nodes={ctrls['max_nodes']['min']}"
        )
        for bad, expect in (
            ("depth=abc", 400),
            ("max_nodes=1e9", 400),
            ("pretty=maybe", 400),
            ("hub_cap=", 200),  # an empty value reads as absent
        ):
            code, body = _status(port, f"/dot?center={entry}&{bad}")
            assert code == expect, (bad, code, body)
            if code == 400:
                assert bad.split("=")[0] in body, body
        assert _status(port, "/dot?center=nope")[0] == 400
    finally:
        _stop(port, t)


def test_visualize_cfg_serves_neighborhood_and_search():
    port = 8931
    t = _serve_bg("cfg", port)
    try:
        entry = int(_get(port, "/entry"))
        dot = _get(port, f"/dot?center={entry}&depth=1")
        # `pretty` is a Function control keyed by IR node id; a Cfg declares
        # none, so the query param is ignored rather than changing the view.
        assert _get(port, f"/dot?center={entry}&depth=1&pretty=0") == dot
        assert "#ffcc00" in dot                 # center highlighted
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
        dot = _get(port, f"/dot?center={entry}&depth=2&pretty=1")
        assert "#ffcc00" in dot   # center highlighted => a real neighborhood
    finally:
        _stop(port, t)


def test_explorer_objects_die_on_their_creating_thread():
    """A `Cfg` / `Function` built inside the explorer thread must be freed
    there, by refcount, the moment the thread's frames go.

    Anything the server still holds is instead cyclic garbage collected later
    on whatever thread runs the next `gc.collect()`, and pyo3 refuses to drop
    an unsendable pyclass off its own thread: it leaks the whole IR arena and
    writes an unraisable exception.
    """
    before = {id(o) for o in gc.get_objects()}
    port = 8933
    t = _serve_bg("ir", port)
    try:
        _get(port, "/entry")
    finally:
        _stop(port, t)
    # No collect: a leak here is *uncollected* garbage, so it is still listed.
    leaked = [
        type(o).__name__
        for o in gc.get_objects()
        if id(o) not in before
        and type(o).__module__.startswith("strider")
        and type(o).__name__ in ("Cfg", "Function")
    ]
    assert not leaked, f"outlived the explorer thread: {leaked}"


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
