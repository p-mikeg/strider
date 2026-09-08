"""A client that stops reading must not park the single-threaded serve loop.

The explorer serves one request at a time, so a response write is inside
`finish_request` and neither `shutdown` nor the interpreter's non-daemon-thread
join can reach the loop while it runs. A valid `GET /viz.js` whose 1.4 MB body
the client never drains is the whole attack: as the FIRST connection it also
kept `started` clear, so `shutdown` reported `[]` for a server it had not
stopped and the process then hung at exit.

Run in a subprocess: the failure is a wall-clock hang at interpreter exit,
which no in-process assertion can observe.
"""

from __future__ import annotations

import subprocess
import sys
import time

STALLED_CLIENT = """
import socket, time
from strider import explore

class V:
    def entry(self): return 0
    def controls(self): return [{"name": "depth", "kind": "int", "default": 5}]
    def dot(self, center, params): return "digraph {}"
    def search(self, query): return {"highlight": []}
    def completions(self): return []

port = explore._serve_background(V(), host="127.0.0.1", port=0, depth=None)
# A tiny receive buffer keeps the body in the server's write rather than
# letting the kernel swallow all 1.4 MB of it.
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 2048)
s.connect(("127.0.0.1", port))
s.sendall(b"GET /viz.js HTTP/1.1\\r\\nHost: 127.0.0.1\\r\\n\\r\\n")
time.sleep(1.0)
print("STOPPED", explore.shutdown(port), flush=True)
"""

#: Generous next to the ~2 s a healthy run takes, and far under the write
#: deadline a regression would park for.
_BUDGET = 45.0


def test_a_stalled_first_client_neither_hides_the_shutdown_nor_hangs_exit():
    t0 = time.monotonic()
    r = subprocess.run(
        [sys.executable, "-c", STALLED_CLIENT],
        capture_output=True,
        text=True,
        timeout=_BUDGET,
    )
    elapsed = time.monotonic() - t0
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    stopped = next(
        ln for ln in r.stdout.splitlines() if ln.startswith("STOPPED")
    ).removeprefix("STOPPED ")
    assert stopped != "[]", "shutdown reported nothing stopped while still serving"
    # The hang is at exit, after `shutdown` has already returned.
    assert elapsed < _BUDGET / 2, f"exit took {elapsed:.1f}s"
