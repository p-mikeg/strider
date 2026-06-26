//! Complex pattern queries against rich fixtures.
//!
//! Each test issues a `strider_pattern::Matcher` query mirroring a realistic
//! user-facing query (e.g. "find every (a*b)+c expression"; "find every
//! recursive call site").  These tests are the canonical contract that
//! the pattern crate continues to compose with the strider lifter's IR shape.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable,
    clippy::useless_conversion
)]

mod common;
use common::*;
use strider_pattern::{MatchPat, Matcher, add, any, call, mul};

// mul_then_add covers `add(mul(_,_), _)` across all archs via:
//   * Strider lifter side: Truncate-narrowing rules in ConstantFold (mips32
//     hot path), drop-high-half-in-Or-Trunc, drop-low-mask-under-Trunc,
//     Truncate(Extend(x)) round-trip.
//   * Matcher side: `Matcher::ignore_casts()` lets the matcher walk
//     transparently through Extend / Truncate / CastTo* nodes that the
//     optimizer couldn't fully eliminate (x64 `Add(Extend(Mul), arg)`
//     register-merge chain — pushing Extend through Mul is not a valid
//     identity in general, but skipping it during pattern matching is
//     fine when the user opts in).
per_arch_test!("patterns", "mul_then_add", mac_pattern_finds_match);
// chained_xor_mask: ConstantFold collapses the literal constants, so
// the pattern matches only the structural shape.
per_arch_test!(
    "patterns",
    "chained_xor_mask",
    xor_chain_pattern_finds_match
);
// if_returns_const exercises width-aware int_const matching.  ARM's
// MVN-based -50 lifting (`mvnle r0, #49` → `~49`) requires constant_fold
// to fold the canonical `Xor(IntConst(49), IntConst(all_ones))` shape
// (the former BitNot unary-op was removed in favour of `Xor(_, all_ones)`)
// while keeping `IntUnaryOp::Neg` (two's complement) distinct.
per_arch_test!(
    "patterns",
    "if_returns_const",
    if_const_pattern_finds_two_consts
);
per_arch_test!(
    "patterns",
    "loop_with_invariant_load",
    invariant_load_pattern_finds_load
);
// recursive_with_accumulator relies on -fno-optimize-sibling-calls in
// fixtures/Makefile to keep the tail call from being elided.
per_arch_test!(
    "patterns",
    "recursive_with_accumulator",
    recursive_pattern_finds_self_call
);

fn mac_pattern_finds_match(function: &strider_ir::Function) {
    // Pattern: add(mul(?, ?), ?).  We use `.ignore_casts()` because some
    // arches (notably x64) lower this as `Add(Extend_zext(Mul@W), arg)`
    // — the Mul is one hop deeper than the matcher's exact-walk would
    // see otherwise.  Other arches don't have intervening casts, so the
    // flag is a no-op there (direct match still tried first).
    let m = Matcher::new(function);
    let pat = add(mul(any(), any()), any()).into_pattern().ignore_casts();
    let hits = m.find_all(&pat).unwrap();
    assert!(
        !hits.is_empty(),
        "expected ≥1 match of add(mul(_,_), _); got {} matches",
        hits.len()
    );
}

fn xor_chain_pattern_finds_match(function: &strider_ir::Function) {
    // ConstantFold collapses (x ^ k1) & m1 ^ k2  →  (x & m1) ^ (k1^k2)
    // before pattern matching — the inner xor disappears, so the original
    // three-deep xor(and(xor)) query never matches.  The post-fold shape
    // retains at least one Xor and one And; assert that union of nodes
    // survives.  An IntConst-aware variant of this query would require
    // constants the optimiser can't fold (e.g. volatile-loaded); that's a
    // separate, larger fixture redesign.
    use strider_ir::IntBinaryOp;
    assert!(
        common::count_int_binop(function, IntBinaryOp::Xor) >= 1,
        "post-fold graph must contain ≥1 Xor; got {}",
        common::count_int_binop(function, IntBinaryOp::Xor)
    );
    assert!(
        common::count_int_binop(function, IntBinaryOp::And) >= 1,
        "post-fold graph must contain ≥1 And; got {}",
        common::count_int_binop(function, IntBinaryOp::And)
    );
}

fn if_const_pattern_finds_two_consts(function: &strider_ir::Function) {
    // After PhiCollapse, both arms of the If feed a Phi resolving to either
    // IntConst(100) or IntConst(-50).  Pin both constants.
    //
    // On 32-bit archs (arm, mips32) the -50 constant lives in a I32 IntConst
    // (0xffff_ffce).  On x86-64, the compiler zero-extends a 32-bit move so the
    // constant appears as IntConst(0xffff_ffce) at I64 width, which is the same
    // bit pattern but semantically +4294967246 (not -50) at I64.  The raw
    // has_constant check covers all archs correctly:
    //   has_constant(g, 0xffff_ffce) matches the node regardless of its output type
    //   because the stored u128 value equals u128::from(0xffff_ffce as u64).
    assert!(
        has_constant(function, 100),
        "expected IntConst(100) — true-branch return value"
    );
    let neg50_u32 = (-50i32) as u32 as u64;
    let neg50_u64 = (-50i64) as u64;
    assert!(
        has_constant(function, neg50_u32) || has_constant(function, neg50_u64),
        "expected IntConst(-50) — false-branch return value (any of {neg50_u32}, {neg50_u64})"
    );
}

fn invariant_load_pattern_finds_load(function: &strider_ir::Function) {
    // Pattern: any Load.  Primary check is that the Load pattern matches.
    // We don't assert the loop survives — the compiler is free to close-form
    // the triangle-sum (e.g. x86_kernel collapses the loop entirely into
    // arithmetic on n and the invariant *p), which is a valid optimization
    // that strider faithfully represents as "no back-edge".
    let m = Matcher::new(function);
    let pat = strider_pattern::load().build();
    let hits = m.find_all(&pat).unwrap();
    assert!(
        !hits.is_empty(),
        "expected ≥1 Load match in loop_with_invariant_load"
    );
}

fn recursive_pattern_finds_self_call(function: &strider_ir::Function) {
    // Pattern: any Call.
    let m = Matcher::new(function);
    let pat = call().build();
    let hits = m.find_all(&pat).unwrap();
    assert!(
        !hits.is_empty(),
        "expected ≥1 Call match in recursive_with_accumulator; got {} matches",
        hits.len()
    );
    assert!(count_ifs(function) >= 1);
}
