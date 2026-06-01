//! Smoke tests for the predicate-guard chainers and the deferred
//! filter builders that depend on the widened `post_match` closure
//! signature.
//!
//! Covers: `Pat::when_match` (Wildcard coercion), `Pat::capture`,
//! `LoadPat::bit_width`, `PhiPat::for_vn`, `CallOtherPat::name`.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir_test_utils::{reg_vn, RegisterSet};
use strider_pattern::{
    any, call_other, int_const, load, phi, Capture, Matcher, Pat,
};

// ── Pat::when_match coerces to Wildcard ─────────────────────────────────────

#[test]
fn when_match_coerces_to_wildcard() {
    // The compiler-level proof: `int_const(5).when_match(...)` must
    // type-check as `Pat<Wildcard>`.  No runtime assertion needed —
    // this test passes iff the code compiles.
    let _p: Pat<strider_pattern::Wildcard> =
        int_const(5u128).when_match(|_ctx, _ty, _b| true);
}

// ── Pat::capture on a non-wildcard root ─────────────────────────────────────

#[test]
fn capture_on_pat_binds_root() {
    let mut b = RegisterSet::new().build_fn_single_region().unwrap();
    let five = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
    b.build_return(Some(five), &[]).unwrap();
    let function = b.build().unwrap();

    let c = Capture::new();
    let pat = int_const(5u128).capture(c);
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1, "exactly one IntConst(5) hit");
    let out = hits[0].output(c).expect("c must bind");
    assert!(matches!(
        function.kind_of_output(out),
        NodeKind::IntConst(5)
    ));
}

// ── Pat::cap (name-keyed capture) shares ids across same-name calls ─────────

#[test]
fn cap_interns_capture_by_name() {
    let a = Capture::named("x");
    let b = Capture::named("x");
    assert_eq!(a.id(), b.id(), "same name => same capture id");
    let other = Capture::named("y");
    assert_ne!(a.id(), other.id(), "different names => different ids");
}

// ── Pat::ordered disables commutative retry ─────────────────────────────────

#[test]
fn ordered_disables_commutative_retry() {
    use strider_pattern::add;
    let mut b = RegisterSet::new().build_fn_single_region().unwrap();
    let seven = b.build_int_const(7u64, NodeOutputType::I64).unwrap();
    let five = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
    // IR: Add(7, 5) — operand order is reversed relative to the
    // pattern's Add(IntConst(5), _).
    let sum = b
        .build_int_binary_operation(seven, five, strider_ir::IntBinaryOp::Add, NodeOutputType::I64)
        .unwrap();
    b.build_return(Some(sum), &[]).unwrap();
    let function = b.build().unwrap();

    // Without `.ordered()`: commutative retry catches the swap.
    let p = add(int_const(5u128), any());
    assert_eq!(
        Matcher::try_new(&function).unwrap().find_all(&p).len(),
        1,
        "commutative match should hit",
    );

    // With `.ordered()`: no retry, no hit (since the IR is Add(7, 5)).
    let p = add(int_const(5u128), any()).ordered();
    assert_eq!(
        Matcher::try_new(&function).unwrap().find_all(&p).len(),
        0,
        ".ordered() must disable commutative retry",
    );
}

// ── LoadPat::bit_width ──────────────────────────────────────────────────────

#[test]
fn load_bit_width_filters_by_value_width() {
    // Build a function with two loads of different widths sharing a
    // tracked base address.
    let base = reg_vn(0x40, 8);
    let mut b = RegisterSet::new()
        .tracked(base)
        .arg(base)
        .build_fn_single_region()
        .unwrap();
    let base_v = b.read_variable(&base).unwrap();
    let l32 = b
        .build_load(base_v, rsleigh::VnSpace::RAM, NodeOutputType::I32)
        .unwrap();
    let l64 = b
        .build_load(base_v, rsleigh::VnSpace::RAM, NodeOutputType::I64)
        .unwrap();
    // Combine both loads so each appears in the Return's reachable set.
    let l32_64 = b
        .extend_if_needed(l32, NodeOutputType::I64, strider_ir::ExtendOp::ZeroExtend)
        .unwrap();
    let combined = b
        .build_int_binary_operation(
            l32_64,
            l64,
            strider_ir::IntBinaryOp::Add,
            NodeOutputType::I64,
        )
        .unwrap();
    b.build_return(Some(combined), &[]).unwrap();
    let function = b.build().unwrap();

    let m = Matcher::try_new(&function).unwrap();
    let any_load: Pat<_> = load().into();
    assert_eq!(m.find_all(&any_load).len(), 2, "two reachable loads");

    let only_32: Pat<_> = load().bit_width(32).into();
    assert_eq!(
        m.find_all(&only_32).len(),
        1,
        "bit_width(32) filters to the I32 load",
    );

    let only_64: Pat<_> = load().bit_width(64).into();
    assert_eq!(
        m.find_all(&only_64).len(),
        1,
        "bit_width(64) filters to the I64 load",
    );
}

// ── PhiPat::for_vn ───────────────────────────────────────────────────────────

#[test]
fn phi_for_vn_matches_tagged_phi_only() {
    let rax = reg_vn(0, 8);
    let mut b = RegisterSet::new()
        .tracked(rax)
        .build_fn_single_region()
        .unwrap();
    let rax_val = b.read_variable(&rax).unwrap();
    b.build_return(Some(rax_val), &[]).unwrap();
    let function = b.build().unwrap();

    let m = Matcher::try_new(&function).unwrap();
    // Sanity: any phi matches.
    let any_phi: Pat<_> = phi().into();
    assert_eq!(m.find_all(&any_phi).len(), 1, "the tagged Phi exists");

    // for_vn(rax) matches it.
    let exact: Pat<_> = phi().for_vn(rax).into();
    assert_eq!(m.find_all(&exact).len(), 1, "phi().for_vn(rax) matches");

    // for_vn(different) doesn't.
    let wrong = reg_vn(0x100, 8);
    let mismatched: Pat<_> = phi().for_vn(wrong).into();
    assert!(
        m.find_all(&mismatched).is_empty(),
        "phi().for_vn(different) must not match",
    );
}

// ── CallOtherPat::name ──────────────────────────────────────────────────────

#[test]
fn call_other_name_filters_by_userop_name() {
    let mut b = RegisterSet::new().build_fn_single_region().unwrap();
    // Build a value-bearing CallOther via the modeled builder; the
    // `name` arg lands in the `Function::call_other_name` side-table.
    let user_op_id = 0x42_u64;
    b.build_call_other_modeled(
        user_op_id,
        "frobnicate",
        &[],
        Some(NodeOutputType::I64),
        &[],
        &[],
        &[],
    )
    .unwrap();
    let function = b.build().unwrap();

    let m = Matcher::try_new(&function).unwrap();
    // Sanity: any CallOther matches.
    let any_co: Pat<_> = call_other().into();
    assert_eq!(m.find_all(&any_co).len(), 1, "the modeled CallOther exists");

    // .name("frobnicate") matches.
    let named_hit: Pat<_> = call_other().name("frobnicate").into();
    assert_eq!(
        m.find_all(&named_hit).len(),
        1,
        ".name(\"frobnicate\") must match",
    );

    // .name("other") does not.
    let named_miss: Pat<_> = call_other().name("other").into();
    assert!(
        m.find_all(&named_miss).is_empty(),
        ".name(\"other\") must NOT match",
    );
}
