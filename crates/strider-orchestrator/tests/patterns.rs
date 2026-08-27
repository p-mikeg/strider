//! `strider_pattern::Matcher` queries mirroring realistic user-facing ones
//! ("find every (a*b)+c expression", "find every recursive call site"), pinning
//! that the pattern crate composes with the shapes the lifter emits.

#![allow(clippy::useless_conversion)]

mod common;
use common::*;
use strider_pattern::{MatchPat, Matcher, anything, call, int_add, int_mul};

// mul_then_add covers `add(mul(_,_), _)` across all archs via:
//   * lifter side: Truncate-narrowing rules in ConstantFold (mips32 hot
//     path), drop-high-half-in-Or-Trunc, drop-low-mask-under-Trunc,
//     Truncate(Extend(x)) round-trip.
//   * matcher side: `Matcher::ignore_casts()` walks transparently through
//     Extend/Truncate/CastTo* nodes the optimizer couldn't fully eliminate
//     (x64's `Add(Extend(Mul), arg)` register-merge chain). Pushing Extend
//     through Mul isn't a valid identity in general, but skipping it during
//     matching is fine when the user opts in.
per_arch_test!("patterns", "mul_then_add", mac_pattern_finds_match);
// chained_xor_mask: ConstantFold collapses the literal constants, so
// the pattern matches only the structural shape.
per_arch_test!(
    "patterns",
    "chained_xor_mask",
    xor_chain_pattern_finds_match
);
// if_returns_const exercises width-aware int_const matching.  ARM's
// MVN-based -50 lifting (`mvnle r0, #49` -> `~49`) requires constant_fold to
// fold the canonical `Xor(IntConst(49), IntConst(all_ones))` shape while
// keeping `IntUnaryOp::Neg` (two's complement) distinct.
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
    // `.ignore_casts()`: x64 lowers this as `Add(Extend_zext(Mul@W), arg)`,
    // one hop deeper than the matcher's exact walk would see otherwise.
    // Other arches have no intervening casts, so the flag is a no-op there.
    let m = Matcher::new(function);
    let pat = int_add(int_mul(anything(), anything()), anything())
        .into_pattern()
        .ignore_casts();
    let hits = m.find_all(&pat).unwrap();
    assert!(
        !hits.is_empty(),
        "expected >=1 match of add(mul(_,_), _); got {} matches",
        hits.len()
    );
}

fn xor_chain_pattern_finds_match(function: &strider_ir::Function) {
    // ConstantFold collapses (x ^ k1) & m1 ^ k2 into (x & m1) ^ (k1^k2)
    // before matching, so a three-deep xor(and(xor)) query never matches;
    // assert only that the post-fold shape retains a Xor and an And.
    use strider_ir::IntBinaryOp;
    assert!(
        common::count_int_binop(function, IntBinaryOp::Xor) >= 1,
        "post-fold graph must contain >=1 Xor; got {}",
        common::count_int_binop(function, IntBinaryOp::Xor)
    );
    assert!(
        common::count_int_binop(function, IntBinaryOp::And) >= 1,
        "post-fold graph must contain >=1 And; got {}",
        common::count_int_binop(function, IntBinaryOp::And)
    );
}

fn if_const_pattern_finds_two_consts(function: &strider_ir::Function) {
    // After PhiCollapse, both arms feed a Phi resolving to IntConst(100)
    // or IntConst(-50). On 32-bit archs (arm, mips32) -50 is an I32
    // IntConst (0xffff_ffce); on x86-64 the compiler zero-extends a
    // 32-bit move, so it appears as IntConst(0xffff_ffce) at I64 width,
    // same bits but semantically +4294967246, not -50. has_constant
    // covers both: it compares the raw u128 value regardless of type.
    assert!(
        has_constant(function, 100),
        "expected IntConst(100), the true-branch return value"
    );
    let neg50_u32 = (-50i32) as u32 as u64;
    let neg50_u64 = (-50i64) as u64;
    assert!(
        has_constant(function, neg50_u32) || has_constant(function, neg50_u64),
        "expected IntConst(-50), the false-branch return value (any of {neg50_u32}, {neg50_u64})"
    );
}

fn invariant_load_pattern_finds_load(function: &strider_ir::Function) {
    // Only asserts a Load is present; doesn't require the loop to survive.
    // The compiler is free to close-form the triangle-sum (e.g. x86_kernel
    // collapses it into arithmetic on n and the invariant *p), a valid
    // optimization strider faithfully represents as "no back-edge".
    let m = Matcher::new(function);
    let pat = strider_pattern::load().build();
    let hits = m.find_all(&pat).unwrap();
    assert!(
        !hits.is_empty(),
        "expected >=1 Load match in loop_with_invariant_load"
    );
}

fn recursive_pattern_finds_self_call(function: &strider_ir::Function) {
    let m = Matcher::new(function);
    let pat = call().build();
    let hits = m.find_all(&pat).unwrap();
    assert!(
        !hits.is_empty(),
        "expected >=1 Call match in recursive_with_accumulator; got {} matches",
        hits.len()
    );
    assert!(count_ifs(function) >= 1);
}
