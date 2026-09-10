"""`shutdown()` returns as soon as the serve loop has, from any thread.

A foreground `visualize` registers its CALLER's thread, which outlives the
serve loop it was parked in; joining that thread from elsewhere can only ever
burn the whole timeout.
"""

from __future__ import annotations

import threading
import time

from strider import explore


class _StubVisualizer:
    """Enough of the visualizer protocol for `_Server` to bind and serve."""

    def entry(self) -> int:
        return 0

    def controls(self) -> list:
        return []

    def dot(self, center: int, params) -> str:
        return "digraph {}"

    def search(self, query: str) -> dict:
        return {}

    def completions(self) -> list:
        return []


def test_shutdown_from_another_thread_does_not_join_the_serving_caller():
    elapsed: list[float] = []
    serving = threading.Event()

    def stopper() -> None:
        serving.wait(10.0)
        # The serve loop is entered from the main thread below; `shutdown`
        # carries its own `started` guard, so a small head start is enough.
        time.sleep(0.2)
        t = time.perf_counter()
        explore.shutdown()
        elapsed.append(time.perf_counter() - t)

    worker = threading.Thread(target=stopper, daemon=True)
    worker.start()
    serving.set()
    # Foreground: blocks on this thread until `stopper` calls `shutdown`.
    explore._serve(_StubVisualizer(), host="127.0.0.1", port=0)
    worker.join(20.0)

    assert elapsed, "the stopping thread never returned from shutdown()"
    assert elapsed[0] < explore._SHUTDOWN_JOIN_SECONDS / 2, (
        f"shutdown() from another thread took {elapsed[0]:.2f} s"
    )
