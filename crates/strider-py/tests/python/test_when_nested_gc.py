"""A `.when()` predicate nested into another pattern stays visible to the GC.

The compiled closures own the predicate, and the owning `Pat` is the only
object that can report it. A `Pat` handed to an outer builder gives up its
compiled `Pattern`, so the outer one has to take over reporting it or a cycle
through the predicate is uncollectable.
"""

import gc

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
