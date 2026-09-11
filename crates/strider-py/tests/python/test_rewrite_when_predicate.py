"""A `.when()` guard on a rewrite `find` pattern must be refused.

Only `find_all` / `find_unique` publish the function the predicate needs to
build its `Match`; a rewrite holds the function for mutation while its rules
fire, so the predicate could not read one. It used to be accepted, print an
internal error to stderr per candidate node, and rewrite nothing.
"""

import pytest

import strider
from strider.pattern import Capture, int_add, load, var

from .conftest import built_function


def _build_graph():
    return built_function("x86", "memory", "array_sum", optimize=False)


def _find_pat(x, y):
    return int_add(var(x), var(y))


def test_unguarded_rewrite_still_fires():
    x, y = Capture(), Capture()
    assert _build_graph().rewrite(find=_find_pat(x, y), replace=var(x)) > 0


def test_guarded_find_is_rejected():
    x, y = Capture(), Capture()
    with pytest.raises(strider.StriderError, match="find_all"):
        _build_graph().rewrite(
            find=_find_pat(x, y).when(lambda m: True), replace=var(x)
        )


def test_guarded_nested_operand_is_rejected():
    """The guard is found wherever it sits in the pattern, not just at the
    root."""
    x, y = Capture(), Capture()
    with pytest.raises(strider.StriderError, match="find_all"):
        _build_graph().rewrite(
            find=int_add(var(x).when(lambda m: True), var(y)), replace=var(y)
        )


def test_guarded_builder_is_rejected():
    """Typed builders carry `.when()` through their own state, a different
    path from `Pat.when`."""
    c = Capture()
    with pytest.raises(strider.StriderError, match="find_all"):
        _build_graph().rewrite(find=load().capture(c).when(lambda m: True), replace=var(c))


def test_guarded_rewrite_all_pair_is_rejected():
    x, y = Capture(), Capture()
    with pytest.raises(strider.StriderError, match="find_all"):
        _build_graph().rewrite_all([(_find_pat(x, y).when(lambda m: True), var(x))])


def test_rejection_does_not_leak_into_the_next_rewrite():
    """The scope flag is per-call: an unguarded rewrite after a rejected one
    still runs."""
    g = _build_graph()
    x, y = Capture(), Capture()
    with pytest.raises(strider.StriderError):
        g.rewrite(find=_find_pat(x, y).when(lambda m: True), replace=var(x))
    assert g.rewrite(find=_find_pat(x, y), replace=var(x)) > 0


def test_when_still_runs_in_find_all():
    x, y = Capture(), Capture()
    seen = []
    matches = _build_graph().find_all(
        _find_pat(x, y).when(lambda m: seen.append(m.root) or True)
    )
    assert seen
    assert matches


def test_guarded_into_pat_is_rejected():
    """`.into_pat()` compiles the guard in early; the finished pattern still
    reports it."""
    c = Capture()
    with pytest.raises(strider.StriderError, match="find_all"):
        _build_graph().rewrite(
            find=load().capture(c).when(lambda m: True).into_pat(), replace=var(c)
        )


def test_guarded_operand_of_into_pat_is_rejected():
    """A guard on an operand of a finished pattern reports itself too."""
    c = Capture()
    with pytest.raises(strider.StriderError, match="find_all"):
        _build_graph().rewrite(
            find=load().addr(var(c).when(lambda m: True)).into_pat(), replace=var(c)
        )
