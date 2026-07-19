"""Per-arch pattern tests: realistic queries against the `patterns`
fixture, asserting each shape survives the optimiser on every arch."""

from __future__ import annotations

from strider import pattern as pat
from strider.pattern import anything, var, Capture

from ._helpers import (
    analyze,
    count_int_binop,
    count_regions,
    count_pat,
    has_constant,
)


def test_mul_then_add(arch_id, fixtures_dir):
    # `ignore_casts` is needed for x64, whose register-merge emits
    # `Add(Extend(Mul), arg)`.
    g = analyze(arch_id, "patterns", "mul_then_add", fixtures_dir=fixtures_dir)
    p = pat.add(pat.mul(anything(), anything()), anything())
    hits = g.find_all(p, ignore_casts=True)
    assert len(hits) >= 1, "expected ≥1 add(mul(_,_), _) match"


def test_chained_xor_mask(arch_id, fixtures_dir):
    # ConstantFold collapses (x ^ k1) & m1 ^ k2 to (x & m1) ^ (k1^k2)
    # before matching, so the inner xor is gone; one Xor and one And
    # survive.
    g = analyze(arch_id, "patterns", "chained_xor_mask", fixtures_dir=fixtures_dir)
    assert count_int_binop(g, "Xor") >= 1
    assert count_int_binop(g, "And") >= 1


def test_if_returns_const(arch_id, fixtures_dir):
    # -50 appears at whatever width the arch materialises it: 0xffff_ffce
    # on 32-bit, sign-extended to 64 bits on x64. ARM-Thumb is the odd one:
    # gcc emits `mvnle r0, #49`, so the literal may still read as 49 if
    # ConstantFold hasn't folded the negation by the assertion point.
    g = analyze(arch_id, "patterns", "if_returns_const", fixtures_dir=fixtures_dir)
    assert has_constant(g, 100)
    neg50_u32 = (-50) & 0xFFFF_FFFF
    neg50_u64 = (-50) & 0xFFFF_FFFF_FFFF_FFFF
    assert (
        has_constant(g, neg50_u32)
        or has_constant(g, neg50_u64)
        or has_constant(g, 49)  # arm_thumb MVN-immediate pre-fold
    )


def test_loop_with_invariant_load(arch_id, fixtures_dir):
    g = analyze(arch_id, "patterns", "loop_with_invariant_load", fixtures_dir=fixtures_dir)
    hits = g.find_all(pat.load())
    assert len(hits) >= 1, "expected ≥1 Load match"
    assert count_regions(g) >= 1, "loop must remain"


def test_recursive_with_accumulator(arch_id, fixtures_dir):
    g = analyze(arch_id, "patterns", "recursive_with_accumulator", fixtures_dir=fixtures_dir)
    hits = g.find_all(pat.call())
    assert len(hits) >= 1, "expected ≥1 Call match"
    assert count_pat(g, pat.if_else()) >= 1
