"""Interactive IR explorer: a small local server plus a graphviz frontend.

`Lifter.visualize(target)` serves a page showing the neighborhood (inputs and
outputs) around the entry node. Click a node to re-center on it, click an edge
to walk along it, shift-click to mark it. The search bar runs a strider
pattern, with autocomplete, and highlights the matching nodes.

The graph is rendered a neighborhood at a time, never whole. Drawn node ids
are IR node ids, so pattern matches line up one to one with what you see.
Per-use constant boxes (`c*`) and virtual nodes (`v*`: if.true, if.false,
Post Call) are not navigation targets.

The render controls in the toolbar are built from `/controls`, whose defaults
are read out of the renderer binding's own signature, so a default changed in
Rust reaches the page with no edit here.
"""

from __future__ import annotations

import ast
import atexit
import http.server
import inspect
import json
import pathlib
import socketserver
import threading
import urllib.parse
from typing import TYPE_CHECKING, Any, Protocol, cast

if TYPE_CHECKING:
    from collections.abc import Callable, Sequence

    from strider.cfg import Cfg
    from strider.ir import Function


def _pattern_names() -> list[str]:
    """The `strider.pattern` builder names, for the search-bar autocomplete."""
    from strider import pattern as _p

    return sorted(k for k in dir(_p) if not k.startswith("_"))


_CONST_TYPES = (bool, int, float, str, type(None))


def _eval_pattern_node(node: ast.expr, names: dict[str, Any]) -> Any:
    """Evaluate one node of a pattern expression, rejecting anything that
    could reach an attribute.

    Builder calls on `strider.pattern` names, literals and their lists are the
    whole language. `eval` with stripped builtins is not a sandbox:
    `().__class__.__base__.__subclasses__()` walks from any literal to
    `os.system`.
    """
    if isinstance(node, ast.Constant):
        if isinstance(node.value, _CONST_TYPES):
            return node.value
        raise ValueError(f"{type(node.value).__name__} literals are not allowed")
    if isinstance(node, ast.Name):
        if node.id not in names or node.id.startswith("_"):
            raise ValueError(f"unknown pattern builder: {node.id!r}")
        return names[node.id]
    if isinstance(node, ast.UnaryOp) and isinstance(node.op, ast.USub):
        operand = node.operand
        if isinstance(operand, ast.Constant) and isinstance(operand.value, (int, float)):
            return -operand.value
        raise ValueError("unary minus applies to a numeric literal only")
    if isinstance(node, (ast.List, ast.Tuple)):
        elts = [_eval_pattern_node(e, names) for e in node.elts]
        return elts if isinstance(node, ast.List) else tuple(elts)
    if isinstance(node, ast.Call):
        if not isinstance(node.func, ast.Name):
            raise ValueError("only a plain builder name is callable")
        func = _eval_pattern_node(node.func, names)
        args = [_eval_pattern_node(a, names) for a in node.args]
        kwargs: dict[str, Any] = {}
        for kw in node.keywords:
            if kw.arg is None:
                raise ValueError("** unpacking is not allowed")
            if kw.arg.startswith("_"):
                raise ValueError(f"keyword {kw.arg!r} is not allowed")
            kwargs[kw.arg] = _eval_pattern_node(kw.value, names)
        return func(*args, **kwargs)
    raise ValueError(f"{type(node).__name__} is not allowed in a pattern expression")


def _run_pattern(function: Function, expr: str) -> list[int]:
    """Evaluate the `strider.pattern` expression `expr` against `function`,
    returning the node ids of the deduplicated matches, e.g.
    `load(addr=int_add(initial_var(), int_const()))`."""
    from strider import pattern as _p

    names: dict[str, Any] = {
        k: getattr(_p, k) for k in dir(_p) if not k.startswith("_")
    }
    pat = _eval_pattern_node(ast.parse(expr, mode="eval").body, names)
    return sorted({m.root for m in function.find_all(pat)})


#: Presentation for each render knob: label, tooltip and the range the UI and
#: the server clamp to. Defaults live in the renderer binding, never here.
_KNOB_UI = {
    "depth": {
        "kind": "int",
        "label": "depth",
        "help": "how many hops out from the centered node to draw",
        "min": 1,
        "max": 20,
        "step": 1,
    },
    "hub_cap": {
        "kind": "int",
        "label": "hub cap",
        "help": "a node with more consumers than this is drawn but not "
        "expanded, so one popular value cannot flood the view",
        "min": 1,
        "max": 512,
        "step": 2,
    },
    "max_nodes": {
        "kind": "int",
        "label": "max nodes",
        "help": "hard cap on how many nodes the view may draw",
        "min": 1,
        "max": 2000,
        "step": 10,
    },
    "count_producers": {
        "kind": "bool",
        "label": "+prod",
        "help": "count a node's inputs toward the hub cap too, not just its "
        "consumer fan-out",
    },
    "pretty": {
        "kind": "bool",
        "label": "pretty",
        "help": "inline constants, add virtual nodes and resolve register "
        "names; off draws the graph exactly as stored",
    },
}


def _controls(
    render: Callable[..., str],
    names: Sequence[str],
    **literal_defaults: bool | int,
) -> list[dict[str, Any]]:
    """Control descriptors for `names`, each default read from `render`'s own
    signature unless `literal_defaults` states one, which is how the page
    opens on a different setting from the binding's own default."""
    params = inspect.signature(render).parameters
    return [
        {
            **_KNOB_UI[n],
            "name": n,
            "default": literal_defaults[n]
            if n in literal_defaults
            else params[n].default,
        }
        for n in names
    ]


def _parse_control(ctl: dict[str, Any], text: str) -> bool | int:
    """One query-string control value, clamped to the control's range. Raises
    `ValueError` on a value that is not a number / flag at all."""
    if ctl["kind"] == "bool":
        low = text.lower()
        if low in ("1", "true", "yes", "on"):
            return True
        if low in ("0", "false", "no", "off"):
            return False
        raise ValueError(f"{ctl['name']}: {text!r} is not a flag")
    try:
        n = int(text, 0)
    except ValueError:
        raise ValueError(f"{ctl['name']}: {text!r} is not an integer") from None
    return max(ctl["min"], min(ctl["max"], n))


def _dot_params(
    query: dict[str, list[str]], controls: list[dict[str, Any]]
) -> dict[str, Any]:
    """The renderer knobs for one `/dot` request: every declared control,
    falling back to the control's default when the query omits it."""
    out: dict[str, Any] = {}
    for ctl in controls:
        text = (query.get(ctl["name"]) or [""])[0]
        out[ctl["name"]] = ctl["default"] if text == "" else _parse_control(ctl, text)
    return out


_FRONTEND = (pathlib.Path(__file__).parent / "explore.html").read_text(
    encoding="utf-8"
)


class _Visualizer(Protocol):
    """The visualizer shape `_serve` drives."""

    def entry(self) -> int: ...
    def controls(self) -> list[dict[str, Any]]: ...
    def dot(self, center: int, params: dict[str, Any]) -> str: ...
    def search(self, query: str) -> dict[str, Any]: ...
    def completions(self) -> list[str]: ...


class _IrVisualizer:
    """Adapts a `Function` to what `_serve` expects: `entry()`, `controls()`,
    `dot(center, params)`, `search(query)`, `completions()`."""

    def __init__(self, function: Function) -> None:
        """Explore `function`."""
        self._fn = function

    def entry(self) -> int:
        """The node id to center the first view on."""
        return self._fn.entry_node()

    def controls(self) -> list[dict[str, Any]]:
        """The render knobs, defaults taken from the renderer binding. The
        page opens on the readable view, so `pretty` starts on."""
        return _controls(
            self._fn.neighborhood_dot,
            ["depth", "hub_cap", "max_nodes", "count_producers", "pretty"],
            pretty=True,
        )

    def dot(self, center: int, params: dict[str, Any]) -> str:
        """DOT for the neighborhood around `center`, `params` holding one
        value per declared control. `pretty=False` falls back to the
        structure-faithful view for when the readable one cannot be trusted."""
        return self._fn.neighborhood_dot(center, **params)

    def search(self, query: str) -> dict[str, Any]:
        """Node ids matching the pattern expression `query`."""
        return {"highlight": _run_pattern(self._fn, query)}

    def completions(self) -> list[str]:
        """Autocomplete candidates for the search bar."""
        return _pattern_names()


class _CfgVisualizer:
    """Adapts a `Cfg` to what `_serve` expects: `entry()`, `controls()`,
    `dot(center, params)`, `search(query)`, `completions()`."""

    def __init__(self, cfg: Cfg) -> None:
        """Explore `cfg`, building its per-region disassembly text once for
        reuse by every text search."""
        self._cfg = cfg
        self._texts: dict[int, str] = cfg._region_texts()

    def entry(self) -> int:
        """The region index to center the first view on."""
        return self._cfg.entry()

    def controls(self) -> list[dict[str, Any]]:
        """The render knobs, defaults taken from the renderer binding. The raw
        view and the hub cap are `Function` concepts (the raw view is keyed by
        IR node id), so a query naming either knob is ignored."""
        return _controls(self._cfg.neighborhood_dot, ["depth", "max_nodes"])

    def dot(self, center: int, params: dict[str, Any]) -> str:
        """DOT for the regions around `center`, `params` holding one value per
        declared control."""
        return self._cfg.neighborhood_dot(center, **params)

    def search(self, query: str) -> dict[str, Any]:
        """Center the region containing `query` when it parses as an address,
        else highlight every region whose disassembly contains it."""
        q = query.strip()
        try:
            addr = int(q, 0)
            blk = self._cfg.region_at(addr)
            return {"center": blk} if blk is not None else {"highlight": []}
        except ValueError:
            ql = q.lower()
            hits = sorted(rid for rid, txt in self._texts.items() if ql in txt.lower())
            return {"highlight": hits}

    def completions(self) -> list[str]:
        """Empty: CFG search takes free text, not a pattern expression."""
        return []


class _Handler(http.server.BaseHTTPRequestHandler):
    """The explorer's HTTP surface, served from `self.server`'s visualizer.

    Module level, not nested in `_serve`: a locally defined class is a gc
    cycle through its own `__mro__`, and a nested one's method closures would
    own the `unsendable` `Function` / `Cfg`, deferring their drop to whatever
    thread happens to collect next.
    """

    def _send(
        self, body: str | bytes, ctype: str = "text/html", code: int = 200
    ) -> None:
        b = body.encode() if isinstance(body, str) else body
        try:
            self.send_response(code)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(b)))
            self.end_headers()
            self.wfile.write(b)
        except (BrokenPipeError, ConnectionError):
            pass  # client cancelled the request (e.g. clicked again)

    def do_GET(self) -> None:
        # Close after each response. This server is single-threaded, so it
        # must never sit blocked in a keep-alive read on an idle connection:
        # that would stall every other request until the idle connection
        # times out.
        self.close_connection = True
        srv = cast("_Server", self.server)
        visualizer = cast("_Visualizer", srv.visualizer)
        u = urllib.parse.urlparse(self.path)
        q = urllib.parse.parse_qs(u.query)
        try:
            if u.path == "/":
                self._send(_FRONTEND)
            elif u.path == "/viz.js":
                import strider._strider as _ext

                self._send(_ext._viz_standalone_js(), "application/javascript")
            elif u.path == "/entry":
                self._send(json.dumps(srv.entry), "application/json")
            elif u.path == "/controls":
                self._send(json.dumps(srv.controls), "application/json")
            elif u.path == "/patterns":
                self._send(json.dumps(visualizer.completions()), "application/json")
            elif u.path == "/dot":
                c = int(q.get("center", [srv.entry])[0])
                self._send(
                    visualizer.dot(c, _dot_params(q, srv.controls)), "text/plain"
                )
            elif u.path == "/pattern":
                result = visualizer.search(q.get("q", [""])[0])
                self._send(json.dumps(result), "application/json")
            else:
                self.send_error(404)
        except (BrokenPipeError, ConnectionError):
            pass  # client went away mid-request
        except Exception as e:  # noqa: BLE001 (surface the error to the UI)
            self._send(f"{type(e).__name__}: {e}", "text/plain", code=400)

    def handle_one_request(self) -> None:
        # Swallow the client-disconnect races the single-threaded loop hits.
        try:
            super().handle_one_request()
        except (BrokenPipeError, ConnectionError):
            self.close_connection = True

    def log_message(self, format: str, *args: Any) -> None:  # noqa: A002 (base signature)
        pass


class _Server(socketserver.TCPServer):
    """Carries the visualizer, its entry node and its render controls for
    `_Handler` to read.

    Single-threaded: a `Function` may only be touched from the thread that
    created it, which is the caller's thread blocked in `serve_forever`.
    """

    allow_reuse_address = True

    def __init__(
        self,
        address: tuple[str, int],
        visualizer: _Visualizer,
        depth: int | None = None,
    ) -> None:
        self.visualizer: _Visualizer | None = visualizer
        self.entry = visualizer.entry()
        self.controls = visualizer.controls()
        #: Set from inside the serve loop. `BaseServer.shutdown` blocks
        #: forever unless `serve_forever` is already running, so a `shutdown`
        #: racing the start must wait for this.
        self.started = threading.Event()
        if depth is not None:
            for ctl in self.controls:
                if ctl["name"] == "depth":
                    ctl["default"] = _parse_control(ctl, str(depth))
        super().__init__(address, _Handler)

    def service_actions(self) -> None:
        # Called once per poll iteration, on the serving thread, with the loop
        # already entered.
        self.started.set()


#: Explorer servers currently serving, with the thread parked in each, keyed by
#: the port they bound. `visualize` blocks, so a caller running it on another
#: thread holds no handle on the server; this registry is how `shutdown` reaches
#: it, and the thread is what `shutdown` joins.
_RUNNING: dict[int, tuple[_Server, threading.Thread]] = {}


#: Bound so a wedged handler cannot hang interpreter exit; the thread is out of
#: the Rust frame long before this, and exceeding it only risks the abort that
#: was the status quo.
_SHUTDOWN_JOIN_SECONDS = 5.0

#: How long `shutdown` waits for a server registered but not yet inside
#: `serve_forever`. Exceeding it means the serve loop never started, so there
#: is nothing to stop.
_SHUTDOWN_START_SECONDS = 5.0


def shutdown(port: int | None = None) -> list[int]:
    """Stop explorer servers started by `visualize`, unblocking whatever
    thread is parked in it. Returns the ports actually stopped.

    `port=None` stops every running explorer. Safe to call when nothing is
    running (returns `[]`), and safe to call from a thread other than the one
    serving.

    Joins the serving thread as well as stopping the server. Stopping alone is
    not enough: `_Server.shutdown` returns as soon as the serve loop exits,
    while the thread is still unwinding out of the Rust frame, and an
    interpreter that finalizes with a thread in that state aborts the process.

    Registered with `threading._register_atexit`, NOT `atexit`: CPython's
    `Py_FinalizeEx` joins non-daemon threads BEFORE running `atexit` handlers,
    and a thread parked in `serve_forever` is exactly such a thread, so an
    `atexit` hook would never get to run and the process would hang on the join
    instead.
    """
    targets = list(_RUNNING.items()) if port is None else [
        (p, s) for p, s in _RUNNING.items() if p == port
    ]
    current = threading.current_thread()
    for _p, (srv, _thread) in targets:
        # `BaseServer.shutdown` waits on an event `serve_forever` clears on
        # entry and sets on exit, so calling it before the loop starts blocks
        # forever.
        if srv.started.wait(_SHUTDOWN_START_SECONDS):
            srv.shutdown()  # returns once the serve loop has exited
    for _p, (_srv, thread) in targets:
        # Joining the thread serving us would deadlock; that caller is already
        # past the frame this exists to drain.
        if thread is not current and thread.is_alive():
            thread.join(timeout=_SHUTDOWN_JOIN_SECONDS)
    return [p for p, _s in targets]


# Runs BEFORE the non-daemon-thread join, which is the only point at which a
# parked serve loop can still be stopped; `atexit` is too late (see `shutdown`).
# Falls back to `atexit` on an interpreter without the private hook, where an
# explorer left running is no worse off than before.
if hasattr(threading, "_register_atexit"):
    threading._register_atexit(shutdown)  # pyright: ignore[reportAttributeAccessIssue]
else:  # pragma: no cover - CPython has had this since 3.9
    atexit.register(shutdown)


def visualize(
    target: Function | Cfg,
    *,
    host: str = "127.0.0.1",
    port: int = 0,
    depth: int | None = None,
) -> None:
    """Start the explorer for `target`, a `Function` from `analyze` or a
    `Cfg` from `build_cfg` / `analyze`. Blocks serving requests until
    interrupted.

    `depth=None` starts at the renderer's own default depth; a number seeds
    the toolbar's depth control instead. Every other render knob starts at the
    renderer default and is set from the page.

    Runs on the thread that created `target`: it reads the unsendable
    `Function` / `Cfg` at once, and any other thread raises
    `PanicException: unsendable`. `shutdown(port)` is called from another
    thread to unblock it, and the serving thread must be joined before the
    interpreter exits: a thread still parked here at interpreter shutdown
    aborts the process."""
    tn = type(target).__name__
    if tn == "Function":
        vis: _Visualizer = _IrVisualizer(cast("Function", target))
    elif tn == "Cfg":
        vis = _CfgVisualizer(cast("Cfg", target))
    else:
        raise TypeError(f"visualize expects a Function or Cfg, got {tn}")
    return _serve(vis, host=host, port=port, depth=depth)


def _serve(
    visualizer: _Visualizer,
    *,
    host: str = "127.0.0.1",
    port: int = 0,
    depth: int | None = None,
) -> None:
    """Serve the explorer over any visualizer-shaped object. Prints the URL
    to stdout for the caller to open. Blocks serving requests until
    interrupted."""
    srv = _Server((host, port), visualizer, depth)
    bound_port: int = srv.server_address[1]
    url = f"http://{host}:{bound_port}/"
    print(f"strider explorer -> {url}  (Ctrl-C to stop)")
    print("  renders the neighborhood around a node you pick, never the whole graph")
    _RUNNING[bound_port] = (srv, threading.current_thread())
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        srv.shutdown()
    finally:
        _RUNNING.pop(bound_port, None)
        srv.server_close()
        # `shutdown` runs on another thread and outlives this frame holding
        # `srv`, so drop the unsendable `Function` / `Cfg` here, on the thread
        # that created them.
        srv.visualizer = None
