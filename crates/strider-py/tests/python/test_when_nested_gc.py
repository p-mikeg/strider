"""A `.when()` predicate nested into another pattern stays visible to the GC.

The compiled closures own the predicate, and the owning `Pat` is the only
object that can report it. A `Pat` handed to an outer builder gives up its
compiled `Pattern`, so the outer one has to take over reporting it or a cycle
through the predicate is uncollectable.
"""

import gc
import subprocess
import sys
import textwrap

from strider import pattern as p


def _live_pats() -> int:
    gc.collect()
    return sum(1 for o in gc.get_objects() if type(o).__name__ == "Pat")


def _cycle(nested: bool) -> None:
    """pat -> compiled closure -> f -> holder -> pat."""
    holder: dict = {}

    def f(_m):
        return holder is not None

    inner = p.entry().when(f)
    pat = (
        p.if_else().true_branch(inner.into_pat()).into_pat()
        if nested
        else inner.into_pat()
    )
    holder["pat"] = pat


def _collects(nested: bool) -> bool:
    base = _live_pats()
    for _ in range(20):
        _cycle(nested)
    gc.collect()
    gc.collect()
    return _live_pats() == base


def test_flat_when_cycle_is_collected():
    assert _collects(False)


def test_nested_when_cycle_is_collected():
    assert _collects(True)


def test_nested_pat_reports_the_predicate():
    def f(_m):
        return True

    pat = p.if_else().true_branch(p.entry().when(f).into_pat()).into_pat()
    assert f in gc.get_referents(pat)


def _reports(obj, target) -> int:
    return sum(1 for r in gc.get_referents(obj) if r is target)


def test_every_report_of_a_nested_predicate_is_an_owned_reference():
    """The collector subtracts one reference per report, so two `Pat`s
    reporting a predicate they hold one reference to between them make it
    look unreachable while something else still holds it."""

    def f(_m):
        return True

    base = sys.getrefcount(f)
    inner = p.ret().when(f).into_pat()
    outer = p.if_else().true_branch(inner).into_pat()
    owned = sys.getrefcount(f) - base
    assert _reports(inner, f) + _reports(outer, f) == owned


def test_a_nested_predicate_held_elsewhere_survives_collection():
    """A reference the collector cannot see (a C extension's, simulated with
    `Py_IncRef`) keeps the predicate alive while both `Pat`s become cyclic
    garbage. Over-reported, the collector cleared the live function and the
    next call through it crashed, so the run is a child."""
    body = textwrap.dedent(
        """\
        import ctypes, gc
        from strider import pattern as p

        def make():
            def pred(m):
                return True
            inner = p.ret().when(pred).into_pat()
            outer = p.if_else().true_branch(inner).into_pat()
            ctypes.pythonapi.Py_IncRef(ctypes.py_object(pred))
            junk = [inner, outer]
            junk.append(junk)
            return id(pred)

        addr = make()
        gc.collect()
        pred = ctypes.cast(addr, ctypes.py_object).value
        print(type(pred.__globals__).__name__, pred(None), flush=True)
        """
    )
    out = subprocess.run(
        [sys.executable, "-c", body], capture_output=True, text=True, timeout=120
    )
    assert out.returncode == 0, f"child exited {out.returncode}: stderr={out.stderr!r}"
    assert out.stdout.split() == ["dict", "True"], out.stdout
