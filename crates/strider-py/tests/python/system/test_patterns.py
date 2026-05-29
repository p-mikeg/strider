"""Per-arch pattern tests.

Mirror of `crates/strider/tests/patterns.rs`: drives realistic pattern
queries against the `patterns` fixture and asserts each shape survives
the optimiser pipeline at least once per arch.
"""

from __future__ import annotations

from strider import pattern as pat
from strider.pattern import any_, var, Capture

from ._helpers import (
    analyze,
    count_int_binop,
    count_regions,
    count_pat,
    has_constant,
)


def test_mul_then_add(arch_id, fixtures_dir):
    # add(mul(_, _), _) — `ignore_casts` lets the matcher walk through
    # x64's `Add(Extend(Mul), arg)` register-merge chain transparently.
    g = analyze(arch_id, "patterns", "mul_then_add", fixtures_dir=fixtures_dir)
    p = pat.add(pat.mul(any_(), any_()), any_())
    hits = g.find_all(p, ignore_casts=True)
    assert len(hits) >= 1, "expected ≥1 add(mul(_,_), _) match"


def test_chained_xor_mask(arch_id, fixtures_dir):
    # ConstantFold collapses (x ^ k1) & m1 ^ k2 → (x & m1) ^ (k1^k2)
    # before pattern matching — the inner xor disappears.  We assert
    # one Xor + one And survive — same shape contract as the Rust test.
    g = analyze(arch_id, "patterns", "chained_xor_mask", fixtures_dir=fixtures_dir)
    assert count_int_binop(g, "Xor") >= 1
    assert count_int_binop(g, "And") >= 1


def test_if_returns_const(arch_id, fixtures_dir):
    # After RedundantPhis, both arms feed a Phi resolving to either
    # IntConst(100) or IntConst(-50).  -50 lives at U32 width as
    # 0xffff_ffce on 32-bit archs and at U64 width as
    # 0xffff_ffff_ffff_ffce on x64 (zero-extended 32-bit move).
    # ARM-Thumb is a special case: gcc lowers the negative literal via
    # `mvnle r0, #49` (MVN + immediate 49), which lifts to
    # `IntUnaryOp::Neg(IntConst(49))` in IR.  ConstantFold then folds
    # that to `IntConst(0xffff_ffce)`, but only after Truncate +
    # KnownBits propagation — at the assertion point the constant may
    # appear either as 49 (pre-fold) or 0xffff_ffce (post-fold).
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
    assert count_pat(g, pat.if_()) >= 1
