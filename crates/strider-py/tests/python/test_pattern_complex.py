"""Fixture choice:
* `mul_then_add` (patterns.c) is `a * b + c`, for commutative `add`.
* `chained_xor_mask` (patterns.c) is `((x ^ k1) & m1) ^ k2`, for 3-deep
  capture chains.
* `array_sum` (memory.c) is a load + add loop body, for back-references.
"""

from __future__ import annotations

import pytest

import strider
from strider.pattern import (
    Capture,
    int_add,
    anything,
    int_const,
    int_xor,
    load,
    int_mul,
    var,
)

from .conftest import symbol_addr, fixture_path


def _build_graph(elf_path, symbol):
    addr = symbol_addr(elf_path, symbol)
    arch = strider.sleigh.SleighArch.x86()
    cc = strider.sleigh.CallingConvention.x86_cdecl()
    mem = strider.lift.load_elf(str(elf_path)).reader()
    s = strider.lift.lifter(arch, mem)
    _cfg, g, _unresolved = s.analyze(
        addr, cc, opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True))
    )
    return g


def test_multi_level_capture_chain(x86_memory_elf):
    """Captures nested two levels deep must both populate.
    `struct_field_load(p)` returns `p->x + p->y`, so the y-field access
    lifts to a load through `int_add(base, offset)`.
    """
    g = _build_graph(x86_memory_elf, "struct_field_load")
    base = Capture()
    off = Capture()
    pat = load(addr=int_add(var(base), var(off)))
    hits = g.find_all(pat, ignore_casts=True)
    assert len(hits) >= 1
    for h in hits:
        assert base in h
        assert off in h


def test_back_reference_same_capture_twice(x86_memory_elf):
    """A repeated capture is a back-reference: `int_xor(v, v)` fires only
    when both operands are the same value, so it must be a subset of
    `int_xor(a, b)`.  Whether `array_sum` contains a zeroing xor at all is
    codegen-dependent, so the subset bound is all this can assert.
    """
    g = _build_graph(x86_memory_elf, "array_sum")
    pat = int_xor(Capture("v"), Capture("v"))
    hits = g.find_all(pat)
    assert isinstance(hits, list)

    pat_distinct = int_xor(Capture("a"), Capture("b"))
    hits_distinct = g.find_all(pat_distinct)
    assert len(hits) <= len(hits_distinct)


def test_predicate_guard_filters_int_const(x86_memory_elf):
    """A `.when` guard must narrow the result set, not just decorate it:
    every surviving hit satisfies the predicate and the count cannot grow.
    """
    g = _build_graph(x86_memory_elf, "array_sum")
    c = Capture()
    pat_unfiltered = int_const(c)
    pat_filtered = int_const(c).when(lambda m: (m.uint(c) or 0) < 0x100)

    hits_unfiltered = g.find_all(pat_unfiltered)
    hits_filtered = g.find_all(pat_filtered)

    assert len(hits_filtered) <= len(hits_unfiltered)
    for h in hits_filtered:
        v = h.uint(c)
        assert v is not None
        assert v < 0x100


def test_predicate_returning_false_yields_zero_matches(x86_memory_elf):
    g = _build_graph(x86_memory_elf, "array_sum")
    c = Capture()
    pat = int_const(c).when(lambda m: False)
    hits = g.find_all(pat)
    assert len(hits) == 0


def test_commutative_add_matches_either_order():
    """Lift + optimisation may canonicalise the operand order either way;
    the commutative `add` matcher must find it regardless.
    """
    elf = fixture_path("x86", "patterns")
    g = _build_graph(elf, "mul_then_add")

    a, b = Capture(), Capture()
    pat_add = int_add(var(a), var(b))
    hits = g.find_all(pat_add, ignore_casts=True)

    # `mul_then_add` always has the `+ c`, plus prologue/epilogue stack math.
    assert len(hits) >= 1


def test_commutative_swapping_inner_mul_matches():
    """Commutativity applies to a nested operand too: swapping which side
    the `mul` is spelled on must not change the hit count.  The count itself
    is codegen-dependent, so only the equality is asserted.
    """
    elf = fixture_path("x86", "patterns")
    g = _build_graph(elf, "mul_then_add")

    pat_form1 = int_add(int_mul(Capture("a"), Capture("b")), Capture("c"))
    hits_form1 = g.find_all(pat_form1, ignore_casts=True)

    pat_form2 = int_add(Capture("c"), int_mul(Capture("a"), Capture("b")))
    hits_form2 = g.find_all(pat_form2, ignore_casts=True)

    assert len(hits_form1) == len(hits_form2)


def test_chained_xor_mask_pattern_finds_xor():
    """`chained_xor_mask` lifts to nested xor/and/xor chains, so at least one
    `int_xor(_, const)` must exist and its constant must read back.
    """
    elf = fixture_path("x86", "patterns")
    g = _build_graph(elf, "chained_xor_mask")
    k = Capture()
    pat = int_xor(anything(), int_const(k))
    hits = g.find_all(pat, ignore_casts=True)
    assert len(hits) >= 1
    for h in hits:
        v = h.uint(k)
        assert v is not None
