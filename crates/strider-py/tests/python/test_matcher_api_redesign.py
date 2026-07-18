"""Tests for the unified binding-centric matcher API:

- `find_all(pat, ...)` accepts a single `Pattern` *or* a `list[Pattern]`
  (a list behaves like the old `find_joined`, returning merged bindings),
  and returns a deduplicated `list[Match]`.
- `find_all(pat, ...)[0]` is the first binding; `[]` means no match.
- `find_unique(pat, ...)` returns the single binding, erroring on 0 and >1.

Dedup is controlled by `ignore_root`: default keys on captures+root(s);
`ignore_root=True` keys on captures only.  See `docs/python-matcher-api.md`.
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
    # The 1-element-join invariant: find_all(p) == find_all([p]).
    g = _switch_graph()
    single = g.find_all(call())
    listed = g.find_all([call()])
    assert _roots(single) == _roots(listed)


def test_find_all_empty_list_is_empty():
    g = _switch_graph()
    assert g.find_all([]) == []


def test_ignore_root_collapses_captureless_pattern():
    # 5 loads at 5 distinct roots.  Default dedup keeps them apart (5);
    # ignore_root=True keys on captures only — with no captures every
    # binding is identical, so they collapse to one.
    g = _switch_graph()
    assert len(g.find_all(load())) == 5
    assert len(g.find_all(load(), ignore_root=True)) == 1


def test_ignore_root_keeps_distinct_captures():
    # Each load binds a distinct node to `c`, so ignore_root=True still
    # yields 5 (the captures differ even though the pattern is the same).
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
    with pytest.raises(strider.errors.StriderError):
        g.find_unique(impossible)


def test_find_unique_raises_on_many():
    g = _switch_graph()  # 5 loads
    with pytest.raises(strider.errors.StriderError):
        g.find_unique(load())


def test_find_all_returns_empty_when_no_match():
    g = _switch_graph()
    assert g.find_all(int_const(0xDEAD_BEEF_CAFE_BABE)) == []


def test_find_all_list_merges_shared_capture():
    # Two patterns share `target`; a list input returns MERGED bindings —
    # each Match reads the shared capture directly (no per-pattern tuple).
    elf = fixture_path("x86", "switch")
    f_addr = strider.load_elf(str(elf)).symbol("f")
    g = _switch_graph()

    target = Capture()
    pat_call = call().target(var(target))
    pat_const = any_int_const(target)

    merged = g.find_all([pat_call, pat_const])
    assert len(merged) >= 1
    assert any(m.const_uint(target) == f_addr for m in merged)


def test_find_all_list_with_unmatchable_pattern_is_empty():
    # A pattern that cannot match anywhere makes the whole join empty.
    g = _switch_graph()
    impossible = int_const(0xDEAD_BEEF_CAFE_BABE)
    assert g.find_all([call(), impossible]) == []


def test_find_all_list_disagreement_is_empty():
    g = _switch_graph()
    shared = Capture()
    pat_call = call().capture(shared)
    pat_const = int_const(0).capture(shared)
    assert g.find_all([pat_call, pat_const]) == []
