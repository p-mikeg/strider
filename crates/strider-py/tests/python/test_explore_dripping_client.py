"""A client trickling bytes must not park the single-threaded serve loop.

`_Handler.timeout` is a per-`recv` deadline, so on its own it only ever fires
on a connection that says nothing at all. `BufferedReader.readline` loops
`recv` inside ONE call, so a deadline armed per `readline` handed each of those
recvs a full fresh window and a client sending one byte every 0.4 s renewed it
forever: no later request was served, `shutdown` could not reach a loop inside
`finish_request`, and the non-daemon serving thread never let the interpreter
exit either.

The drip here never ends its request line, so nothing but the read deadline can
close it. Run in subprocesses: two of the three failures are wall-clock hangs,
which no in-process assertion can observe.
"""

from __future__ import annotations

import subprocess
import sys
import time

_SERVER = """
import socket, threading, time
from strider import explore

class V:
    def entry(self): return 0
    def controls(self): return [{"name": "depth", "kind": "int", "default": 5}]
    def dot(self, center, params): return "digraph {}"
    def search(self, query): return {"highlight": []}
    def completions(self): return []

port = explore._serve_background(V(), host="127.0.0.1", port=0, depth=None)

def drip():
    s = socket.socket()
    s.connect(("127.0.0.1", port))
    try:
        s.sendall(b"GET / HTTP/1.1\\r\\nHost: 127.0.0.1\\r\\nX-Pad: ")
        while True:
            s.sendall(b"a")
            time.sleep(0.4)
    except OSError:
        pass

threading.Thread(target=drip, daemon=True).start()
time.sleep(1.0)
"""

#: One request a healthy explorer answers in milliseconds, given a whole
#: `_Handler.timeout` window of its own plus slack.
_LEGIT_TIMEOUT = 8.0

STARVATION = (
    _SERVER
    + f"""
import urllib.request
try:
    body = urllib.request.urlopen(
        f"http://127.0.0.1:{{port}}/entry", timeout={_LEGIT_TIMEOUT}
    ).read()
    print("SERVED", body.decode(), flush=True)
except Exception as e:
    print("STARVED", type(e).__name__, flush=True)
explore.shutdown(port)
"""
)

SHUTDOWN_WHILE_DRIPPING = (
    _SERVER
    + """
print("SHUTDOWN", explore.shutdown(port), flush=True)
"""
)

EXIT_WHILE_DRIPPING = (
    _SERVER
    + """
print("RETURNING", flush=True)
"""
)

#: Well over the ~3 s a healthy run takes; a regression is an unbounded hang.
_BUDGET = 45.0


def _run(script: str) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            [sys.executable, "-c", script],
            capture_output=True,
            text=True,
            timeout=_BUDGET,
        )
    except subprocess.TimeoutExpired:
        raise AssertionError(
            f"a live drip wedged the explorer for the whole {_BUDGET}s budget"
        ) from None


def test_a_drip_does_not_starve_a_real_request():
    r = _run(STARVATION)
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    assert "SERVED" in r.stdout, f"a live drip starved a real request: {r.stdout}"


def test_shutdown_returns_while_a_drip_is_live():
    t0 = time.monotonic()
    r = _run(SHUTDOWN_WHILE_DRIPPING)
    elapsed = time.monotonic() - t0
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    line = next(ln for ln in r.stdout.splitlines() if ln.startswith("SHUTDOWN"))
    assert line.removeprefix("SHUTDOWN ") != "[]", "shutdown stopped nothing"
    assert elapsed < _BUDGET / 2, f"shutdown took {elapsed:.1f}s"


def test_the_interpreter_exits_while_a_drip_is_live():
    """No `shutdown()` call at all: the atexit hook has to reach the loop."""
    t0 = time.monotonic()
    r = _run(EXIT_WHILE_DRIPPING)
    elapsed = time.monotonic() - t0
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    assert "RETURNING" in r.stdout
    assert elapsed < _BUDGET / 2, f"exit took {elapsed:.1f}s"
