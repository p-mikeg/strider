use super::*;
use ir::FunctionBuilder;
use ir::node::{NodeKind, NodeOutputType};
use ir::IntBinaryOp;

use crate::{ConstantFold, OptimizerPipeline, RedundantPhis};

fn reg_vn(off: u64, size: u32) -> rsleigh::Vn {
    rsleigh::Vn {
        size,
        addr: rsleigh::VnAddr {
            off,
            space: rsleigh::VnSpace::REGISTER,
        },
    }
}

// Helper: count ControlState nodes with N ctrl inputs.
fn count_cs_with_n_inputs(fg: &ir::BuiltFunctionGraph, n: usize) -> usize {
    fg.all_node_ids()
        .filter(|&node| {
            matches!(fg.graph.node_kind(node), NodeKind::ControlState)
                && fg.graph.node_inputs(node).len() == n
        })
        .count()
}

/// Build a function with `if(cond)`, two branches each ending in `return`.
fn make_if_fn(cond_val: bool) -> Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    let true_region = b.create_region()?;
    let false_region = b.create_region()?;

    b.set_entry_region(entry)?;
    b.set_region(entry);
    let cond = b.build_boolean_const(cond_val);
    b.build_if(cond, true_region, false_region)?;

    b.set_region(true_region);
    let true_val = b.build_int_const(1, ir::ValueType::U64);
    b.build_return(Some(true_val), &[])?;

    b.set_region(false_region);
    let false_val = b.build_int_const(2, ir::ValueType::U64);
    b.build_return(Some(false_val), &[])?;

    Ok(b.build()?)
}

// ── Original tests ────────────────────────────────────────────────────────────

#[test]
fn dead_branch_false() -> Result<()> {
    let mut fg = make_if_fn(false)?;

    // Before: three ControlState nodes with 1 ctrl input each
    // (entry, true-branch, false-branch).
    assert_eq!(count_cs_with_n_inputs(&fg, 1), 3);

    let result = DeadBranchElimination.optimize(&mut fg)?;
    assert!(result.changed());

    // After: true region's CS loses its input (dead branch removed).
    // Entry CS and false region's CS each still have 1 input.
    assert_eq!(
        count_cs_with_n_inputs(&fg, 0),
        1,
        "dead branch CS should have 0 inputs"
    );
    assert_eq!(
        count_cs_with_n_inputs(&fg, 1),
        2,
        "entry and live branch CS should have 1 input"
    );
    Ok(())
}

#[test]
fn dead_branch_true() -> Result<()> {
    let mut fg = make_if_fn(true)?;

    assert_eq!(count_cs_with_n_inputs(&fg, 1), 3);

    let result = DeadBranchElimination.optimize(&mut fg)?;
    assert!(result.changed());

    assert_eq!(
        count_cs_with_n_inputs(&fg, 0),
        1,
        "dead (false) branch CS should have 0 inputs"
    );
    assert_eq!(
        count_cs_with_n_inputs(&fg, 1),
        2,
        "entry and live (true) branch CS should have 1 input"
    );
    Ok(())
}

#[test]
fn dead_branch_non_const_no_change() -> Result<()> {
    // Build if(x) where x is a non-const boolean.
    let mut fg = {
        let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
        let entry = b.create_region()?;
        let true_r = b.create_region()?;
        let false_r = b.create_region()?;
        b.set_entry_region(entry)?;
        b.set_region(entry);
        // Non-constant condition: BoolConst(true) & BoolConst(false)
        // (two nodes combined so it won't be constant at the If level until
        // ConstantFold runs — but we don't run ConstantFold here).
        let t = b.build_boolean_const(true);
        let f = b.build_boolean_const(false);
        let cond = b.build_boolean_operation(t, f, ir::BoolBinaryOp::And)?;
        b.build_if(cond, true_r, false_r)?;
        b.set_region(true_r);
        b.build_return(None, &[])?;
        b.set_region(false_r);
        b.build_return(None, &[])?;
        b.build()?
    };

    // DeadBranchElimination alone should not fire because the condition
    // is a BoolBinaryOp node, not a BoolConst.
    assert!(!DeadBranchElimination.optimize(&mut fg)?.changed());
    Ok(())
}

// ── Comprehensive tests ───────────────────────────────────────────────────────

/// `if(true)` nested inside the live branch of an outer `if(true)` — the
/// pipeline (ConstantFold + DBE + RedundantPhis) must eliminate both Ifs.
#[test]
fn nested_if_true_eliminated() -> Result<()> {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    let outer_t = b.create_region()?;
    let outer_f = b.create_region()?;
    let inner_t = b.create_region()?;
    let inner_f = b.create_region()?;
    b.set_entry_region(entry)?;

    b.set_region(entry);
    let outer_cond = b.build_boolean_const(true);
    b.build_if(outer_cond, outer_t, outer_f)?;

    b.set_region(outer_t);
    let inner_cond = b.build_boolean_const(true);
    b.build_if(inner_cond, inner_t, inner_f)?;

    b.set_region(outer_f);
    b.build_return(None, &[])?;
    b.set_region(inner_t);
    let v = b.build_int_const(1, ir::ValueType::U64);
    b.build_return(Some(v), &[])?;
    b.set_region(inner_f);
    b.build_return(None, &[])?;

    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(DeadBranchElimination);
    pipeline.add(RedundantPhis);
    pipeline.run(&mut fg)?;

    let reachable: std::collections::HashSet<_> = fg.preorder().collect();
    let if_count = fg
        .all_node_ids()
        .filter(|n| reachable.contains(n))
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::If))
        .count();
    assert_eq!(if_count, 0, "both If nodes must be eliminated");
    Ok(())
}

/// A ControlPhi at a 2-input join — when the dead branch is removed, the
/// phi must lose exactly one input slot (the dead position).
#[test]
fn control_phi_loses_dead_slot() -> Result<()> {
    let var = reg_vn(0x1000, 8);
    let mut b = FunctionBuilder::new_raw(vec![var], &[var], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    let true_r = b.create_region()?;
    let false_r = b.create_region()?;
    let join = b.create_region()?;
    b.set_entry_region(entry)?;

    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, true_r, false_r)?;

    b.set_region(true_r);
    let v_t = b.build_int_const(1, NodeOutputType::U64);
    b.write_variable(&var, v_t)?;
    b.build_branch(join)?;

    b.set_region(false_r);
    let v_f = b.build_int_const(2, NodeOutputType::U64);
    b.write_variable(&var, v_f)?;
    b.build_branch(join)?;

    b.set_region(join);
    let merged = b.read_variable(&var)?;
    b.build_return(Some(merged), &[])?;

    let mut fg = b.build()?;
    let pre_phi_count = fg
        .all_node_ids()
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::ControlPhi(_)))
        .count();
    assert!(pre_phi_count > 0);

    DeadBranchElimination.optimize(&mut fg)?;
    // A ControlPhi at the join should now have only the live predecessor's
    // value input (length = 1 token + 1 value = 2).
    let join_phi = fg
        .all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::ControlPhi(v) if *v == var))
        .expect("control phi at join must exist");
    let phi_inputs = fg.graph.node_inputs(join_phi);
    assert_eq!(phi_inputs.len(), 2, "phi must have exactly 1 live value");
    Ok(())
}
