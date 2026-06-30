"""`Function.clone()` + non-destructive clone-then-rewrite.

`clone()` returns a deep, independent copy of the IR function: mutating
the clone (via `rewrite` / `optimize`) must never touch the original.
These tests exercise that independence directly — match counts on the
original are invariant across rewrites of the clone, and the clone's
generation advances independently so a pre-rewrite `Match` on the clone
goes stale while the original's handles keep working.
"""

from __future__ import annotations

import pytest

import strider
from strider.pattern import Capture, add, var, int_const

from .conftest import built_function


def _build_graph(symbol="array_sum"):
    # Unoptimized so the raw `add(a, b)` shapes survive for a rewrite to fire.
    return built_function("x86", "memory", symbol, optimize=False)


def test_clone_is_independent_of_original():
    """Rewriting the clone leaves the original's match population intact."""
    g = _build_graph()
    x, y = Capture(), Capture()

    before = len(g.find_all(add(var(x), var(y))))
    assert before > 0, "fixture must contain at least one add(a, b)"

    clone = g.clone()
    # The clone starts as a structural twin: same match count.
    assert len(clone.find_all(add(var(x), var(y)))) == before

    # Collapse every add(a, b) → a on the CLONE only.
    fired = clone.rewrite(find=add(var(x), var(y)), replace=var(x))
    assert fired > 0, "the rewrite must mutate the clone"

    # The original is untouched: same number of adds as before the clone.
    after_original = len(g.find_all(add(var(x), var(y))))
    assert after_original == before, "rewriting the clone must not touch the original"

    # The clone genuinely shrank.
    after_clone = len(clone.find_all(add(var(x), var(y))))
    assert after_clone < before, "clone lost adds to the rewrite"


def test_clone_then_rewrite_identity_fold_fires():
    """`add(x, 0) → x` on a clone returns the fire count and drops matches to 0."""
    g = _build_graph()
    x = Capture()

    clone = g.clone()
    # Pre-condition: at least one `add(_, 0)` exists to fold.
    matches_before = len(clone.find_all(add(var(x), int_const(0))))
    if matches_before == 0:
        pytest.skip("fixture has no add(_, 0) to fold")

    fired = clone.rewrite(find=add(var(x), int_const(0)), replace=var(x))
    assert isinstance(fired, int)
    assert fired == matches_before

    # After folding, no `add(_, 0)` remains.
    assert len(clone.find_all(add(var(x), int_const(0)))) == 0


def test_rewrite_returns_zero_when_nothing_matches():
    """A rewrite whose LHS matches nothing fires zero times and is a no-op."""
    g = _build_graph()
    clone = g.clone()

    # A capture-only LHS that requires an add-of-two-identical-huge-consts:
    # int_const(0xDEADBEEF12345678) + int_const(0xDEADBEEF12345678).
    sentinel = 0xDEAD_BEEF_1234_5678
    fired = clone.rewrite(
        find=add(int_const(sentinel), int_const(sentinel)),
        replace=int_const(sentinel),
    )
    assert fired == 0


def test_clone_match_staleness_is_independent():
    """A pre-rewrite Match on the clone goes stale after the clone is
    rewritten; the original's matches keep working (independent generations).
    """
    g = _build_graph()
    x, y = Capture(), Capture()

    clone = g.clone()

    # Handles sampled BEFORE the clone's rewrite.
    orig_hits = g.find_all(add(var(x), var(y)))
    clone_hits = clone.find_all(add(var(x), var(y)))
    assert orig_hits and clone_hits

    fired = clone.rewrite(find=add(var(x), var(y)), replace=var(x))
    assert fired > 0

    # The clone's pre-rewrite handle is now stale.
    with pytest.raises(strider.errors.StriderError):
        clone_hits[0].uint(x)

    # The original's handle — same generation as before — still reads fine.
    node = orig_hits[0].node(x)
    assert node is not None
    assert isinstance(node.kind(), str)
