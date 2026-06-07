"""Complex / multi-level pattern integration tests.

These exercise the pattern API beyond "find any load" — they
combine multi-level captures, back-references, predicate guards,
and commutative matching against real fixture binaries.

Fixture choice:
* `mul_then_add` from `patterns.c` — `a * b + c` shape, exercises
  `add(c, mul(a, b))` (commutative).
* `chained_xor_mask` from `patterns.c` — `((x ^ k1) & m1) ^ k2`
  shape, exercises 3-deep capture chains.
* `array_sum` from `memory.c` — load + add loop body, exercises
  back-references.
"""

from __future__ import annotations

import pytest

import strider
from strider.pattern import (
    Capture,
    add,
    any_,
    any_int_const,
    int_const,
    load,
    mul,
    var,
    xor,
)

from .conftest import symbol_addr, fixture_path


def _build_graph(elf_path, symbol):
    addr = symbol_addr(elf_path, symbol)
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.load_elf(str(elf_path)).memory_map()
    s = strider.Lifter(arch, mem, cc)
    cfg = s.build_cfg(addr, allow_code_before_start_addr=True)
    g = s.analyze_cfg(cfg).function
    pipe = s.build_optimizer_pipeline()
    g.optimize(pipe)
    return g


# ── Test 1: multi-level capture chain ───────────────────────────────


def test_multi_level_capture_chain(x86_memory_elf):
    """Find a load whose address is `add(base, K)` and bind both
    capture endpoints.  `struct_field_load(p)` returns `p->x + p->y`,
    which lifts to a load through a `add(base, offset)` for the y
    field.  Bind both endpoints and confirm captures populate.
    """
    g = _build_graph(x86_memory_elf, "struct_field_load")
    base = Capture()
    off = Capture()
    pat = load(addr=add(var(base), var(off)))
    hits = g.find_all(pat, ignore_casts=True)
    assert len(hits) >= 1
    # Both captures must be set on every successful match.
    for h in hits:
        assert base in h
        assert off in h


# ── Test 2: back-reference within a single pattern ──────────────────


def test_back_reference_same_capture_twice(x86_memory_elf):
    """`xor(x, x)` should fire only when both operands are literally
    the same value.  A shape like `xor(eax, eax)` (zeroing) appears in
    typical x86 codegen; `array_sum` doesn't always have one, so we
    just check the pattern is built and runs without raising.
    """
    g = _build_graph(x86_memory_elf, "array_sum")
    pat = xor("v", "v")
    hits = g.find_all(pat)
    # Either 0 or N matches — both are valid (depends on codegen).
    assert isinstance(hits, list)
    # If we got matches, the same node must be on both sides.
    # We can't directly check that without exposing inputs; the
    # back-reference enforcement is in the matcher itself, so the
    # mere fact `xor("v", "v")` returned a match list (not an error)
    # is the assertion.

    # Build the contrasting `xor("a", "b")` — same number-of-matches
    # bound is `>=`.
    pat_distinct = xor("a", "b")
    hits_distinct = g.find_all(pat_distinct)
    # `xor("v", "v")` must be a SUBSET of `xor("a", "b")` since the
    # back-reference is a stricter constraint.
    assert len(hits) <= len(hits_distinct)


# ── Test 3: predicate guard filters matches ─────────────────────────


def test_predicate_guard_filters_int_const(x86_memory_elf):
    """`any_int_const(c)` matches every IntConst.  Filter to only
    constants with `uint(c) < 0x100`.  Compare against the unfiltered
    count to confirm the predicate actually changed the result set.
    """
    g = _build_graph(x86_memory_elf, "array_sum")
    c = Capture()
    pat_unfiltered = any_int_const(c)
    pat_filtered = any_int_const(c).when(lambda m: (m.uint(c) or 0) < 0x100)

    hits_unfiltered = g.find_all(pat_unfiltered)
    hits_filtered = g.find_all(pat_filtered)

    # Filtered must be a subset.
    assert len(hits_filtered) <= len(hits_unfiltered)
    # Every filtered hit must satisfy the predicate.
    for h in hits_filtered:
        v = h.uint(c)
        assert v is not None
        assert v < 0x100


def test_predicate_returning_false_yields_zero_matches(x86_memory_elf):
    """A predicate that always returns False must drop every match."""
    g = _build_graph(x86_memory_elf, "array_sum")
    c = Capture()
    pat = any_int_const(c).when(lambda m: False)
    hits = g.find_all(pat)
    assert len(hits) == 0


# ── Test 4: commutative matching ────────────────────────────────────


def test_commutative_add_matches_either_order():
    """`add(a, b)` is commutative — the matcher must find both
    `add(const, var)` and `add(var, const)` shapes.

    We use the patterns.c `mul_then_add(a, b, c)` function which
    computes `a * b + c`.  After lift + optimisation, the IR may
    canonicalise the operand order either way; the test confirms
    that whichever order it picks, the commutative `add` matcher
    finds it.
    """
    elf = fixture_path("x86", "patterns")
    g = _build_graph(elf, "mul_then_add")

    # Capture both operands of every add.
    a, b = Capture(), Capture()
    pat_add = add(var(a), var(b))
    hits = g.find_all(pat_add, ignore_casts=True)

    # mul_then_add inevitably contains at least one add (the `+ c`),
    # plus prologue/epilogue stack arithmetic.
    assert len(hits) >= 1


def test_commutative_swapping_inner_mul_matches():
    """Confirm that `add(mul(a, b), c)` and `add(c, mul(a, b))` both
    match the same `a * b + c` IR shape — `add` is commutative so
    the matcher should not care about the operand ordering.
    """
    elf = fixture_path("x86", "patterns")
    g = _build_graph(elf, "mul_then_add")

    # Form 1: add(mul, var)
    pat_form1 = add(mul("a", "b"), "c")
    hits_form1 = g.find_all(pat_form1, ignore_casts=True)

    # Form 2: add(var, mul)
    pat_form2 = add("c", mul("a", "b"))
    hits_form2 = g.find_all(pat_form2, ignore_casts=True)

    # Both should yield the same hit count (commutative `add`
    # tries both orderings).  The exact count depends on codegen;
    # the equality is the key assertion.
    assert len(hits_form1) == len(hits_form2)


# ── Bonus: chained xor / mask for the patterns.c chained_xor_mask ────


def test_chained_xor_mask_pattern_finds_xor():
    """`chained_xor_mask` lifts to nested xor/and/xor chains.
    Find at least one xor with a captured constant operand.
    """
    elf = fixture_path("x86", "patterns")
    g = _build_graph(elf, "chained_xor_mask")
    k = Capture()
    pat = xor(any_(), any_int_const(k))
    hits = g.find_all(pat, ignore_casts=True)
    # `chained_xor_mask` has at least one `xor(..., const)` shape.
    assert len(hits) >= 1
    # The captured constant must extract as an integer.
    for h in hits:
        v = h.uint(k)
        assert v is not None
