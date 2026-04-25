//! Complex pattern queries against rich fixtures.
//!
//! Each test issues a `pattern::Matcher` query mirroring a realistic
//! user-facing query (e.g. "find every (a*b)+c expression"; "find every
//! recursive call site").  These tests are the canonical contract that
//! the pattern crate continues to compose with the analyzer's IR shape.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;
use pattern::{Matcher, Pat, add, mul, call, any};

per_arch_test!("patterns", "mul_then_add",                mac_pattern_finds_match);
per_arch_test!("patterns", "chained_xor_mask",            xor_chain_pattern_finds_match);
per_arch_test!("patterns", "if_returns_const",            if_const_pattern_finds_two_consts);
per_arch_test!("patterns", "loop_with_invariant_load",    invariant_load_pattern_finds_load);
per_arch_test!("patterns", "recursive_with_accumulator",  recursive_pattern_finds_self_call);

fn mac_pattern_finds_match(g: &ir::BuiltFunctionGraph) {
    // Pattern: add(mul(?, ?), ?)
    let m = Matcher::new(g);
    let pat: Pat = add(mul(any(), any()), any()).into();
    let hits = m.find_all(&pat);
    assert!(!hits.is_empty(),
            "expected ≥1 match of add(mul(_,_), _); got {} matches", hits.len());
}

fn xor_chain_pattern_finds_match(g: &ir::BuiltFunctionGraph) {
    // Pattern: xor(and(xor(?, c), c), c) — three-deep xor/and chain.
    use pattern::{xor, and, any_int_const, Var};
    let m = Matcher::new(g);
    let pat: Pat = xor(
        and(
            xor(any(), any_int_const(Var::new())),
            any_int_const(Var::new()),
        ),
        any_int_const(Var::new()),
    ).into();
    let hits = m.find_all(&pat);
    assert!(!hits.is_empty(),
            "expected ≥1 match of xor(and(xor(_,c), c), c); got {} matches", hits.len());
}

fn if_const_pattern_finds_two_consts(g: &ir::BuiltFunctionGraph) {
    // After RedundantPhis, both arms of the If feed a Phi resolving to either
    // IntConst(100) or IntConst(-50 as u64).  Pin both constants.
    assert!(has_constant(g, 100),
            "expected IntConst(100) — true-branch return value");
    let neg50_u32 = (-50i32) as u32 as u64;
    let neg50_u64 = (-50i64) as u64;
    assert!(has_constant(g, neg50_u32) || has_constant(g, neg50_u64),
            "expected IntConst(-50) — false-branch return value (any of {neg50_u32}, {neg50_u64})");
}

fn invariant_load_pattern_finds_load(g: &ir::BuiltFunctionGraph) {
    // Pattern: any Load.
    let m = Matcher::new(g);
    let pat: Pat = pattern::load().into();
    let hits = m.find_all(&pat);
    assert!(!hits.is_empty(), "expected ≥1 Load match in loop_with_invariant_load");
    assert!(count_loops(g) >= 1, "loop must remain");
}

fn recursive_pattern_finds_self_call(g: &ir::BuiltFunctionGraph) {
    // Pattern: any Call.
    let m = Matcher::new(g);
    let pat: Pat = call().into();
    let hits = m.find_all(&pat);
    assert!(!hits.is_empty(),
            "expected ≥1 Call match in recursive_with_accumulator; got {} matches", hits.len());
    assert!(count_ifs(g) >= 1);
}
