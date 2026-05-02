//! Unit tests for the [`SubToAdd`] canonicalisation pass.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::SubToAdd;
use crate::pipeline::Optimizer;
use crate::test_support::make_fn_with_var;
use ir::node::{NodeKind, NodeOutputType};
use ir::test_utils::reg_vn;
use ir::IntBinaryOp;

/// Find the unique `IntBinaryOp(op)` node in `fg`'s reachable graph.
fn find_unique_binop(
    fg: &ir::BuiltFunctionGraph,
    op: IntBinaryOp,
) -> Option<ir::node::NodeId> {
    let mut hits: Vec<_> = fg
        .preorder()
        .filter(|n| matches!(*fg.graph.node_kind(*n), NodeKind::IntBinaryOp(o) if o == op))
        .collect();
    if hits.len() == 1 { hits.pop() } else { None }
}

fn count_kind<P: Fn(&NodeKind) -> bool>(fg: &ir::BuiltFunctionGraph, p: P) -> usize {
    fg.preorder().filter(|n| p(fg.graph.node_kind(*n))).count()
}

#[test]
fn sub_const_rewrites_to_add_neg_const() -> crate::Result<()> {
    // x - 7 → x + (-7).  -7 at U64 is 0xFFFFFFFFFFFFFFF9.
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c7 = b.build_int_const(7u64, NodeOutputType::U64).unwrap();
        b.build_int_binary_operation(x, c7, IntBinaryOp::Sub, NodeOutputType::U64)
    })?;

    let _ = SubToAdd.optimize(&mut fg.graph, fg.entry)?;

    // The Sub node is detached (its output has no users).  The new
    // Add node points at the original LHS and a fresh IntConst.
    let add = find_unique_binop(&fg, IntBinaryOp::Add).expect("expected one Add");
    let inputs = fg.graph.node_inputs(add);
    assert_eq!(inputs.len(), 2);
    assert_eq!(inputs[0], x);
    let rhs_node = fg.graph.get_node_from_output(inputs[1]);
    let NodeKind::IntConst(rhs_val) = *fg.graph.node_kind(rhs_node) else {
        panic!("RHS of new Add should be IntConst");
    };
    assert_eq!(rhs_val, (-7i128) as u128 & u64::MAX as u128);
    Ok(())
}

#[test]
fn sub_zero_is_a_no_op() -> crate::Result<()> {
    // x - 0 → x - 0 (unchanged; ConstantFold collapses to x separately).
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let c0 = b.build_int_const(0u64, NodeOutputType::U64).unwrap();
        b.build_int_binary_operation(x, c0, IntBinaryOp::Sub, NodeOutputType::U64)
    })?;

    let r = SubToAdd.optimize(&mut fg.graph, fg.entry)?;
    assert!(!r.changed(), "Sub(_, 0) must not be rewritten — Const 0 + Add 0 is a redundant pair");
    // The original Sub still stands.
    assert_eq!(count_kind(&fg, |k| matches!(k, NodeKind::IntBinaryOp(IntBinaryOp::Sub))), 1);
    Ok(())
}

#[test]
fn sub_with_variable_rhs_is_not_rewritten() -> crate::Result<()> {
    // x - x — RHS is a varnode, not a const.  Pass must leave it alone:
    // rewriting to add(x, neg(x)) would *add* a Neg node with no payoff.
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        b.build_int_binary_operation(x, x, IntBinaryOp::Sub, NodeOutputType::U64)
    })?;

    let r = SubToAdd.optimize(&mut fg.graph, fg.entry)?;
    assert!(!r.changed());
    assert_eq!(count_kind(&fg, |k| matches!(k, NodeKind::IntBinaryOp(IntBinaryOp::Sub))), 1);
    assert_eq!(count_kind(&fg, |k| matches!(k, NodeKind::IntBinaryOp(IntBinaryOp::Add))), 0);
    Ok(())
}

#[test]
fn sub_const_at_u32_negates_within_width() -> crate::Result<()> {
    // U32: x - 1 → x + 0xFFFFFFFF.  Width-aware negation matters
    // here — naively writing -1 as i128 = u128::MAX would mismatch
    // the U32 output type.
    let vn = reg_vn(0x1000, 4);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let c1 = b.build_int_const(1u64, NodeOutputType::U32).unwrap();
        b.build_int_binary_operation(x, c1, IntBinaryOp::Sub, NodeOutputType::U32)
    })?;

    let _ = SubToAdd.optimize(&mut fg.graph, fg.entry)?;

    let add = find_unique_binop(&fg, IntBinaryOp::Add).expect("expected one Add");
    let inputs = fg.graph.node_inputs(add);
    let rhs_node = fg.graph.get_node_from_output(inputs[1]);
    let NodeKind::IntConst(rhs_val) = *fg.graph.node_kind(rhs_node) else {
        panic!("RHS of Add should be IntConst");
    };
    // -1 at U32 = 0xFFFFFFFF — masked to the type's width, NOT the
    // u128 sign-extension.
    assert_eq!(rhs_val, 0xFFFF_FFFF);
    Ok(())
}
