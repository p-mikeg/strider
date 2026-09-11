"""A `.when()` predicate must run wherever it is attached, and a predicate
that raises must not be swallowed.

A builder nested in a `mem` slot compiles through a different path from a
value operand, and used to drop its predicate: the query matched every site,
and `rewrite()`, which refuses a guarded `find` pattern, never saw the guard
either, so it mutated the graph with the condition discarded.
"""

from __future__ import annotations

import pytest

import strider
from strider import pattern as p
from strider.pattern import Capture, int_add, var

from .conftest import built_function


def _graph():
    return built_function("x86", "memory", "array_sum", optimize=False)


def _load_after_store(guard=None):
    inner = p.store() if guard is None else p.store().when(guard)
    return p.load().mem(inner)


def test_when_in_a_mem_slot_runs():
    g = _graph()
    assert g.find_all(_load_after_store()), "the unguarded shape must exist"

    seen = []
    hits = g.find_all(_load_after_store(lambda m: seen.append(m) or False))
    assert seen, "the predicate in a mem slot never ran"
    assert not hits, "a False predicate in a mem slot must reject every site"


def test_when_in_a_mem_slot_can_accept():
    g = _graph()
    baseline = len(g.find_all(_load_after_store()))
    assert len(g.find_all(_load_after_store(lambda m: True))) == baseline


def test_guarded_mem_rewrite_is_rejected():
    """The rewrite-LHS guard must see a predicate nested in a memory slot."""
    g = _graph()
    c = Capture()
    with pytest.raises(strider.StriderError, match="find_all"):
        g.rewrite(
            find=p.load().capture(c).mem(p.store().when(lambda m: True)),
            replace=var(c),
        )


def test_a_raising_predicate_propagates():
    g = _graph()
    x, y = Capture(), Capture()

    def boom(m):
        raise ValueError("predicate blew up")

    with pytest.raises(ValueError, match="predicate blew up"):
        g.find_all(int_add(var(x), var(y)).when(boom))


def test_a_raising_predicate_in_a_mem_slot_propagates():
    g = _graph()

    def boom(m):
        raise ValueError("mem predicate blew up")

    with pytest.raises(ValueError, match="mem predicate blew up"):
        g.find_all(_load_after_store(boom))


def test_a_non_bool_return_propagates():
    """A predicate returning a non-bool is a TypeError, not a silent skip."""
    g = _graph()
    x, y = Capture(), Capture()
    with pytest.raises(Exception):
        # Deliberate: the binding extracts a real bool, nothing looser.
        g.find_all(int_add(var(x), var(y)).when(lambda m: object()))  # type: ignore[arg-type,return-value]
