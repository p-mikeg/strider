"""The heap-noalias surface: the `noalias_allocators` option and the
`heap_only()` load/store filter. The alias-analysis behaviour itself is
covered by the Rust suite; this pins the Python API."""

from __future__ import annotations

import strider
from strider.pattern import Capture, int_mul, load, store, var

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


# x86-64: `call 0x1100; mov qword [rax], 42; ret`.  With 0x1100 declared a pure
# allocator, `[rax]` is a heap-rooted address and the store is `heap_only()`.
_HEAP_CODE = bytes.fromhex("e8fb000000" "48c7002a000000" "c3")
_HEAP_BASE = 0x1000
_HEAP_ALLOCATOR = 0x1100


def _heap_function(allocators=(_HEAP_ALLOCATOR,)):
    mem = strider.reader.BufferReader(_HEAP_BASE, _HEAP_CODE)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    opts = strider.lift.LifterOptions(
        assumptions=strider.lift.AssumptionOptions(
            noalias_allocators=list(allocators)
        )
    )
    _cfg, function, _unresolved = lift.analyze(
        _HEAP_BASE, strider.sleigh.CallingConvention.x86_64_systemv(), opts
    )
    return lift, function


def test_analyze_classifies_an_allocator_return_as_heap():
    _lift, g = _heap_function()
    assert len(g.find_all(store().heap_only())) == 1


def test_rewrite_keeps_the_allocator_set_analyze_ran_under():
    """`rewrite` refills the memory-class table every call.  Refilling it
    without the run's `noalias_allocators` reclassifies every heap access as
    not-memory, so `heap_only()` silently stops matching."""
    _lift, g = _heap_function()
    assert len(g.find_all(store().heap_only())) == 1
    x, y = Capture(), Capture()
    assert g.rewrite(find=int_mul(var(x), var(y)), replace=var(x)) == 0
    assert len(g.find_all(store().heap_only())) == 1, (
        "a rewrite that fires nothing must not drop the store's heap class"
    )


def test_clone_keeps_the_allocator_set_for_rewrite():
    _lift, g = _heap_function()
    clone = g.clone()
    x, y = Capture(), Capture()
    clone.rewrite(find=int_mul(var(x), var(y)), replace=var(x))
    assert len(clone.find_all(store().heap_only())) == 1


def test_optimize_updates_the_allocator_set_rewrite_uses():
    """`optimize` is the other writer of the remembered assumptions: a set
    handed to it must reach a later `rewrite`, even though `analyze` ran
    without one."""
    lift, g = _heap_function(allocators=())
    assert g.find_all(store().heap_only()) == []
    lift.optimize(
        g,
        opts=strider.lift.LifterOptions(
            assumptions=strider.lift.AssumptionOptions(
                noalias_allocators=[_HEAP_ALLOCATOR]
            )
        ),
    )
    assert len(g.find_all(store().heap_only())) == 1
    x, y = Capture(), Capture()
    g.rewrite(find=int_mul(var(x), var(y)), replace=var(x))
    assert len(g.find_all(store().heap_only())) == 1
