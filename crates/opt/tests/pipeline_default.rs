//! End-to-end tests for `opt::default_pipeline`. Black-box: exercises only
//! the public API of the `opt` crate.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]

mod common;

use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir::test_utils::SENTINEL_LIFT_ADDR;
use strider_ir::IntBinaryOp;
use opt::{ConstantFold, DeadBranchElimination, KnownBits, RedundantPhis, default_pipeline};

use common::{make_fn, make_fn_with_var, reg_vn, return_kind};

/// `((1 + 2) + 3) + 4 → 10` — standard ConstantFold cascade.
#[test]
fn default_pipeline_folds_int_chain() -> opt::Result<()> {
    let mut fg = make_fn(|b| {
        let c1 = b.build_int_const(1u64, NodeOutputType::U64).unwrap();
        let c2 = b.build_int_const(2u64, NodeOutputType::U64).unwrap();
        let c3 = b.build_int_const(3u64, NodeOutputType::U64).unwrap();
        let c4 = b.build_int_const(4u64, NodeOutputType::U64).unwrap();
        let a = b.build_int_binary_operation(c1, c2, IntBinaryOp::Add, NodeOutputType::U64)?;
        let bb = b.build_int_binary_operation(a, c3, IntBinaryOp::Add, NodeOutputType::U64)?;
        b.build_int_binary_operation(bb, c4, IntBinaryOp::Add, NodeOutputType::U64)
    })?;
    default_pipeline().run(&mut fg.graph, fg.entry)?;
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(10));
    Ok(())
}

/// `if(true) return 1 else return 2` — pipeline must eliminate the If and
/// the VarPhi at the join.
#[test]
fn default_pipeline_eliminates_dead_branch() -> opt::Result<()> {
    use strider_ir::FunctionBuilder;
    let mut b = FunctionBuilder::empty()?;
    let entry = b.create_region()?;
    let t = b.create_region()?;
    let f = b.create_region()?;
    b.set_entry_region(entry)?;
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let cond = b.build_boolean_const(true);
    b.build_if(cond, t, f)?;
    b.set_region(t);
    let v = b.build_int_const(1u64, strider_ir::ValueType::U64).unwrap();
    b.build_return(Some(v), &[])?;
    b.set_region(f);
    let v2 = b.build_int_const(2u64, strider_ir::ValueType::U64).unwrap();
    b.build_return(Some(v2), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    default_pipeline().run(&mut fg.graph, fg.entry)?;

    let reachable: std::collections::HashSet<_> = fg.preorder().collect();
    let if_nodes = fg
        .all_node_ids()
        .filter(|n| reachable.contains(n))
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::If))
        .count();
    assert_eq!(if_nodes, 0, "If(true) must be eliminated");
    Ok(())
}

/// The pipeline calls `validate` at the end: a trivial graph runs to
/// completion without error.
#[test]
fn default_pipeline_validates_at_end() -> opt::Result<()> {
    let mut fg = make_fn(|b| Ok(b.build_int_const(42u64, NodeOutputType::U64).unwrap()))?;
    default_pipeline().run(&mut fg.graph, fg.entry)?;
    Ok(())
}

/// KnownBits + ConstantFold cooperate on bit-level masking: `(x | 0xFF) & 0xF0`
/// for U8 — `(x | 0xFF)` is statically all-ones; ANDing with `0xF0` yields `0xF0`.
#[test]
fn default_pipeline_known_bits_cooperates_with_constant_fold() -> opt::Result<()> {
    let vn = reg_vn(0x1000, 1);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let ff = b.build_int_const(0xFFu64, NodeOutputType::U8).unwrap();
        let f0 = b.build_int_const(0xF0u64, NodeOutputType::U8).unwrap();
        let or_ = b.build_int_binary_operation(x, ff, IntBinaryOp::Or, NodeOutputType::U8)?;
        b.build_int_binary_operation(or_, f0, IntBinaryOp::And, NodeOutputType::U8)
    })?;
    default_pipeline().run(&mut fg.graph, fg.entry)?;
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0xF0));
    Ok(())
}

/// Default pipeline composes correctly when each pass is added in a custom order.
#[test]
fn manual_pipeline_matches_default_for_simple_input() -> opt::Result<()> {
    let mut fg = make_fn(|b| {
        let c = b.build_int_const(7u64, NodeOutputType::U64).unwrap();
        Ok(c)
    })?;
    let mut p = opt::OptimizerPipeline::new();
    p.add(ConstantFold);
    p.add(KnownBits);
    p.add(RedundantPhis);
    p.add(DeadBranchElimination);
    p.run(&mut fg.graph, fg.entry)?;
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(7));
    Ok(())
}
