"""Interactive IR explorer: a small local server plus a graphviz frontend.

`visualize(target)` serves a page showing the whole graph; `whole=False`
shows the neighborhood around one node instead, for large functions. Click a
node to re-center, click an edge to walk it, shift-click to mark. The search
bar runs a strider pattern and highlights the matches.

Drawn node ids are IR node ids. Per-use constant boxes (`c*`) and virtual
nodes (`v*`: if.true, if.false, Post Call) are not navigation targets.
"""

from __future__ import annotations

import ast
import http.server
import inspect
import io
import ipaddress
import json
import operator
import pathlib
import socket
import socketserver
import threading
import time
import urllib.parse
from typing import TYPE_CHECKING, Any, Protocol, cast

if TYPE_CHECKING:
    from collections.abc import Callable, Sequence

    from strider.cfg import Cfg
    from strider.ir import Function


def _pattern_names() -> list[str]:
    """What a query may name: the `strider.pattern` builders and `Capture`.
    `field` is excluded: its result is used through a method, which
    `_eval_pattern_node` forbids."""
    from strider import pattern as _p

    return sorted(
        k
        for k in dir(_p)
        if k == "Capture"
        or (
            not k.startswith("_")
            and k.islower()
            and k != "field"
            and callable(getattr(_p, k))
        )
    )


_CONST_TYPES = (bool, int, float, str, type(None))


def _eval_pattern_node(node: ast.expr, names: dict[str, Any]) -> Any:
    """Evaluate one node of a pattern expression: builder calls, literals
    and lists of them. No attribute access: from any literal,
    `().__class__.__base__.__subclasses__()` reaches `os.system`.
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
    """The sorted root node ids of `expr`'s matches in `function`, e.g. for
    `load(addr=int_add(initial_var(), int_const()))`."""
    from strider import pattern as _p

    names: dict[str, Any] = {k: getattr(_p, k) for k in _pattern_names()}
    pat = _eval_pattern_node(ast.parse(expr, mode="eval").body, names)
    return sorted({m.root for m in function.find_all(pat)})


#: Each render knob's label, tooltip and clamp range. Defaults come from the
#: renderer binding.
_KNOB_UI = {
    "depth": {
        "kind": "int",
        "label": "depth",
        "help": "how many hops out from the centered node to draw; 0 draws "
        "every hop",
        "min": 0,
        "max": 4096,
        "step": 1,
    },
    "hub_cap": {
        "kind": "int",
        "label": "hub cap",
        "help": "a node with more consumers than this is drawn but not "
        "expanded, so one popular value cannot flood the view; 0 expands "
        "every node",
        "min": 0,
        "max": 100000,
        "step": 2,
    },
    "max_nodes": {
        "kind": "int",
        "label": "max nodes",
        "help": "hard cap on how many nodes the view may draw; 0 is no cap",
        "min": 0,
        "max": 100000,
        "step": 10,
    },
    "whole": {
        "kind": "bool",
        "label": "whole",
        "help": "draw the entire graph instead of a neighborhood; the depth, "
        "hub cap and max-nodes knobs do not apply, and a large function can "
        "take the layout engine a long time",
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
    """Control descriptors for `names`, defaulting from `render`'s signature
    unless `literal_defaults` overrides."""
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
    """One query-string control value, clamped to its range. Raises
    `ValueError` on a non-number / non-flag."""
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
    """Every control's value for one `/dot` request, defaulted when
    omitted."""
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


#: What a 0 ("no limit") neighborhood knob becomes; the renderers only compare
#: against it.
_NO_LIMIT = 1 << 62


def _uncap(params: dict[str, Any]) -> dict[str, Any]:
    """`params` with each 0 neighborhood knob set to `_NO_LIMIT`."""
    return {
        k: (_NO_LIMIT if k in ("depth", "hub_cap", "max_nodes") and v == 0 else v)
        for k, v in params.items()
    }


class _IrVisualizer:
    """A `Function` as a `_Visualizer`."""

    def __init__(self, function: Function, whole: bool = True, depth: int = 0) -> None:
        self._fn = function
        self._whole = whole
        self._depth = depth

    def entry(self) -> int:
        """The node id to center the first view on."""
        return self._fn.entry_node()

    def controls(self) -> list[dict[str, Any]]:
        """The render knobs; the page opens uncapped and pretty."""
        return _controls(
            self._fn.neighborhood_dot,
            ["whole", "depth", "hub_cap", "max_nodes", "count_producers", "pretty"],
            pretty=True,
            whole=self._whole,
            depth=self._depth,
            hub_cap=0,
            max_nodes=0,
        )

    def dot(self, center: int, params: dict[str, Any]) -> str:
        """DOT around `center`, or of every node under `whole`."""
        pretty = params.get("pretty", True)
        if params.pop("whole", False):
            return cast("str", self._fn.to_dot(pretty=pretty))
        return self._fn.neighborhood_dot(center, **_uncap(params))

    def search(self, query: str) -> dict[str, Any]:
        """Node ids matching the pattern expression `query`."""
        return {"highlight": _run_pattern(self._fn, query)}

    def completions(self) -> list[str]:
        """Autocomplete candidates for the search bar."""
        return _pattern_names()


class _CfgVisualizer:
    """A `Cfg` as a `_Visualizer`."""

    def __init__(self, cfg: Cfg, whole: bool = True, depth: int = 0) -> None:
        """Explore `cfg`; its regions' p-code text is built once for search."""
        self._cfg = cfg
        self._whole = whole
        self._depth = depth
        self._texts: dict[int, str] = cfg._region_texts()

    def entry(self) -> int:
        """The region index to center the first view on."""
        return self._cfg.entry()

    def controls(self) -> list[dict[str, Any]]:
        """The render knobs. `pretty` and `hub_cap` are `Function`-only, so a
        query naming either is ignored."""
        return _controls(
            self._cfg.neighborhood_dot,
            ["whole", "depth", "max_nodes"],
            whole=self._whole,
            depth=self._depth,
            max_nodes=0,
        )

    def dot(self, center: int, params: dict[str, Any]) -> str:
        """DOT around `center`, or of every region under `whole`."""
        if params.pop("whole", False):
            return cast("str", self._cfg.to_dot())
        return self._cfg.neighborhood_dot(center, **_uncap(params))

    def search(self, query: str) -> dict[str, Any]:
        """Center the region containing `query` when it parses as an address,
        else highlight every region whose p-code text contains it (`IntAdd`,
        `RSP`), case-insensitively."""
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


#: How long a response write may make no progress before the connection is
#: dropped; progress re-arms it. A healthy slow reader can stall a send ~10 s.
_WRITE_STALL_SECONDS = 60.0

#: Longest one blocked `send` waits before re-checking `_Server.stopping`;
#: bounds `shutdown` with a body in flight.
_WRITE_POLL_SECONDS = 0.5

#: Total budget for one response: `_RESPONSE_START_SECONDS` plus the body at
#: `_MIN_BODY_RATE` bytes/s, so a trickling client cannot hold the
#: single-threaded loop forever.
_RESPONSE_START_SECONDS = 20.0
_MIN_BODY_RATE = 512 * 1024


def _is_loopback(host: str) -> bool:
    """Whether `host` is `localhost` or a loopback address, IPv4-mapped
    included."""
    if host == "localhost":
        return True
    try:
        addr = ipaddress.ip_address(host)
    except ValueError:
        return False
    return (getattr(addr, "ipv4_mapped", None) or addr).is_loopback


class _DeadlineReader(io.RawIOBase):
    """The raw socket layer under `rfile`, with a whole-request deadline:
    each `recv` gets only what is left of the budget, so a trickling client
    runs out. Below the buffer because `readline` loops `recv` in one call.
    """

    def __init__(self, inner: Any, conn: Any, budget: float) -> None:
        super().__init__()
        self._inner = inner
        self._conn = conn
        self._budget = budget
        self._deadline = time.monotonic() + budget

    def restart(self) -> None:
        """Restart the budget at the top of a request."""
        self._deadline = time.monotonic() + self._budget

    def readable(self) -> bool:
        return True

    def readinto(self, buffer: Any) -> int | None:
        left = self._deadline - time.monotonic()
        if left <= 0:
            raise TimeoutError("request exceeded the read deadline")
        self._conn.settimeout(left)
        return cast("int | None", self._inner.readinto(buffer))

    def close(self) -> None:
        try:
            self._inner.close()
        finally:
            super().close()


class _Handler(http.server.BaseHTTPRequestHandler):
    """The explorer's HTTP surface over `self.server`'s visualizer.

    Module level, not nested in `_serve`: a local class is a gc cycle and its
    closures would own the `Function` / `Cfg`, deferring their drop to
    whichever thread collects next.
    """

    #: The whole-request read budget. Without it, one silent connection wedges
    #: the single-threaded, non-daemon server and with it `shutdown` and exit.
    timeout = 2.0

    def _write_body(self, b: bytes) -> None:
        """Send `b` in `_WRITE_POLL_SECONDS` slices so the write stays
        interruptible; one `sendall` timeout would either truncate a slow
        client or park the loop. `deadline` (re-armed on progress) cuts an
        idle peer, `total` a trickling one, and `stopping` bounds `shutdown`.
        """
        stopping = cast("_Server", self.server).stopping
        view = memoryview(b)
        sent = 0
        now = time.monotonic()
        deadline = now + _WRITE_STALL_SECONDS
        total = now + _RESPONSE_START_SECONDS + len(view) / _MIN_BODY_RATE
        while sent < len(view):
            if stopping.is_set():
                raise TimeoutError("server is stopping")
            left = min(deadline, total) - time.monotonic()
            if left <= 0:
                raise TimeoutError("client stopped reading the response")
            self.connection.settimeout(min(_WRITE_POLL_SECONDS, left))
            try:
                n = self.connection.send(view[sent:])
            except (TimeoutError, socket.timeout):
                continue
            if n:
                sent += n
                deadline = time.monotonic() + _WRITE_STALL_SECONDS

    def _send(
        self, body: str | bytes, ctype: str = "text/html", code: int = 200
    ) -> None:
        b = body.encode() if isinstance(body, str) else body
        try:
            # For the headers' `sendall`; the read deadline may be near zero.
            self.connection.settimeout(_WRITE_POLL_SECONDS)
            self.send_response(code)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(b)))
            # Responses echo binary-derived text; never sniff it as HTML.
            self.send_header("X-Content-Type-Options", "nosniff")
            self.end_headers()
            self._write_body(b)
        except (BrokenPipeError, ConnectionError, TimeoutError, socket.timeout):
            # Cancelled or too slow to read: the connection is finished.
            self.close_connection = True
        finally:
            self.connection.settimeout(self.timeout)

    def _host_is_local(self, addr: Any) -> bool:
        """Whether `Host` names this server by a loopback address. An absent
        `Host` passes (browsers always send one), and so does any `Host` on a
        server bound to a non-loopback interface.
        """
        host = self.headers.get("Host")
        if host is None:
            return True
        parts: list[Any] = list(addr) if isinstance(addr, tuple) else []
        if not _is_loopback(str(parts[0]) if parts else ""):
            return True
        # A bracketed IPv6 literal's port follows `]`.
        name = host.strip()
        name = (
            name[1:].split("]", 1)[0]
            if name.startswith("[")
            else name.rsplit(":", 1)[0]
        )
        return _is_loopback(name)

    def do_GET(self) -> None:
        # No keep-alive: an idle connection would block the single thread.
        self.close_connection = True
        srv = cast("_Server", self.server)
        # DNS rebinding: a web page resolving its own name to loopback could
        # read the graph; browsers do not let a page forge `Host`.
        if not self._host_is_local(cast(Any, srv.server_address)):
            self._send("bad host", ctype="text/plain", code=421)
            return
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

    def setup(self) -> None:
        super().setup()
        # `timeout` alone is per `recv`, which a dripping client renews
        # forever; `_DeadlineReader` makes it one whole-request budget.
        budget = self.timeout if self.timeout is not None else 2.0
        raw = self.rfile
        if isinstance(raw, io.BufferedIOBase):
            raw = raw.detach()
        self._deadline_reader = _DeadlineReader(raw, self.connection, budget)
        self.rfile = cast(  # type: ignore[assignment]
            Any, io.BufferedReader(self._deadline_reader)
        )

    def handle_one_request(self) -> None:
        # Swallow the client-disconnect races the single-threaded loop hits.
        try:
            reader = getattr(self, "_deadline_reader", None)
            if reader is not None:
                reader.restart()
            super().handle_one_request()
        except (BrokenPipeError, ConnectionError):
            self.close_connection = True
        except (TimeoutError, socket.timeout):
            # Out of budget mid-request: the client is not going to finish.
            self.close_connection = True

    def log_message(self, format: str, *args: Any) -> None:  # noqa: A002 (base signature)
        pass


def _address_family(host: str) -> int:
    """`AF_INET6` for a literal IPv6 host, else `AF_INET`."""
    try:
        socket.inet_pton(socket.AF_INET6, host)
    except OSError:
        return socket.AF_INET
    return socket.AF_INET6


def _url(host: str, port: int) -> str:
    """The explorer URL, bracketing an IPv6 host as RFC 3986 requires."""
    return f"http://[{host}]:{port}/" if ":" in host else f"http://{host}:{port}/"


class _Server(socketserver.TCPServer):
    """The visualizer, entry node and render controls `_Handler` reads.
    Single-threaded.
    """

    allow_reuse_address = True

    def __init__(self, address: tuple[str, int], visualizer: _Visualizer) -> None:
        self.visualizer: _Visualizer | None = visualizer
        self.entry = visualizer.entry()
        self.controls = visualizer.controls()
        #: Set on entry to the serve loop; a racing `shutdown` waits for it.
        self.started = threading.Event()
        #: Set by `shutdown` first, so a handler blocked writing gives up.
        self.stopping = threading.Event()
        #: Set once the serving thread has left the loop and dropped the
        #: visualizer.
        self.finished = threading.Event()
        #: The background serving thread; None for a foreground `visualize`.
        self.worker: threading.Thread | None = None
        self.address_family = _address_family(address[0])
        super().__init__(address, _Handler)

    def serve_forever(self, poll_interval: float = 0.5) -> None:
        # Not from `service_actions`, which runs only after a request
        # completes.
        self.started.set()
        super().serve_forever(poll_interval)


#: Running explorer servers by bound port, for `shutdown` to reach.
_RUNNING: dict[int, _Server] = {}


#: Bound on joining a drained serve loop's thread.
_SHUTDOWN_JOIN_SECONDS = 5.0

#: How long `shutdown` waits for a stopped loop to drain; an in-flight render
#: cannot be preempted, so past this the loop exits on its own.
_SHUTDOWN_STOP_SECONDS = 5.0

#: How long `shutdown` waits for a registered server's loop to start.
_SHUTDOWN_START_SECONDS = 5.0


def shutdown(port: int | None = None) -> list[int]:
    """Stop the explorer on `port`, or every one when `None`, and join its
    serving thread (finalizing with it inside Rust aborts the process).
    Returns the ports stopped; `[]` when nothing runs. Callable from any
    thread. Waits are bounded: a port mid-render is not reported, and its
    loop exits once the render returns. Runs at interpreter exit.
    """
    # A snapshot: a serving thread pops its own entry as it exits.
    running = list(_RUNNING.items())
    targets = running if port is None else [(p, s) for p, s in running if p == port]
    current = threading.current_thread()
    stopped = []
    # Flag every server before waiting on any.
    for _p, srv in targets:
        srv.stopping.set()
    for p, srv in targets:
        if not srv.started.wait(_SHUTDOWN_START_SECONDS):
            # Not stopped: a set flag would fail a later `_write_body`.
            srv.stopping.clear()
            continue
        # `BaseServer.shutdown`'s stop half, without its unbounded wait.
        srv._BaseServer__shutdown_request = True  # type: ignore[attr-defined]
        if srv.finished.wait(_SHUTDOWN_STOP_SECONDS):
            stopped.append(p)
    for p, srv in targets:
        # Skip a loop that never ran, our own thread, and a foreground server.
        worker = srv.worker
        if p not in stopped or worker is None or worker is current:
            continue
        if worker.is_alive():
            worker.join(timeout=_SHUTDOWN_JOIN_SECONDS)
    return stopped


# Not `atexit`: finalization joins non-daemon threads before `atexit` runs.
threading._register_atexit(shutdown)  # pyright: ignore[reportAttributeAccessIssue]


def visualize(
    target: Function | Cfg,
    *,
    host: str = "127.0.0.1",
    port: int = 0,
    depth: int | None = None,
    whole: bool = True,
    background: bool = False,
) -> int:
    """Serve the explorer for `target`, a `Function` or a `Cfg`, printing
    its URL. Blocks until interrupted, then returns the bound port.

    Opens on the whole graph (slow to lay out at a few thousand nodes);
    `whole=False` opens on a neighborhood around the entry. The neighborhood
    knobs open uncapped (0 is no limit); an integer `depth` seeds the depth
    control. A negative or non-integer `depth` (`bool`, `float`) raises
    `ValueError`.

    `background=True` serves on a thread and returns the port at once; stop
    it with `shutdown(port)`. Rendering does not block analysis on the
    calling thread."""
    if depth is not None:
        try:
            # `operator.index` accepts `bool`.
            if isinstance(depth, bool):
                raise TypeError
            depth = operator.index(depth)
        except TypeError:
            raise ValueError(f"depth: {str(depth)!r} is not an integer") from None
        if depth < 0:
            raise ValueError(f"depth must be >= 0, got {depth}")
    seed = min(depth or 0, cast("int", _KNOB_UI["depth"]["max"]))
    tn = type(target).__name__
    if tn == "Function":
        vis: _Visualizer = _IrVisualizer(cast("Function", target), whole, seed)
    elif tn == "Cfg":
        vis = _CfgVisualizer(cast("Cfg", target), whole, seed)
    else:
        raise TypeError(f"visualize expects a Function or Cfg, got {tn}")
    return _serve(vis, host=host, port=port, background=background)


def _serve(
    visualizer: _Visualizer,
    *,
    host: str = "127.0.0.1",
    port: int = 0,
    background: bool = False,
) -> int:
    """Serve `visualizer`, printing the URL, and return the bound port.
    Blocks unless `background`, which serves on a non-daemon thread (a daemon
    one would abort inside Rust at exit). Binds on this thread, so a port in
    use raises here."""
    srv = _Server((host, port), visualizer)
    bound_port: int = srv.server_address[1]

    def run() -> None:
        try:
            srv.serve_forever()
        except KeyboardInterrupt:
            srv.shutdown()
        finally:
            _RUNNING.pop(bound_port, None)
            srv.server_close()
            # `shutdown` may still hold `srv`; drop the `Function` / `Cfg` now.
            srv.visualizer = None
            srv.finished.set()

    # Before the loop starts, so a racing `shutdown()` finds it.
    _RUNNING[bound_port] = srv
    if background:
        srv.worker = threading.Thread(
            target=run, name=f"strider-explorer-{bound_port}", daemon=False
        )
        try:
            srv.worker.start()
        except BaseException:
            _RUNNING.pop(bound_port, None)
            srv.server_close()
            raise
    how = f"strider.explore.shutdown({bound_port})" if background else "Ctrl-C"
    try:
        print(f"strider explorer -> {_url(host, bound_port)}  ({how} to stop)")
        print("  drag or arrows pan, ctrl+wheel or +/- zooms, f fits, 0 is 100%")
    except BaseException:
        # A foreground server never reaches the loop that would drain it.
        if not background:
            _RUNNING.pop(bound_port, None)
            srv.server_close()
        raise
    if not background:
        run()
    return bound_port
