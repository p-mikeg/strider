"""The heap-noalias surface: the `noalias_allocators` option and the
`heap_only()` load/store filter. The alias-analysis behaviour itself is
covered by the Rust suite; this pins the Python API."""

from __future__ import annotations

import strider
from strider.pattern import load, store

from .conftest import built_function


def test_noalias_allocators_option_round_trips():
    o = strider.lift.LifterOptions(
        assumptions=strider.lift.AssumptionOptions(
            noalias_allocators=[0x1000, 0x2000]
        )
    )
    assert o.assumptions.noalias_allocators == [0x1000, 0x2000]
    assert "noalias_allocators=[4096, 8192]" in repr(o)


def test_noalias_allocators_defaults_empty():
    assert strider.lift.LifterOptions().assumptions.noalias_allocators == []


def test_heap_only_builds_on_load_and_store():
    # heap_only() produces a valid pattern the matcher accepts. With no
    # allocators configured the fixture has no heap-classified access, so the
    # result is an empty list rather than an error.
    g = built_function("x86", "memory", "array_sum")
    assert isinstance(g.find_all(load().heap_only()), list)
    assert isinstance(g.find_all(store().heap_only()), list)
