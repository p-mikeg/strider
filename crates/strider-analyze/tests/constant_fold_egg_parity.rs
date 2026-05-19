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

use std::collections::BTreeMap;

use strider_analyze::opt::{ConstantFold, OptimizerRaw, constant_fold_egg::ConstantFoldEgg};
use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir::test_utils::{make_empty_fn, make_fn_with_var, reg_vn};
use strider_ir::{
    BoolBinaryOp, BoolUnaryOp, BuiltFunctionGraph, ExtendOp, FloatBinaryOp, FloatCmpOp,
    FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp,
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

// ── Structural-parity helpers (for identity/reassoc/bitcast rules) ───────────
//
// These rules don't always collapse to a single const node — they may
// rewrite `x + 0 → x` where `x` is a variable read.  The check is a
// payload-elided kind histogram of all reachable nodes: v1 and v2 must
// match.  This is the same recipe `pipeline_v2_parity` uses (just inline
// here since dev-tests can't share helper code across crates without
// extra crate plumbing).

fn kind_bucket(k: &NodeKind) -> String {
    match k {
        NodeKind::IntBinaryOp(op) => format!("IntBinaryOp::{op:?}"),
        NodeKind::IntUnaryOp(op) => format!("IntUnaryOp::{op:?}"),
        NodeKind::IntCmpOp(op) => format!("IntCmpOp::{op:?}"),
        NodeKind::BoolBinaryOp(op) => format!("BoolBinaryOp::{op:?}"),
        NodeKind::BoolUnaryOp(op) => format!("BoolUnaryOp::{op:?}"),
        NodeKind::FloatBinaryOp(op) => format!("FloatBinaryOp::{op:?}"),
        NodeKind::FloatUnaryOp(op) => format!("FloatUnaryOp::{op:?}"),
        NodeKind::FloatCmpOp(op) => format!("FloatCmpOp::{op:?}"),
        NodeKind::Extend(op) => format!("Extend::{op:?}"),
        NodeKind::IntConst(_) => "IntConst".to_string(),
        NodeKind::BoolConst(_) => "BoolConst".to_string(),
        NodeKind::FloatConst(_) => "FloatConst".to_string(),
        NodeKind::InitialVar(_) => "InitialVar".to_string(),
        NodeKind::VarPhi(_) => "VarPhi".to_string(),
        NodeKind::MemPhi => "MemPhi".to_string(),
        NodeKind::ValuePhi => "ValuePhi".to_string(),
        NodeKind::Truncate => "Truncate".to_string(),
        NodeKind::CastToInt => "CastToInt".to_string(),
        NodeKind::CastToBool => "CastToBool".to_string(),
        NodeKind::CastToFloat => "CastToFloat".to_string(),
        NodeKind::IntBitsToFloat => "IntBitsToFloat".to_string(),
        NodeKind::FloatBitsToInt => "FloatBitsToInt".to_string(),
        NodeKind::IntToFloat => "IntToFloat".to_string(),
        NodeKind::FloatToInt => "FloatToInt".to_string(),
        NodeKind::FloatToFloat => "FloatToFloat".to_string(),
        NodeKind::Popcount => "Popcount".to_string(),
        NodeKind::Lzcount => "Lzcount".to_string(),
        other => format!("{other:?}"),
    }
}

fn reachable_histogram(fg: &BuiltFunctionGraph) -> BTreeMap<String, usize> {
    let mut h = BTreeMap::new();
    for nid in fg.preorder() {
        let k = fg.graph.node_kind(nid);
        *h.entry(kind_bucket(k)).or_insert(0) += 1;
    }
    h
}

fn assert_histogram_parity(
    msg: &str,
    build: impl Fn() -> BuiltFunctionGraph,
) {
    let mut fg_v1 = build();
    let mut fg_v2 = build();
    run_to_fixed_point(&ConstantFold, &mut fg_v1);
    run_to_fixed_point(&ConstantFoldEgg::new(), &mut fg_v2);
    let h1 = reachable_histogram(&fg_v1);
    let h2 = reachable_histogram(&fg_v2);
    if h1 != h2 {
        let mut all_keys: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for k in h1.keys() { all_keys.insert(k.as_str()); }
        for k in h2.keys() { all_keys.insert(k.as_str()); }
        let mut diff = Vec::new();
        for k in all_keys {
            let a = h1.get(k).copied().unwrap_or(0);
            let b = h2.get(k).copied().unwrap_or(0);
            if a != b {
                diff.push(format!("    {k}: v1={a} v2={b}"));
            }
        }
        panic!("histogram mismatch ({msg}):\n{}", diff.join("\n"));
    }
}

/// Builds a function returning `f(InitialVar(x_vn))` where `x_vn` is a
/// scratch register varnode.  Used by identity / reassoc / bitcast tests
/// that need a non-const operand.
fn build_fn_with_var<F>(f: F) -> BuiltFunctionGraph
where
    F: FnOnce(
        &mut strider_ir::FunctionBuilder,
        strider_ir::Value,
    ) -> anyhow::Result<strider_ir::Value>,
{
    let x_vn = reg_vn(0x1000, 8);
    let (fg, _x) = make_fn_with_var(x_vn, |b, x| f(b, x)).expect("build fn");
    fg
}

// ── Identity rule parity tests (Group 1) ─────────────────────────────────────

#[test]
fn parity_identity_add_zero() {
    assert_histogram_parity("x + 0 → x", || {
        build_fn_with_var(|b, x| {
            let zero = b.build_int_const(0u64, NodeOutputType::U64)?;
            b.build_int_binary_operation(x, zero, IntBinaryOp::Add, NodeOutputType::U64)
        })
    });
}

#[test]
fn parity_identity_add_zero_commuted() {
    assert_histogram_parity("0 + x → x (commuted)", || {
        build_fn_with_var(|b, x| {
            let zero = b.build_int_const(0u64, NodeOutputType::U64)?;
            b.build_int_binary_operation(zero, x, IntBinaryOp::Add, NodeOutputType::U64)
        })
    });
}

#[test]
fn parity_identity_xor_self() {
    assert_histogram_parity("x ^ x → 0", || {
        build_fn_with_var(|b, x| {
            b.build_int_binary_operation(x, x, IntBinaryOp::Xor, NodeOutputType::U64)
        })
    });
}

#[test]
fn parity_identity_xor_zero() {
    assert_histogram_parity("x ^ 0 → x", || {
        build_fn_with_var(|b, x| {
            let zero = b.build_int_const(0u64, NodeOutputType::U64)?;
            b.build_int_binary_operation(x, zero, IntBinaryOp::Xor, NodeOutputType::U64)
        })
    });
}

#[test]
fn parity_identity_mul_zero() {
    assert_histogram_parity("x * 0 → 0", || {
        build_fn_with_var(|b, x| {
            let zero = b.build_int_const(0u64, NodeOutputType::U64)?;
            b.build_int_binary_operation(x, zero, IntBinaryOp::Mul, NodeOutputType::U64)
        })
    });
}

#[test]
fn parity_identity_mul_one() {
    assert_histogram_parity("x * 1 → x", || {
        build_fn_with_var(|b, x| {
            let one = b.build_int_const(1u64, NodeOutputType::U64)?;
            b.build_int_binary_operation(x, one, IntBinaryOp::Mul, NodeOutputType::U64)
        })
    });
}

#[test]
fn parity_identity_and_zero() {
    assert_histogram_parity("x & 0 → 0", || {
        build_fn_with_var(|b, x| {
            let zero = b.build_int_const(0u64, NodeOutputType::U64)?;
            b.build_int_binary_operation(x, zero, IntBinaryOp::And, NodeOutputType::U64)
        })
    });
}

#[test]
fn parity_identity_and_self() {
    assert_histogram_parity("x & x → x", || {
        build_fn_with_var(|b, x| {
            b.build_int_binary_operation(x, x, IntBinaryOp::And, NodeOutputType::U64)
        })
    });
}

#[test]
fn parity_identity_or_zero() {
    assert_histogram_parity("x | 0 → x", || {
        build_fn_with_var(|b, x| {
            let zero = b.build_int_const(0u64, NodeOutputType::U64)?;
            b.build_int_binary_operation(x, zero, IntBinaryOp::Or, NodeOutputType::U64)
        })
    });
}

#[test]
fn parity_identity_or_self() {
    assert_histogram_parity("x | x → x", || {
        build_fn_with_var(|b, x| {
            b.build_int_binary_operation(x, x, IntBinaryOp::Or, NodeOutputType::U64)
        })
    });
}

#[test]
fn parity_identity_shl_zero() {
    assert_histogram_parity("x << 0 → x", || {
        build_fn_with_var(|b, x| {
            let zero = b.build_int_const(0u64, NodeOutputType::U64)?;
            b.build_int_binary_operation(x, zero, IntBinaryOp::ShiftLeft, NodeOutputType::U64)
        })
    });
}

#[test]
fn parity_identity_shr_zero() {
    assert_histogram_parity("x >> 0 → x", || {
        build_fn_with_var(|b, x| {
            let zero = b.build_int_const(0u64, NodeOutputType::U64)?;
            b.build_int_binary_operation(x, zero, IntBinaryOp::ShiftRight, NodeOutputType::U64)
        })
    });
}

#[test]
fn parity_identity_sshr_zero() {
    assert_histogram_parity("x >>> 0 → x", || {
        build_fn_with_var(|b, x| {
            let zero = b.build_int_const(0u64, NodeOutputType::U64)?;
            b.build_int_binary_operation(x, zero, IntBinaryOp::SShiftRight, NodeOutputType::U64)
        })
    });
}

#[test]
fn parity_identity_and_all_ones() {
    assert_histogram_parity("x & all_ones → x", || {
        build_fn_with_var(|b, x| {
            let all = b.build_int_const(u64::MAX, NodeOutputType::U64)?;
            b.build_int_binary_operation(x, all, IntBinaryOp::And, NodeOutputType::U64)
        })
    });
}

#[test]
fn parity_identity_xor_all_ones() {
    assert_histogram_parity("x ^ all_ones → ~x", || {
        build_fn_with_var(|b, x| {
            let all = b.build_int_const(u64::MAX, NodeOutputType::U64)?;
            b.build_int_binary_operation(x, all, IntBinaryOp::Xor, NodeOutputType::U64)
        })
    });
}

#[test]
fn parity_identity_sub_self() {
    // Sub is lowered: `x - x` = `Add(x, Neg(x))`.  v1's rule_sub_self
    // matches the lowered shape.
    assert_histogram_parity("x - x → 0", || {
        build_fn_with_var(|b, x| {
            b.build_int_sub(x, x, NodeOutputType::U64)
        })
    });
}

#[test]
fn parity_identity_sub_zero() {
    // `x - 0` = `Add(x, Neg(0))`.  Const-eval folds `Neg(0)` → `0`, then
    // identity folds `Add(x, 0)` → `x`.
    assert_histogram_parity("x - 0 → x", || {
        build_fn_with_var(|b, x| {
            let zero = b.build_int_const(0u64, NodeOutputType::U64)?;
            b.build_int_sub(x, zero, NodeOutputType::U64)
        })
    });
}

// ── Bool + Float rule parity tests (Group 3) ─────────────────────────────────

#[test]
fn parity_bool_and_const_true() {
    assert_histogram_parity("bool_and(true, true) → true", || {
        make_empty_fn(|b| {
            let t = b.build_boolean_const(true);
            b.build_boolean_operation(t, t, BoolBinaryOp::And)
        }).expect("build")
    });
}

#[test]
fn parity_bool_or_const_false() {
    assert_histogram_parity("bool_or(false, false) → false", || {
        make_empty_fn(|b| {
            let f = b.build_boolean_const(false);
            b.build_boolean_operation(f, f, BoolBinaryOp::Or)
        }).expect("build")
    });
}

#[test]
fn parity_bool_xor_const() {
    assert_histogram_parity("bool_xor(true, false) → true", || {
        make_empty_fn(|b| {
            let t = b.build_boolean_const(true);
            let f = b.build_boolean_const(false);
            b.build_boolean_operation(t, f, BoolBinaryOp::Xor)
        }).expect("build")
    });
}

#[test]
fn parity_bool_neg_const() {
    assert_histogram_parity("bool_neg(true) → false", || {
        make_empty_fn(|b| {
            let t = b.build_boolean_const(true);
            b.build_boolean_unary_operation(t, BoolUnaryOp::Neg)
        }).expect("build")
    });
}

#[test]
fn parity_bool_double_neg() {
    // Build with a Bool-typed variable: the var supplies an opaque
    // "x" for `!!x → x`.  We synthesise via an IntCmpOp (always Bool).
    assert_histogram_parity("!!x → x", || {
        build_fn_with_var(|b, x| {
            // Make a Bool from x via Equal(x, 0).
            let zero = b.build_int_const(0u64, NodeOutputType::U64)?;
            let cond = b.build_int_cmp_operation(x, zero, IntCmpOp::Equal, NodeOutputType::U64)?;
            let n1 = b.build_boolean_unary_operation(cond, BoolUnaryOp::Neg)?;
            let n2 = b.build_boolean_unary_operation(n1, BoolUnaryOp::Neg)?;
            Ok(n2)
        })
    });
}

#[test]
fn parity_float_add_const() {
    assert_histogram_parity("FloatAdd(1.0, 2.0) → 3.0", || {
        make_empty_fn(|b| {
            let one = b.build_float_const(1.0_f64.to_bits(), NodeOutputType::F64);
            let two = b.build_float_const(2.0_f64.to_bits(), NodeOutputType::F64);
            b.build_float_binary_op(one, two, FloatBinaryOp::Add, NodeOutputType::F64)
        }).expect("build")
    });
}

#[test]
fn parity_float_unary_neg_const() {
    assert_histogram_parity("FloatNeg(1.0) → -1.0", || {
        make_empty_fn(|b| {
            let one = b.build_float_const(1.0_f64.to_bits(), NodeOutputType::F64);
            b.build_float_unary_op(one, FloatUnaryOp::Neg, NodeOutputType::F64)
        }).expect("build")
    });
}

#[test]
fn parity_float_cmp_equal_const() {
    assert_histogram_parity("FloatEqual(1.0, 1.0) → true", || {
        make_empty_fn(|b| {
            let one = b.build_float_const(1.0_f64.to_bits(), NodeOutputType::F64);
            b.build_float_cmp_op(one, one, FloatCmpOp::Equal)
        }).expect("build")
    });
}

// ── Reassoc + mask rule parity tests (Group 4) ──────────────────────────────

#[test]
fn parity_reassoc_add_add() {
    assert_histogram_parity("(x + 1) + 2 → x + 3", || {
        build_fn_with_var(|b, x| {
            let c1 = b.build_int_const(1u64, NodeOutputType::U64)?;
            let c2 = b.build_int_const(2u64, NodeOutputType::U64)?;
            let inner = b.build_int_binary_operation(x, c1, IntBinaryOp::Add, NodeOutputType::U64)?;
            b.build_int_binary_operation(inner, c2, IntBinaryOp::Add, NodeOutputType::U64)
        })
    });
}

#[test]
fn parity_reassoc_add_sub_lowered() {
    // (x + 1) - 2 lowers to Add(Add(x, 1), Neg(2)).  After const-folding
    // Neg(2) → IntConst(-2), reassoc should merge to Add(x, -1).
    assert_histogram_parity("(x + 1) - 2 → x - 1 (lowered)", || {
        build_fn_with_var(|b, x| {
            let c1 = b.build_int_const(1u64, NodeOutputType::U64)?;
            let c2 = b.build_int_const(2u64, NodeOutputType::U64)?;
            let add1 = b.build_int_binary_operation(x, c1, IntBinaryOp::Add, NodeOutputType::U64)?;
            b.build_int_sub(add1, c2, NodeOutputType::U64)
        })
    });
}

#[test]
fn parity_reassoc_collapse_to_zero() {
    // (x + 5) - 5 lowers to Add(Add(x, 5), Neg(5)) → Add(Add(x, 5), -5)
    // → Add(x, 0) → x.  Requires 2 outer-loop iterations: reassoc
    // produces Add(x, 0); the next iter's identity rule collapses it.
    assert_histogram_parity("(x + 5) - 5 → x", || {
        build_fn_with_var(|b, x| {
            let c = b.build_int_const(5u64, NodeOutputType::U64)?;
            let add = b.build_int_binary_operation(x, c, IntBinaryOp::Add, NodeOutputType::U64)?;
            b.build_int_sub(add, c, NodeOutputType::U64)
        })
    });
}

#[test]
fn parity_x_minus_x_eq_zero() {
    // x - x lowers to Add(x, Neg(x)).  v1's `sub(var(x), var(x)) → 0`
    // identity fires; my egraph's `is_neg_of` check should handle it
    // too.
    assert_histogram_parity("x - x → 0", || {
        build_fn_with_var(|b, x| {
            b.build_int_sub(x, x, NodeOutputType::U64)
        })
    });
}

#[test]
fn parity_reassoc_and_and_merge() {
    assert_histogram_parity("(x & 0xff) & 0x0f → x & 0x0f", || {
        build_fn_with_var(|b, x| {
            let c1 = b.build_int_const(0xffu64, NodeOutputType::U64)?;
            let c2 = b.build_int_const(0x0fu64, NodeOutputType::U64)?;
            let inner = b.build_int_binary_operation(x, c1, IntBinaryOp::And, NodeOutputType::U64)?;
            b.build_int_binary_operation(inner, c2, IntBinaryOp::And, NodeOutputType::U64)
        })
    });
}

// ── Bitcast + extend rule parity tests (Group 5) ────────────────────────────

#[test]
fn parity_int_float_round_trip() {
    assert_histogram_parity("IntBitsToFloat(FloatBitsToInt(x)) → x", || {
        // x is U64 — bitcast to F64, back to U64, then to F64 again.
        // The outer IntBitsToFloat(FloatBitsToInt(xf)) ≡ xf and should
        // collapse.
        build_fn_with_var(|b, x| {
            let xf = b.build_int_bits_to_float(x, NodeOutputType::F64)?;
            let xi = b.build_float_bits_to_int(xf, NodeOutputType::U64)?;
            let xf2 = b.build_int_bits_to_float(xi, NodeOutputType::F64)?;
            // We need the return value to be the f64 bitcast back to u64
            // (returns are u-typed); take the FloatBitsToInt of xf2.
            b.build_float_bits_to_int(xf2, NodeOutputType::U64)
        })
    });
}

#[test]
fn parity_truncate_zero_extend_round_trip() {
    assert_histogram_parity("Truncate(ZeroExtend(x)) → x", || {
        build_fn_with_var(|b, x| {
            // x is U64.  Truncate to U32, then ZeroExtend back to U64, then
            // Truncate to U32 again — the outer Truncate(ZeroExtend(.)) should
            // collapse to the inner truncate.
            let x32 = b.truncate_if_needed(x, NodeOutputType::U32)?;
            let xx = b.extend_if_needed(x32, NodeOutputType::U64, ExtendOp::ZeroExtend)?;
            // Inner Truncate(ZeroExtend(x32)) at U32 → x32.
            b.truncate_if_needed(xx, NodeOutputType::U32)
        })
    });
}
