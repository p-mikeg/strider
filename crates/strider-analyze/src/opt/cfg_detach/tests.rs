use super::*;
use strider_ir::node::{NodeKind, NodeOutputKind};
use strider_ir::FunctionBuilder;
use strider_ir_test_utils::SENTINEL_LIFT_ADDR;

use crate::opt::pipeline::Optimizer;
use crate::opt::{DeadBranchElimination, OptCtx};

// ── helpers ───────────────────────────────────────────────────────────────────

/// Count Region nodes with exactly `n` ctrl inputs.
fn count_regions_with_n_inputs(fg: &strider_ir::Graph, n: usize) -> usize {
    fg.all_node_ids()
        .filter(|&node| {
            matches!(fg.node_kind(node), NodeKind::Region)
                && fg.node_inputs(node).len() == n
        })
        .count()
}

/// Build `if(cond_val) { return 1; } else { return 2; }`.
fn make_if_fn(cond_val: bool) -> crate::opt::Result<strider_ir::Function> {
    let mut b = FunctionBuilder::empty()?;
    let entry = b.create_region()?;
    let true_region = b.create_region()?;
    let false_region = b.create_region()?;

    b.set_entry_region(entry)?;
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let cond = b.build_boolean_const(cond_val);
    b.build_if(cond, true_region, false_region)?;

    b.set_region(true_region);
    let true_val = b.build_int_const(1u64, strider_ir::ValueType::I64)?;
    b.build_return(Some(true_val), &[])?;

    b.set_region(false_region);
    let false_val = b.build_int_const(2u64, strider_ir::ValueType::I64)?;
    b.build_return(Some(false_val), &[])?;
    b.set_lift_addr(None);

    b.build()
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Combined test: after `DeadBranchElimination` (which already strips the dead
/// Region predecessor in the current implementation), running `CfgDetach` must
/// leave the graph with exactly one Region at 0 ctrl inputs (the dead branch).
///
/// This test pins the end-state invariant regardless of which pass performs
/// the strip — whichever one acts first, `CfgDetach` is idempotent over an
/// already-clean graph (no double-removal).
#[test]
fn cfg_detach_removes_dead_region_pred_after_dbe() -> crate::opt::Result<()> {
    let mut fg = make_if_fn(false)?;
    DeadBranchElimination.optimize(&mut fg, &OptCtx::empty())?;
    CfgDetach.optimize(&mut fg, &OptCtx::empty())?;
    assert_eq!(
        count_regions_with_n_inputs(&fg, 0),
        1,
        "dead Region ends at 0 inputs after DBE + CfgDetach"
    );
    Ok(())
}

/// Focused isolation test: build a graph, then surgically add an extra ctrl
/// input from a disconnected (unreachable) node to one of the branch Regions.
/// `CfgDetach` alone must strip the unreachable slot and report `Changed`.
///
/// Surgery:
///   1. Build `make_if_fn(true)` — the false branch is dead but DBE is NOT run.
///      We leave the graph in its post-build, pre-optimization state.
///   2. Find the `false_region` (consumer of the If's `ctrl_false` output).
///   3. Create a detached `Region` node in the graph (it has no inputs, no
///      outputs that feed into the CFG spine — it is unreachable from entry).
///   4. Get the detached Region's Control output and `add_node_input` it to
///      `false_region` at a second slot.
///   5. Run `CfgDetach`: the detached Region's ctrl output is the producer of
///      the new slot; since the detached Region is not reachable from entry,
///      `CfgDetach` removes that slot.
///   6. Assert `Changed` and `false_region` has 1 ctrl input (the original,
///      still-reachable If's `ctrl_false`).
#[test]
fn cfg_detach_isolated_removes_unreachable_predecessor_slot() -> crate::opt::Result<()> {
    let mut fg = make_if_fn(true)?;

    // Find the false_region (consumer of ctrl_false = If's output[1]).
    let if_node = fg
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::If))
        .expect("If node must exist");
    let if_outputs = fg.node_outputs(if_node).to_vec();
    assert_eq!(if_outputs.len(), 2);
    let ctrl_false = if_outputs[1];
    let false_region = fg
        .output_uses(ctrl_false)
        .map(|(n, _)| n)
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Region))
        .expect("false_region must be a Region consumer of ctrl_false");

    // Confirm false_region starts with 1 ctrl input.
    assert_eq!(fg.node_inputs(false_region).len(), 1);

    // Create a detached Region node (no ctrl input → unreachable from entry).
    // We give it a Control output we can wire in.
    let ghost_region = fg.create_node(
        NodeKind::Region,
        [],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    let ghost_ctrl_out = fg.node_outputs(ghost_region)[0];

    // Wire the ghost's Control output into false_region as a second pred slot.
    fg.add_node_input(false_region, ghost_ctrl_out)?;
    assert_eq!(
        fg.node_inputs(false_region).len(),
        2,
        "false_region should now have 2 ctrl inputs after surgery"
    );

    // Run CfgDetach in isolation: the ghost_region has no ctrl inputs so it is
    // not reachable from entry.  CfgDetach must remove the ghost slot.
    let result = CfgDetach.optimize(&mut fg, &OptCtx::empty())?;
    assert!(
        result.changed(),
        "CfgDetach must report Changed when it removes an unreachable predecessor"
    );
    assert_eq!(
        fg.node_inputs(false_region).len(),
        1,
        "false_region must drop back to 1 ctrl input after CfgDetach"
    );

    Ok(())
}
