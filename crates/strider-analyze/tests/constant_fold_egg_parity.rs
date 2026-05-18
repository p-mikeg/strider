//! Phase 3 Task 3.2 parity test.
//!
//! For every constant-evaluable integer binary / unary / comparison op,
//! v1's [`ConstantFold`] (imperative, pattern-rewrite-based) and v2's
//! [`ConstantFoldEgg`] (egg-based) MUST produce structurally identical
//! IR.
//!
//! Scope (Phase 3.2 first cut): pure constant evaluation only.
//!   IntConst(a) OP IntConst(b) → IntConst(eval(OP, a, b))
//! Identity rewrites (`x + 0 → x` etc.) are deferred to follow-up
//! commits; this test fixture intentionally feeds *only* constant-pair
//! shapes so identity rules cannot fire.
//!
//! Structural comparison: both pipelines must collapse the test fixture
//! down to a single return-value node whose `NodeKind` matches the
//! oracle (computed independently via `eval_int_binary` / `eval_int_cmp`
//! style logic embedded in the assertions below).
//!
//! Note on bool results: integer comparisons produce a `Bool`-typed
//! `IntConst(0|1)` after v1's fold, but the v1 path stores it as
//! `BoolConst` since comparisons emit `NodeOutputType::Bool` and the
//! const-eval rule for IntCmp goes through `bool_const_with!`.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use strider_analyze::opt::{ConstantFold, OptimizerRaw, constant_fold_egg::ConstantFoldEgg};
use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir::test_utils::make_empty_fn;
use strider_ir::{
    BuiltFunctionGraph, IntBinaryOp, IntCmpOp, IntUnaryOp,
};

/// Helper: builds a function whose return-value is `IntBinaryOp(op)(a, b)`
/// at `ty`, then runs `optimizer` to fixed point, and returns the final
/// `NodeKind` of the return value.
fn fold_binary_to_kind(
    optimizer: &dyn OptimizerRaw,
    op: IntBinaryOp,
    a: u128,
    b: u128,
    ty: NodeOutputType,
) -> NodeKind {
    let mut fg = make_empty_fn(|builder| {
        let ca = builder.build_int_const(a, ty)?;
        let cb = builder.build_int_const(b, ty)?;
        builder.build_int_binary_operation(ca, cb, op, ty)
    })
    .expect("build test fn");
    run_to_fixed_point(optimizer, &mut fg);
    return_kind(&fg)
}

fn fold_unary_to_kind(
    optimizer: &dyn OptimizerRaw,
    op: IntUnaryOp,
    a: u128,
    ty: NodeOutputType,
) -> NodeKind {
    let mut fg = make_empty_fn(|builder| {
        let ca = builder.build_int_const(a, ty)?;
        builder.build_int_unary_operation(ca, op, ty)
    })
    .expect("build test fn");
    run_to_fixed_point(optimizer, &mut fg);
    return_kind(&fg)
}

fn fold_cmp_to_kind(
    optimizer: &dyn OptimizerRaw,
    op: IntCmpOp,
    a: u128,
    b: u128,
    ty: NodeOutputType,
) -> NodeKind {
    let mut fg = make_empty_fn(|builder| {
        let ca = builder.build_int_const(a, ty)?;
        let cb = builder.build_int_const(b, ty)?;
        builder.build_int_cmp_operation(ca, cb, op, ty)
    })
    .expect("build test fn");
    run_to_fixed_point(optimizer, &mut fg);
    return_kind(&fg)
}

fn run_to_fixed_point(optimizer: &dyn OptimizerRaw, fg: &mut BuiltFunctionGraph) {
    // Run repeatedly until no change.  Bound the iteration count so a
    // bug can't hang the test indefinitely.
    let mut steps = 0;
    loop {
        let result = optimizer
            .optimize_raw(&mut fg.graph, fg.entry)
            .expect("optimize_raw must not error on synthetic const-only fixture");
        if !result.changed() {
            break;
        }
        steps += 1;
        assert!(steps < 32, "optimizer failed to reach fixed point");
    }
}

/// Returns the `NodeKind` of the return-value producer.
fn return_kind(fg: &BuiltFunctionGraph) -> NodeKind {
    // Find the unique Return node, inspect its value input (index 2:
    // [ctrl, mem, value]).
    let ret = fg
        .graph
        .all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
        .expect("function must have a Return node");
    let inputs = fg.graph.node_inputs(ret);
    let val_out = inputs[2];
    let producer = fg.graph.get_node_from_output(val_out);
    *fg.graph.node_kind(producer)
}

/// Assert the parity for one binary op + operand pair.
fn assert_binary_parity(op: IntBinaryOp, a: u128, b: u128, ty: NodeOutputType) {
    let v1 = fold_binary_to_kind(&ConstantFold, op, a, b, ty);
    let v2 = fold_binary_to_kind(&ConstantFoldEgg::new(), op, a, b, ty);
    assert_eq!(
        v1, v2,
        "binary parity failed: op={op:?} a={a:#x} b={b:#x} ty={ty:?}: v1={v1:?} v2={v2:?}"
    );
    // Sanity: both should be IntConst (this is a pure const-eval case).
    assert!(
        matches!(v1, NodeKind::IntConst(_)),
        "v1 didn't fold to IntConst for op={op:?} a={a:#x} b={b:#x} ty={ty:?}: got {v1:?}"
    );
}

fn assert_unary_parity(op: IntUnaryOp, a: u128, ty: NodeOutputType) {
    let v1 = fold_unary_to_kind(&ConstantFold, op, a, ty);
    let v2 = fold_unary_to_kind(&ConstantFoldEgg::new(), op, a, ty);
    assert_eq!(
        v1, v2,
        "unary parity failed: op={op:?} a={a:#x} ty={ty:?}: v1={v1:?} v2={v2:?}"
    );
    assert!(
        matches!(v1, NodeKind::IntConst(_)),
        "v1 didn't fold to IntConst for op={op:?} a={a:#x} ty={ty:?}: got {v1:?}"
    );
}

fn assert_cmp_parity(op: IntCmpOp, a: u128, b: u128, ty: NodeOutputType) {
    let v1 = fold_cmp_to_kind(&ConstantFold, op, a, b, ty);
    let v2 = fold_cmp_to_kind(&ConstantFoldEgg::new(), op, a, b, ty);
    assert_eq!(
        v1, v2,
        "cmp parity failed: op={op:?} a={a:#x} b={b:#x} ty={ty:?}: v1={v1:?} v2={v2:?}"
    );
    assert!(
        matches!(v1, NodeKind::BoolConst(_)),
        "v1 didn't fold to BoolConst for op={op:?} a={a:#x} b={b:#x} ty={ty:?}: got {v1:?}"
    );
}

// ── IntBinaryOp parity tests ─────────────────────────────────────────────────

#[test]
fn parity_int_add() {
    assert_binary_parity(IntBinaryOp::Add, 3, 4, NodeOutputType::U64);
    assert_binary_parity(IntBinaryOp::Add, 100, 200, NodeOutputType::U32);
    assert_binary_parity(IntBinaryOp::Add, u128::MAX & 0xFFFF_FFFF, 1, NodeOutputType::U32);
}

#[test]
fn parity_int_mul() {
    assert_binary_parity(IntBinaryOp::Mul, 6, 7, NodeOutputType::U64);
    assert_binary_parity(IntBinaryOp::Mul, 0x100, 0x100, NodeOutputType::U32);
}

#[test]
fn parity_int_and() {
    assert_binary_parity(IntBinaryOp::And, 0xFF, 0xF0, NodeOutputType::U64);
    assert_binary_parity(IntBinaryOp::And, 0xCAFEBABE, 0xDEADBEEF, NodeOutputType::U32);
}

#[test]
fn parity_int_or() {
    assert_binary_parity(IntBinaryOp::Or, 0x0F, 0xF0, NodeOutputType::U64);
    assert_binary_parity(IntBinaryOp::Or, 0, 0x42, NodeOutputType::U32);
}

#[test]
fn parity_int_xor() {
    assert_binary_parity(IntBinaryOp::Xor, 0xAA, 0x55, NodeOutputType::U64);
    assert_binary_parity(IntBinaryOp::Xor, 0x123456, 0x654321, NodeOutputType::U32);
}

#[test]
fn parity_int_shl() {
    assert_binary_parity(IntBinaryOp::ShiftLeft, 1, 4, NodeOutputType::U64);
    assert_binary_parity(IntBinaryOp::ShiftLeft, 0xFF, 8, NodeOutputType::U32);
    // Sleigh semantics: shift >= bit_width → 0
    assert_binary_parity(IntBinaryOp::ShiftLeft, 1, 64, NodeOutputType::U64);
}

#[test]
fn parity_int_shr_unsigned() {
    assert_binary_parity(IntBinaryOp::ShiftRight, 0x100, 4, NodeOutputType::U64);
    assert_binary_parity(IntBinaryOp::ShiftRight, 0x8000_0000, 31, NodeOutputType::U32);
    assert_binary_parity(IntBinaryOp::ShiftRight, 1, 64, NodeOutputType::U64);
}

#[test]
fn parity_int_shr_signed() {
    // Positive value: should look like logical shr.
    assert_binary_parity(IntBinaryOp::SShiftRight, 0x100, 4, NodeOutputType::U64);
    // Negative value (sign-extended): sshr fills with sign bit.
    let neg_one_u32 = 0xFFFF_FFFFu128;
    assert_binary_parity(IntBinaryOp::SShiftRight, neg_one_u32, 4, NodeOutputType::U32);
    // Shift >= bit_width: signbit-set → mask; signbit-clear → 0
    assert_binary_parity(IntBinaryOp::SShiftRight, neg_one_u32, 64, NodeOutputType::U32);
    assert_binary_parity(IntBinaryOp::SShiftRight, 0x7FFF_FFFF, 64, NodeOutputType::U32);
}

// ── IntUnaryOp parity tests ──────────────────────────────────────────────────

#[test]
fn parity_int_bit_not() {
    assert_unary_parity(IntUnaryOp::BitNot, 0, NodeOutputType::U64);
    assert_unary_parity(IntUnaryOp::BitNot, 0xCAFE, NodeOutputType::U32);
}

#[test]
fn parity_int_neg() {
    assert_unary_parity(IntUnaryOp::Neg, 1, NodeOutputType::U64);
    assert_unary_parity(IntUnaryOp::Neg, 0xFF, NodeOutputType::U32);
    assert_unary_parity(IntUnaryOp::Neg, 0, NodeOutputType::U64);
}

// ── IntCmpOp parity tests ────────────────────────────────────────────────────

#[test]
fn parity_int_cmp_equal() {
    assert_cmp_parity(IntCmpOp::Equal, 5, 5, NodeOutputType::U64);
    assert_cmp_parity(IntCmpOp::Equal, 5, 6, NodeOutputType::U64);
}

#[test]
fn parity_int_cmp_less() {
    assert_cmp_parity(IntCmpOp::Less, 5, 6, NodeOutputType::U64);
    assert_cmp_parity(IntCmpOp::Less, 6, 5, NodeOutputType::U64);
    assert_cmp_parity(IntCmpOp::Less, 5, 5, NodeOutputType::U32);
}

#[test]
fn parity_int_cmp_sless() {
    // Signed: -1 < 0
    let neg_one_u32 = 0xFFFF_FFFFu128;
    assert_cmp_parity(IntCmpOp::Sless, neg_one_u32, 0, NodeOutputType::U32);
    // Signed: 1 < -1 = false (1 < (-1) signed is false)
    assert_cmp_parity(IntCmpOp::Sless, 1, neg_one_u32, NodeOutputType::U32);
}
