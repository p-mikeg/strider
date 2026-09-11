"""Stale-handle invalidation after in-place rewrites.

`rewrite` / `rewrite_all` mutate without compacting, so node ids stay
valid and pre-existing `Match` / `Node` handles would silently read
post-rewrite state. Instead they must raise `StriderError`; handles
obtained after the rewrite keep working.
"""

from __future__ import annotations

import pytest

import strider
from strider.pattern import Capture, int_add, var

from .conftest import built_function


def _build_graph():
    # Unoptimized so `int_add(a, b)` shapes survive for the rewrite to fire.
    return built_function("x86", "memory", "array_sum", optimize=False)


def test_match_handle_stale_after_rewrite():
    g = _build_graph()
    x, y = Capture(), Capture()
    hits = g.find_all(int_add(var(x), var(y)))
    assert hits, "fixture must contain at least one int_add(a, b) to rewrite"
    stale_match = hits[0]

    fired = g.rewrite(find=int_add(var(x), var(y)), replace=var(x))
    assert fired > 0, "the rewrite must actually mutate the graph"

    with pytest.raises(strider.StriderError):
        stale_match.uint(x)
    with pytest.raises(strider.StriderError):
        stale_match.node(x)


def test_node_handle_stale_after_rewrite():
    g = _build_graph()
    x, y = Capture(), Capture()
    hits = g.find_all(int_add(var(x), var(y)))
    assert hits
    stale_node = hits[0].node(x)
    assert stale_node is not None

    fired = g.rewrite(find=int_add(var(x), var(y)), replace=var(x))
    assert fired > 0

    with pytest.raises(strider.StriderError):
        stale_node.kind()


def test_node_handle_stale_after_rewrite_all():
    g = _build_graph()
    x, y = Capture(), Capture()
    hits = g.find_all(int_add(var(x), var(y)))
    assert hits
    stale_node = hits[0].node(x)
    assert stale_node is not None

    fired = g.rewrite_all([(int_add(var(x), var(y)), var(x))])
    assert fired > 0

    with pytest.raises(strider.StriderError):
        stale_node.kind()


def test_handle_obtained_after_rewrite_still_works():
    g = _build_graph()
    x, y = Capture(), Capture()
    g.rewrite(find=int_add(var(x), var(y)), replace=var(x))

    # A fresh query samples the post-rewrite generation, so its handles
    # read the mutated graph without tripping the staleness guard.
    a = Capture()
    fresh = g.find_all(var(a))
    assert fresh
    node = fresh[0].node(a)
    assert node is not None
    assert isinstance(node.kind(), str)
