use super::*;
use strider_ir::node::{NodeKind, ValueKind, ValueType};
use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR, reg_vn};

use crate::OptRewrite;
use crate::pipeline::Optimizer;
use crate::{DeadBranchElimination, OptCtx};

// ── DBE-simulate helper ─────────────────────────────────────────────────────
//
// The tests below characterise the REAL post-DBE shape the pass is meant to
// clean up: DBE redirects the live successor of a constant `If` past the `If`
// and detaches the folded `If`, WITHOUT stripping the dead Region predecessor.
// `CfgDetach` is then the sole remover of the now-dead predecessor slot.
//
// `simulate_dbe_redirect_without_strip` performs exactly that redirect+detach
// for an `If(BoolConst(cond))`:
//   * cond=false → live successor is `ctrl_false` (If output[1]),
//                  dead   successor is `ctrl_true`  (If output[0]).
//   * cond=true  → live successor is `ctrl_true`  (If output[0]),
//                  dead   successor is `ctrl_false` (If output[1]).
// It returns the dead control output (the slot CfgDetach must remove).
fn simulate_dbe_redirect_without_strip(
    fg: &mut strider_ir::Function,
    cond: bool,
) -> crate::Result<()> {
    // Target the live If whose constant condition equals `cond`.  This
    // disambiguates nested Ifs (outer cond=false vs inner cond=true) and skips
    // any already-detached If (0 inputs) from a prior simulate.
    let want_cond_val: u128 = u128::from(cond);
    let if_node = fg
        .graph()
        .all_node_ids()
        .find(|&n| {
            if !matches!(fg.node_kind(n), NodeKind::If) {
                return false;
            }
            let ins = fg.node_inputs(n);
            if ins.len() != 2 {
                return false;
            }
            let cond_producer = fg.value_definition(ins[1]).0;
            matches!(
                fg.node_kind(cond_producer),
                NodeKind::IntConst(v) if *v == want_cond_val
            )
        })
        .expect("a live If with the requested constant condition must exist");
    let if_outputs = fg.node_outputs(if_node).to_vec();
    assert_eq!(if_outputs.len(), 2, "If has [ctrl_true, ctrl_false]");
    let ctrl_true = if_outputs[0];
    let ctrl_false = if_outputs[1];
    let live_ctrl = if cond { ctrl_true } else { ctrl_false };
    // If inputs are [ctrl_in, cond]; ctrl_in is the live control to splice in.
    let ctrl_value = fg.node_inputs(if_node)[0];

    // Scope the rewrite ctx so its borrow of `fg` ends here (a bare
    // `drop` of a non-`Drop` type trips `clippy::drop_non_drop`).
    {
        let mut rctx = strider_pattern::RewriteCtx::try_for_built(fg)?;
        rctx.replace_value(live_ctrl, ctrl_value)?; // redirect live successor past the If
        rctx.detach_node_inputs(if_node); // detach the now-unreachable folded If
    }
    Ok(())
}

/// The join Region: the Region with the most ctrl inputs (the diamond /
/// fan-in merge).  Unique in every fixture below (the join fans in strictly
/// more predecessors than any branch/entry Region).
fn find_join_region(fg: &strider_ir::Function) -> NodeId {
    fg.graph()
        .all_node_ids()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Region))
        .max_by_key(|&n| fg.node_inputs(n).len())
        .expect("at least one Region")
}

/// The phi-token output (`outputs[1]`) of a Region.
fn region_phi_token(fg: &strider_ir::Function, region: NodeId) -> strider_ir::node::ValueId {
    fg.node_outputs(region)[1]
}

/// True iff `phi`'s token input (`inputs[0]`) is produced by `region`.
fn phi_belongs_to_region(fg: &strider_ir::Function, phi: NodeId, region: NodeId) -> bool {
    let inputs = fg.node_inputs(phi);
    if inputs.is_empty() {
        return false;
    }
    inputs[0] == region_phi_token(fg, region)
}

/// Find the VarPhi (tagged `var`) whose phi-token belongs to `region`.
fn find_var_phi_of_region(
    fg: &strider_ir::Function,
    region: NodeId,
    var: rsleigh::Vn,
) -> Option<NodeId> {
    fg.graph().all_node_ids().find(|&n| {
        matches!(fg.node_kind(n), NodeKind::Phi)
            && fg.phi_var_tag(n) == Some(var)
            && phi_belongs_to_region(fg, n, region)
    })
}

/// Find the MemPhi whose phi-token belongs to `region`.
fn find_mem_phi_of_region(fg: &strider_ir::Function, region: NodeId) -> Option<NodeId> {
    fg.graph().all_node_ids().find(|&n| {
        matches!(fg.node_kind(n), NodeKind::MemPhi) && phi_belongs_to_region(fg, n, region)
    })
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Build `if(cond_val) { return 1; } else { return 2; }`.
fn make_if_fn(cond_val: bool) -> crate::Result<strider_ir::Function> {
    let mut b = strider_ir_test_utils::empty_builder()?;
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

/// Combined test: `DeadBranchElimination` folds + detaches the constant
/// `If`; the dead branch here has no downstream join, so it becomes fully
/// unreachable from entry.  `CfgDetach` only visits validator-reachable
/// Regions, so it leaves the orphaned dead Region alone — orphans don't
/// affect correctness and are not swept.  The meaningful outcome: the dead
/// Region drops out of the reachable graph (keeping its now-dangling input)
/// while the live Regions remain, and the graph validates.
#[test]
fn cfg_detach_removes_dead_region_pred_after_dbe() -> crate::Result<()> {
    let mut fg = make_if_fn(false)?;

    // cond = false → the true branch (If output index 0) is dead.  Capture
    // the dead Region before teardown.
    let if_node = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::If))
        .expect("If node must exist");
    let dead_ctrl = fg.node_outputs(if_node)[0];
    let dead_region = fg
        .graph()
        .value_uses(dead_ctrl)
        .map(|(n, _)| n)
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Region))
        .expect("dead branch Region must consume the If's dead control output");

    DeadBranchElimination.optimize(&mut fg, &OptCtx::empty())?;
    CfgDetach.optimize(&mut fg, &OptCtx::empty())?;

    let reachable_regions: Vec<_> = fg
        .walk()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Region))
        .collect();
    assert!(
        !reachable_regions.contains(&dead_region),
        "dead Region must be unreachable from entry after DBE + CfgDetach"
    );
    assert_eq!(
        reachable_regions.len(),
        2,
        "entry and the live branch Region must remain reachable"
    );
    strider_ir::validate::validate(&fg, fg.entry().unwrap())
        .map_err(|e| anyhow::anyhow!("post-teardown validation failed: {e:?}"))?;
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
fn cfg_detach_isolated_removes_unreachable_predecessor_slot() -> crate::Result<()> {
    let mut fg = make_if_fn(true)?;

    // Find the false_region (consumer of ctrl_false = If's output[1]).
    let if_node = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::If))
        .expect("If node must exist");
    let if_outputs = fg.node_outputs(if_node).to_vec();
    assert_eq!(if_outputs.len(), 2);
    let ctrl_false = if_outputs[1];
    let false_region = fg
        .graph()
        .value_uses(ctrl_false)
        .map(|(n, _)| n)
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Region))
        .expect("false_region must be a Region consumer of ctrl_false");

    // Confirm false_region starts with 1 ctrl input.
    assert_eq!(fg.node_inputs(false_region).len(), 1);

    // Create a detached Region node (no ctrl input → unreachable from entry).
    // We give it a Control output we can wire in.
    let ghost_region = fg.graph_mut().create_node(
        NodeKind::Region,
        [],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let ghost_ctrl_value = fg.node_outputs(ghost_region)[0];

    // Wire the ghost's Control output into false_region as a second pred slot.
    fg.graph_mut()
        .add_node_input(false_region, ghost_ctrl_value)?;
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

// ── realistic post-DBE characterisation tests ───────────────────────────────

/// Test 1 (load-bearing): a join carrying BOTH a VarPhi and a MemPhi.
///
/// Build `if(false) { v=1; mem++; } else { v=2; mem++; } join: read v; return`.
/// Simulate the future DBE (redirect live ctrl past the If + detach the If,
/// WITHOUT stripping the dead Region predecessor), then run `CfgDetach` and
/// assert it removes the dead control slot AND the matching VarPhi/MemPhi value
/// slots, leaving a structurally valid graph.
#[test]
fn cfg_detach_collapses_var_and_mem_phi_then_validates() -> crate::Result<()> {
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region()?;
    let true_r = b.create_region()?;
    let false_r = b.create_region()?;
    let join = b.create_region()?;
    b.set_entry_region(entry)?;

    b.set_region(entry);
    let cond = b.build_boolean_const(false);
    b.build_if(cond, true_r, false_r)?;

    // true branch: write var, advance memory via a modeled CallOther.
    b.set_region(true_r);
    let v_t = b.build_int_const(1u64, ValueType::I64)?;
    b.write_variable(&var, v_t)?;
    let (call_t, _) = b.build_call_other(
        0,
        "cpuid",
        None,
        &[],
        &strider_target::BuiltCallOtherAbi {
            implicit_reads: Vec::new(),
            implicit_writes: Vec::new(),
            clobbers_memory: false,
        },
        None,
        false,
    )?;
    let mem_t = b.function().node_outputs(call_t)[1];
    b.advance_cur_region_memory(mem_t)?;
    b.build_branch(join)?;

    // false branch: write var differently, advance memory too.
    b.set_region(false_r);
    let v_f = b.build_int_const(2u64, ValueType::I64)?;
    b.write_variable(&var, v_f)?;
    let (call_f, _) = b.build_call_other(
        0,
        "cpuid",
        None,
        &[],
        &strider_target::BuiltCallOtherAbi {
            implicit_reads: Vec::new(),
            implicit_writes: Vec::new(),
            clobbers_memory: false,
        },
        None,
        false,
    )?;
    let mem_f = b.function().node_outputs(call_f)[1];
    b.advance_cur_region_memory(mem_f)?;
    b.build_branch(join)?;

    // join: read var (forces VarPhi) and return.
    b.set_region(join);
    let merged = b.read_variable(&var)?;
    b.build_return(Some(merged), &[])?;
    b.set_lift_addr(None);

    let mut fg = b.build()?;

    // Pre-conditions: VarPhi + MemPhi with two value inputs each.
    let join_node = find_join_region(&fg);
    let var_phi = find_var_phi_of_region(&fg, join_node, var).expect("VarPhi at join");
    let mem_phi = find_mem_phi_of_region(&fg, join_node).expect("MemPhi at join");
    assert_eq!(
        fg.node_inputs(var_phi).len(),
        3,
        "VarPhi: token + 2 values before detach"
    );
    assert_eq!(
        fg.node_inputs(mem_phi).len(),
        3,
        "MemPhi: token + 2 values before detach"
    );

    // Simulate the future DBE: cond=false → live=false branch, dead=true branch.
    simulate_dbe_redirect_without_strip(&mut fg, false)?;

    // The dead branch (true_r) was reached only via the now-detached If, so it
    // is control-dead.  Run CfgDetach.
    let result = CfgDetach.optimize(&mut fg, &OptCtx::empty())?;
    assert!(result.changed(), "CfgDetach must report Changed");

    // VarPhi and MemPhi each drop to exactly one value input.
    assert_eq!(
        fg.node_inputs(var_phi).len(),
        2,
        "VarPhi: token + 1 value after CfgDetach"
    );
    assert_eq!(
        fg.node_inputs(mem_phi).len(),
        2,
        "MemPhi: token + 1 value after CfgDetach"
    );

    // The dead-branch Region (true_r as a NodeId) drops to 0 ctrl inputs.
    let true_r_node = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Region) && fg.node_inputs(n).is_empty())
        .expect("dead-branch Region with 0 ctrl inputs");
    assert_eq!(fg.node_inputs(true_r_node).len(), 0);

    // CRITICAL: the post-CfgDetach graph is structurally valid.
    strider_ir::validate::validate(&fg, fg.entry().unwrap())
        .map_err(|e| anyhow::anyhow!("post-CfgDetach validation failed: {e:?}"))?;
    Ok(())
}

/// Test 2: a MemPhi-only join (no variable merged).
///
/// `if(false) { mem++; } else { mem++; } join: return`.  After the DBE-simulate
/// + CfgDetach the MemPhi drops to one memory input and the graph validates.
#[test]
fn cfg_detach_collapses_mem_phi_only_then_validates() -> crate::Result<()> {
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region()?;
    let true_r = b.create_region()?;
    let false_r = b.create_region()?;
    let join = b.create_region()?;
    b.set_entry_region(entry)?;

    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let cond = b.build_boolean_const(false);
    b.build_if(cond, true_r, false_r)?;

    b.set_region(true_r);
    let (call_t, _) = b.build_call_other(
        0,
        "cpuid",
        None,
        &[],
        &strider_target::BuiltCallOtherAbi {
            implicit_reads: Vec::new(),
            implicit_writes: Vec::new(),
            clobbers_memory: false,
        },
        None,
        false,
    )?;
    let mem_t = b.function().node_outputs(call_t)[1];
    b.advance_cur_region_memory(mem_t)?;
    b.build_branch(join)?;

    b.set_region(false_r);
    let (call_f, _) = b.build_call_other(
        0,
        "cpuid",
        None,
        &[],
        &strider_target::BuiltCallOtherAbi {
            implicit_reads: Vec::new(),
            implicit_writes: Vec::new(),
            clobbers_memory: false,
        },
        None,
        false,
    )?;
    let mem_f = b.function().node_outputs(call_f)[1];
    b.advance_cur_region_memory(mem_f)?;
    b.build_branch(join)?;

    b.set_region(join);
    b.build_return(None, &[])?;
    b.set_lift_addr(None);

    let mut fg = b.build()?;

    let join_node = find_join_region(&fg);
    let mem_phi = find_mem_phi_of_region(&fg, join_node).expect("MemPhi at join");
    assert_eq!(
        fg.node_inputs(mem_phi).len(),
        3,
        "MemPhi: token + 2 values before detach"
    );

    simulate_dbe_redirect_without_strip(&mut fg, false)?;

    let result = CfgDetach.optimize(&mut fg, &OptCtx::empty())?;
    assert!(result.changed(), "CfgDetach must report Changed");

    assert_eq!(
        fg.node_inputs(mem_phi).len(),
        2,
        "MemPhi: token + 1 value after CfgDetach"
    );

    strider_ir::validate::validate(&fg, fg.entry().unwrap())
        .map_err(|e| anyhow::anyhow!("post-CfgDetach validation failed: {e:?}"))?;
    Ok(())
}

/// Test 3: TWO dead predecessors on a 3-way join.
///
/// Build a nested-if shape that produces a 3-input join with a VarPhi over
/// three values, then detach TWO of the three branches.  CfgDetach must remove
/// both dead slots (region 3→1, VarPhi 3→1 values) and the graph validates.
#[test]
fn cfg_detach_removes_two_dead_predecessors_then_validates() -> crate::Result<()> {
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region()?;
    let r_a = b.create_region()?; // outer-true
    let mid = b.create_region()?; // outer-false → inner if
    let r_b = b.create_region()?; // inner-true
    let r_c = b.create_region()?; // inner-false
    let join = b.create_region()?;
    b.set_entry_region(entry)?;

    // Outer if: entry → if(false) { r_a } else { mid }.
    b.set_region(entry);
    let cond_outer = b.build_boolean_const(false);
    b.build_if(cond_outer, r_a, mid)?;

    // r_a: write var, branch to join.
    b.set_region(r_a);
    let v_a = b.build_int_const(1u64, ValueType::I64)?;
    b.write_variable(&var, v_a)?;
    b.build_branch(join)?;

    // mid: inner if(true) { r_b } else { r_c }.
    b.set_region(mid);
    let cond_inner = b.build_boolean_const(true);
    b.build_if(cond_inner, r_b, r_c)?;

    b.set_region(r_b);
    let v_b = b.build_int_const(2u64, ValueType::I64)?;
    b.write_variable(&var, v_b)?;
    b.build_branch(join)?;

    b.set_region(r_c);
    let v_c = b.build_int_const(3u64, ValueType::I64)?;
    b.write_variable(&var, v_c)?;
    b.build_branch(join)?;

    b.set_region(join);
    let merged = b.read_variable(&var)?;
    b.build_return(Some(merged), &[])?;
    b.set_lift_addr(None);

    let mut fg = b.build()?;

    // The join is a 3-way Region with a 3-value VarPhi.
    let join_node = find_join_region(&fg);
    assert_eq!(fg.node_inputs(join_node).len(), 3, "3-way join");
    let var_phi = find_var_phi_of_region(&fg, join_node, var).expect("VarPhi at join");
    assert_eq!(fg.node_inputs(var_phi).len(), 4, "token + 3 values");

    // Simulate DBE on BOTH ifs.  Outer cond=false → live=mid (false branch),
    // dead=r_a (true branch).  Inner cond=true → live=r_b (true branch),
    // dead=r_c (false branch).  So r_a and r_c become control-dead.
    simulate_dbe_redirect_without_strip(&mut fg, false)?; // outer If
    simulate_dbe_redirect_without_strip(&mut fg, true)?; // inner If

    let result = CfgDetach.optimize(&mut fg, &OptCtx::empty())?;
    assert!(result.changed(), "CfgDetach must report Changed");

    assert_eq!(
        fg.node_inputs(join_node).len(),
        1,
        "join Region drops 3→1 ctrl inputs"
    );
    assert_eq!(
        fg.node_inputs(var_phi).len(),
        2,
        "VarPhi drops 3→1 values (token + 1)"
    );

    strider_ir::validate::validate(&fg, fg.entry().unwrap())
        .map_err(|e| anyhow::anyhow!("post-CfgDetach validation failed: {e:?}"))?;
    Ok(())
}

/// Test 4: a Region that is data-reachable but control-dead must still be
/// visited and cleaned.
///
/// After the DBE-simulate, the dead branch's Region is no longer
/// control-reachable from entry, but it stays reachable through the validator's
/// general graph walk via backward-data edges (its VarPhi value still feeds the
/// live join phi until the slot is removed).  CfgDetach iterates `walk()` (not
/// the control-only set), so it must visit the control-dead Region and remove
/// its dead predecessor.  This pins the iteration-set behaviour.
#[test]
fn cfg_detach_visits_control_dead_but_data_reachable_region() -> crate::Result<()> {
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region()?;
    let true_r = b.create_region()?;
    let false_r = b.create_region()?;
    let join = b.create_region()?;
    b.set_entry_region(entry)?;

    b.set_region(entry);
    let cond = b.build_boolean_const(false);
    b.build_if(cond, true_r, false_r)?;

    b.set_region(true_r);
    let v_t = b.build_int_const(1u64, ValueType::I64)?;
    b.write_variable(&var, v_t)?;
    b.build_branch(join)?;

    b.set_region(false_r);
    let v_f = b.build_int_const(2u64, ValueType::I64)?;
    b.write_variable(&var, v_f)?;
    b.build_branch(join)?;

    b.set_region(join);
    let merged = b.read_variable(&var)?;
    b.build_return(Some(merged), &[])?;
    b.set_lift_addr(None);

    let mut fg = b.build()?;

    simulate_dbe_redirect_without_strip(&mut fg, false)?;

    // The dead-branch Region (true_r) is NOT control-reachable from entry now,
    // but it IS visited by the general walk (its VarPhi value still feeds the
    // join phi).  Confirm the control-only set excludes it but the general walk
    // includes it — the premise of the iteration-set choice.
    let entry_node = fg.entry().unwrap();
    let ctrl_reach = strider_ir::walk::cfg_reachable(fg.graph(), entry_node);
    let walk_set: Vec<NodeId> = fg.walk().collect();
    // Identify true_r: the Region whose sole producer (the If's ctrl_true) is
    // now detached / unreachable.
    let dead_region = fg
        .graph()
        .all_node_ids()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Region))
        .find(|&n| {
            let ins = fg.node_inputs(n);
            ins.len() == 1 && !ctrl_reach.contains(fg.value_definition(ins[0]).0)
        })
        .expect("a control-dead Region predecessor must exist");
    assert!(
        !ctrl_reach.contains(dead_region),
        "dead Region is NOT in the control-reachable set"
    );
    assert!(
        walk_set.contains(&dead_region),
        "dead Region IS in the general graph walk (data-reachable)"
    );

    let result = CfgDetach.optimize(&mut fg, &OptCtx::empty())?;
    assert!(result.changed(), "CfgDetach must report Changed");
    assert_eq!(
        fg.node_inputs(dead_region).len(),
        0,
        "control-dead Region's predecessor was removed"
    );

    strider_ir::validate::validate(&fg, fg.entry().unwrap())
        .map_err(|e| anyhow::anyhow!("post-CfgDetach validation failed: {e:?}"))?;
    Ok(())
}
