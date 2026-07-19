"""The binding-centric matcher API.

`find_all` takes a single `Pattern` or a `list[Pattern]` (a list joins on
shared captures, returning merged bindings) and returns a deduplicated
`list[Match]`.  `find_unique` errors on 0 and on >1.

Dedup is controlled by `ignore_root`: the default keys on captures plus
root(s); `ignore_root=True` keys on captures only, collapsing one binding
reached from several roots.
"""

from __future__ import annotations

import pytest

import strider
from strider.pattern import Capture, any_int_const, call, load, int_const, var

from .conftest import built_function, fixture_path


def _switch_graph():
    return built_function("x86", "switch", "dispatch_value")


def _roots(matches):
    return sorted(m.root for m in matches)


def test_find_all_single_equals_one_element_list():
    # The 1-element-join invariant.
    g = _switch_graph()
    single = g.find_all(call())
    listed = g.find_all([call()])
    assert _roots(single) == _roots(listed)


def test_find_all_empty_list_is_empty():
    g = _switch_graph()
    assert g.find_all([]) == []


def test_ignore_root_collapses_captureless_pattern():
    # 5 loads at 5 distinct roots.  With no captures every binding is
    # identical, so ignore_root=True collapses them to one.
    g = _switch_graph()
    assert len(g.find_all(load())) == 5
    assert len(g.find_all(load(), ignore_root=True)) == 1


def test_ignore_root_keeps_distinct_captures():
    # Each load binds a distinct node to `c`, so the captures differ and
    # ignore_root=True still yields 5.
    g = _switch_graph()
    c = Capture()
    assert len(g.find_all(load().capture(c), ignore_root=True)) == 5


def test_find_unique_returns_the_single_binding():
    g = _switch_graph()
    m = g.find_unique(call())
    assert m is not None
    assert m.root == g.find_all(call())[0].root


def test_find_unique_raises_on_zero():
    g = _switch_graph()
    impossible = int_const(0xDEAD_BEEF_CAFE_BABE)
    with pytest.raises(strider.StriderError):
        g.find_unique(impossible)


def test_find_unique_raises_on_many():
    g = _switch_graph()  # 5 loads
    with pytest.raises(strider.StriderError):
        g.find_unique(load())


def test_find_all_returns_empty_when_no_match():
    g = _switch_graph()
    assert g.find_all(int_const(0xDEAD_BEEF_CAFE_BABE)) == []


def test_find_all_list_merges_shared_capture():
    # Two patterns share `target`; each merged Match reads the shared
    # capture directly, with no per-pattern tuple.
    elf = fixture_path("x86", "switch")
    f_addr = strider.lift.load_elf(str(elf)).symbol("f")
    g = _switch_graph()

    target = Capture()
    pat_call = call().target(var(target))
    pat_const = any_int_const(target)

    merged = g.find_all([pat_call, pat_const])
    assert len(merged) >= 1
    assert any(m.const_uint(target) == f_addr for m in merged)


def test_find_all_list_with_unmatchable_pattern_is_empty():
    g = _switch_graph()
    impossible = int_const(0xDEAD_BEEF_CAFE_BABE)
    assert g.find_all([call(), impossible]) == []


def test_find_all_list_disagreement_is_empty():
    g = _switch_graph()
    shared = Capture()
    pat_call = call().capture(shared)
    pat_const = int_const(0).capture(shared)
    assert g.find_all([pat_call, pat_const]) == []
