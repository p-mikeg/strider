"""Dropping a deeply nested `JoinConstraint` must not recurse into the stack.

A composed constraint owns the constraint it wrapped, so freeing a chain of
them descends one native frame per link while the Python `for` loop that built
it hit no interpreter limit.  A depth the nesting guard ACCEPTS used to SIGSEGV
on the free, which no Python `except` sees, so the child runs in a subprocess
and the parent reads its exit code.
"""

from __future__ import annotations

import subprocess
import sys
import textwrap

#: One under `MAX_CONSTRAINT_NESTING`, so the guard admits the chain.
_DEEP = 511

#: Half the smallest stack the compile budget is sized for.
_SMALL_STACK = 512 * 1024


def _run(depth: int, stack: int) -> subprocess.CompletedProcess:
    body = textwrap.dedent(
        f"""\
        import threading

        from strider import pattern as p
        from strider.pattern import constraints as cons

        def build():
            c = cons.dominates(p.Capture("a"), p.Capture("b"))
            for _ in range({depth}):
                c = cons.negate(c)
            print("built", flush=True)
            del c
            print("dropped", flush=True)

        threading.stack_size({stack})
        t = threading.Thread(target=build)
        t.start()
        t.join()
        """
    )
    return subprocess.run(
        [sys.executable, "-c", body], capture_output=True, text=True, timeout=120
    )


def test_a_chain_at_the_nesting_limit_drops_without_killing_the_process():
    out = _run(_DEEP, _SMALL_STACK)
    assert out.returncode == 0, f"child exited {out.returncode}: stderr={out.stderr!r}"
    assert "dropped" in out.stdout, out.stdout


def test_the_same_chain_survives_a_query_and_the_free_after_it():
    """The query materialises the tree a second time, so the drop it leaves
    behind is the one the free has to unwind."""
    body = textwrap.dedent(
        f"""\
        import threading

        import strider
        from strider import pattern as p
        from strider.pattern import constraints as cons

        mem = strider.reader.BufferReader(0x1000, bytes([0x48, 0x01, 0xF8, 0xC3]))
        lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
        fn = lift.analyze(
            0x1000, strider.sleigh.CallingConvention.x86_64_systemv()
        ).function

        def run():
            a, b = p.Capture("a"), p.Capture("b")
            c = cons.dominates(a, b)
            for _ in range({_DEEP}):
                c = cons.negate(c)
            pats = [p.int_add(p.anything(), p.anything()).capture(a),
                    p.ret().capture(b)]
            print("query", len(fn.find_all(pats, constraints=[c])), flush=True)
            del c
            print("dropped", flush=True)

        threading.stack_size({_SMALL_STACK})
        t = threading.Thread(target=run)
        t.start()
        t.join()
        """
    )
    out = subprocess.run(
        [sys.executable, "-c", body], capture_output=True, text=True, timeout=120
    )
    assert out.returncode == 0, f"child exited {out.returncode}: stderr={out.stderr!r}"
    assert "dropped" in out.stdout, out.stdout
