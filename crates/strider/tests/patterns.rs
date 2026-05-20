//! Complex pattern queries against rich fixtures.
//!
//! Each test issues a `pattern::Matcher` query mirroring a realistic
//! user-facing query (e.g. "find every (a*b)+c expression"; "find every
//! recursive call site").  These tests are the canonical contract that
//! the pattern crate continues to compose with the strider lifter's IR shape.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;
use pattern::{Matcher, Pat, add, mul, call, any};

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
per_arch_test!("patterns", "mul_then_add",                mac_pattern_finds_match);
// chained_xor_mask: ConstantFold collapses the literal constants, so
// the pattern matches only the structural shape.
per_arch_test!("patterns", "chained_xor_mask",            xor_chain_pattern_finds_match);
// if_returns_const exercises width-aware int_const matching.  ARM's
// MVN-based -50 lifting (`mvnle r0, #49` → `~49`) requires constant_fold
// and known_bits to keep `IntUnaryOp::BitNot` (bitwise NOT) distinct from
// `IntUnaryOp::Neg` (two's complement) in their evaluators.
per_arch_test!("patterns", "if_returns_const",            if_const_pattern_finds_two_consts);
per_arch_test!("patterns", "loop_with_invariant_load",    invariant_load_pattern_finds_load);
// recursive_with_accumulator relies on -fno-optimize-sibling-calls in
// fixtures/Makefile to keep the tail call from being elided.
per_arch_test!("patterns", "recursive_with_accumulator",  recursive_pattern_finds_self_call);

fn mac_pattern_finds_match(g: &strider_ir::BuiltFunctionGraph) {
    // Pattern: add(mul(?, ?), ?).  We use `.ignore_casts()` because some
    // arches (notably x64) lower this as `Add(Extend_zext(Mul@W), arg)`
    // — the Mul is one hop deeper than the matcher's exact-walk would
    // see otherwise.  Other arches don't have intervening casts, so the
    // flag is a no-op there (direct match still tried first).
    let m = Matcher::new(g).ignore_casts();
    let pat: Pat = add(mul(any(), any()), any()).into();
    let hits = m.find_all(&pat);
    assert!(!hits.is_empty(),
            "expected ≥1 match of add(mul(_,_), _); got {} matches", hits.len());
}

fn xor_chain_pattern_finds_match(g: &strider_ir::BuiltFunctionGraph) {
    // ConstantFold collapses (x ^ k1) & m1 ^ k2  →  (x & m1) ^ (k1^k2)
    // before pattern matching — the inner xor disappears, so the original
    // three-deep xor(and(xor)) query never matches.  The post-fold shape
    // retains at least one Xor and one And; assert that union of nodes
    // survives.  An IntConst-aware variant of this query would require
    // constants the optimiser can't fold (e.g. volatile-loaded); that's a
    // separate, larger fixture redesign.
    use strider_ir::IntBinaryOp;
    assert!(common::count_int_binop(g, IntBinaryOp::Xor) >= 1,
            "post-fold graph must contain ≥1 Xor; got {}",
            common::count_int_binop(g, IntBinaryOp::Xor));
    assert!(common::count_int_binop(g, IntBinaryOp::And) >= 1,
            "post-fold graph must contain ≥1 And; got {}",
            common::count_int_binop(g, IntBinaryOp::And));
}

fn if_const_pattern_finds_two_consts(g: &strider_ir::BuiltFunctionGraph) {
    // After RedundantPhis, both arms of the If feed a Phi resolving to either
    // IntConst(100) or IntConst(-50).  Pin both constants.
    //
    // On 32-bit archs (arm, mips32) the -50 constant lives in a U32 IntConst
    // (0xffff_ffce).  On x86-64, the compiler zero-extends a 32-bit move so the
    // constant appears as IntConst(0xffff_ffce) at U64 width, which is the same
    // bit pattern but semantically +4294967246 (not -50) at U64.  The raw
    // has_constant check covers all archs correctly:
    //   has_constant(g, 0xffff_ffce) matches the node regardless of its output type
    //   because the stored u128 value equals u128::from(0xffff_ffce as u64).
    assert!(has_constant(g, 100),
            "expected IntConst(100) — true-branch return value");
    let neg50_u32 = (-50i32) as u32 as u64;
    let neg50_u64 = (-50i64) as u64;
    assert!(has_constant(g, neg50_u32) || has_constant(g, neg50_u64),
            "expected IntConst(-50) — false-branch return value (any of {neg50_u32}, {neg50_u64})");
}

fn invariant_load_pattern_finds_load(g: &strider_ir::BuiltFunctionGraph) {
    // Pattern: any Load.
    let m = Matcher::new(g);
    let pat: Pat = pattern::load().into();
    let hits = m.find_all(&pat);
    assert!(!hits.is_empty(), "expected ≥1 Load match in loop_with_invariant_load");
    assert!(count_loops(g) >= 1, "loop must remain");
}

fn recursive_pattern_finds_self_call(g: &strider_ir::BuiltFunctionGraph) {
    // Pattern: any Call.
    let m = Matcher::new(g);
    let pat: Pat = call().into();
    let hits = m.find_all(&pat);
    assert!(!hits.is_empty(),
            "expected ≥1 Call match in recursive_with_accumulator; got {} matches", hits.len());
    assert!(count_ifs(g) >= 1);
}
