use super::*;
use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir::FunctionBuilder;
use strider_ir_test_utils::{reg_vn, RegisterSet, SENTINEL_LIFT_ADDR};

use crate::opt::pipeline::Optimizer;

// ── helpers ─────────────────────────────────────────────────────────────────

/// Locate the unique `Return` node.
fn find_return(fg: &strider_ir::Function) -> NodeId {
    fg.all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
        .expect("Return present")
}

/// Find the VarPhi tagged with `var`.
fn find_var_phi(fg: &strider_ir::Function, var: rsleigh::Vn) -> NodeId {
    fg.all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Phi) && fg.phi_var_tag(n) == Some(var))
        .expect("VarPhi present")
}

// ── single-value phi collapses ──────────────────────────────────────────────

/// A VarPhi with a single reachable predecessor (one value input besides
/// the token) is trivial — its consumers must rewire to that value.
#[test]
fn single_value_phi_collapses() -> crate::opt::Result<()> {
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region()?;
    let join = b.create_region()?;
    b.set_entry_region(entry)?;

    b.set_region(entry);
    b.build_branch(join)?;

    b.set_region(join);
    let read_back = b.read_variable(&var)?;
    b.build_return(Some(read_back), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let phi = find_var_phi(&fg, var);
    let phi_inputs = fg.node_inputs(phi);
    assert_eq!(phi_inputs.len(), 2, "token + 1 value");
    let lone_value = phi_inputs[1];

    let changed = PhiCollapse
        .optimize(&mut fg, &crate::opt::OptCtx::empty())?
        .changed();
    assert!(changed, "single-value phi must collapse");

    let ret_val = fg.node_inputs(find_return(&fg))[2];
    assert_eq!(
        ret_val, lone_value,
        "Return must rewire to the phi's only value input"
    );
    Ok(())
}

// ── multi-value-but-all-equal collapses ─────────────────────────────────────

/// A VarPhi at a real 2-predecessor join whose two value inputs resolve
/// to the SAME NodeOutputId collapses to that value (distinct count == 1).
///
/// The builder dedups two structurally-equal writes into a single phi
/// input, so to construct the genuine multi-input-all-equal shape we build
/// a 2-distinct-value join and then surgically redirect the second value
/// input to equal the first — leaving `[token, V, V]`.
#[test]
fn multi_value_all_equal_phi_collapses() -> crate::opt::Result<()> {
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region()?;
    let a = b.create_region()?;
    let bb = b.create_region()?;
    let join = b.create_region()?;
    b.set_entry_region(entry)?;

    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, a, bb)?;

    b.set_region(a);
    let v_a = b.build_int_const(1u64, NodeOutputType::I64)?;
    b.write_variable(&var, v_a)?;
    b.build_branch(join)?;

    b.set_region(bb);
    let v_b = b.build_int_const(2u64, NodeOutputType::I64)?;
    b.write_variable(&var, v_b)?;
    b.build_branch(join)?;

    b.set_region(join);
    let merged = b.read_variable(&var)?;
    b.build_return(Some(merged), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // Anchor on the phi the Return consumes.
    let phi = fg.node_for_output(fg.node_inputs(find_return(&fg))[2]);
    assert!(matches!(fg.node_kind(phi), NodeKind::Phi));
    assert_eq!(fg.node_inputs(phi).len(), 3, "token + 2 values");

    // Surgery: redirect phi value input #2 to equal value input #1, so the
    // phi now has two identical (but distinct-from-each-other-at-build)
    // value outputs.
    let first_value = fg.node_inputs(phi)[1];
    let input2_id = fg.node_input_id_at(phi, 2)?;
    fg.update_input(input2_id, first_value);
    assert_eq!(fg.node_inputs(phi)[1], fg.node_inputs(phi)[2], "both values equal");

    let changed = PhiCollapse
        .optimize(&mut fg, &crate::opt::OptCtx::empty())?
        .changed();
    assert!(changed, "all-equal phi must collapse");

    let ret_val = fg.node_inputs(find_return(&fg))[2];
    assert_eq!(ret_val, first_value, "Return must rewire to the shared value");
    Ok(())
}

// ── loop-carried self-ref collapses ─────────────────────────────────────────

/// A loop-carried phi `[token, x, phi_self]` collapses to `x` — the
/// self-reference is discarded by Braun's rule.
#[test]
fn loop_carried_self_ref_phi_collapses() -> crate::opt::Result<()> {
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region()?;
    let join = b.create_region()?;
    b.set_entry_region(entry)?;

    b.set_region(entry);
    b.build_branch(join)?;

    b.set_region(join);
    let read_back = b.read_variable(&var)?;
    b.build_return(Some(read_back), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let phi = find_var_phi(&fg, var);
    let phi_inputs_pre = fg.node_inputs(phi);
    let initial_value = phi_inputs_pre[1];
    let region = fg.output_definition(phi_inputs_pre[0]).0;

    // Surgery: add a second distinct ctrl predecessor (the join's own
    // ctrl-out, a self-loop), and feed every phi over that region its OWN
    // output for the new slot (the loop back-edge self-ref).
    let region_outputs = fg.node_outputs(region);
    let region_ctrl_out = region_outputs[0];
    let region_phi_out = region_outputs[1];
    fg.add_node_input(region, region_ctrl_out)?;
    let phi_consumers: Vec<NodeId> = fg.output_uses(region_phi_out).map(|(n, _)| n).collect();
    for p in phi_consumers {
        let self_out = fg.node_outputs_exact::<1>(p)?[0];
        fg.add_node_input(p, self_out)?;
    }
    assert_eq!(
        fg.node_inputs(phi).len(),
        3,
        "[token, initial, self-ref] after surgery"
    );

    PhiCollapse.optimize(&mut fg, &crate::opt::OptCtx::empty())?;

    let ret_val = fg.node_inputs(find_return(&fg))[2];
    assert_eq!(
        ret_val, initial_value,
        "Return must rewire to the non-self-referential value"
    );
    Ok(())
}

// ── genuine 2-distinct phi is left unchanged ────────────────────────────────

/// A genuine merge of two DISTINCT values must NOT collapse.
#[test]
fn genuine_two_value_phi_unchanged() -> crate::opt::Result<()> {
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region()?;
    let a = b.create_region()?;
    let bb = b.create_region()?;
    let join = b.create_region()?;
    b.set_entry_region(entry)?;

    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, a, bb)?;

    b.set_region(a);
    let v_a = b.build_int_const(1u64, NodeOutputType::I64)?;
    b.write_variable(&var, v_a)?;
    b.build_branch(join)?;

    b.set_region(bb);
    let v_b = b.build_int_const(2u64, NodeOutputType::I64)?;
    b.write_variable(&var, v_b)?;
    b.build_branch(join)?;

    b.set_region(join);
    let merged = b.read_variable(&var)?;
    b.build_return(Some(merged), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // The Return reads the merged VarPhi output; capture it directly
    // (the builder may layer phis, so anchor on the value the Return
    // actually consumes rather than on `find_var_phi`).
    let phi_out = fg.node_inputs(find_return(&fg))[2];
    let phi = fg.node_for_output(phi_out);
    assert!(
        matches!(fg.node_kind(phi), NodeKind::Phi),
        "Return's value must be produced by a VarPhi, got {:?}",
        fg.node_kind(phi)
    );
    // Sanity: the phi merges two distinct values.
    let phi_inputs = fg.node_inputs(phi);
    assert_eq!(phi_inputs.len(), 3, "token + 2 distinct values");
    assert_ne!(phi_inputs[1], phi_inputs[2], "the two values are distinct");

    // Other single-pred phis in the graph (entry/branch MemPhis) may
    // collapse, so the overall result can be `Changed`; what matters is
    // that the *genuine* 2-distinct join phi survives untouched.
    PhiCollapse.optimize(&mut fg, &crate::opt::OptCtx::empty())?;

    let ret_val_after = fg.node_inputs(find_return(&fg))[2];
    assert_eq!(
        ret_val_after, phi_out,
        "Return must still read the genuine 2-distinct phi output"
    );
    assert!(
        matches!(fg.node_kind(phi), NodeKind::Phi),
        "the genuine join phi must still be a Phi"
    );
    Ok(())
}

// ── MemPhi collapses the same way ───────────────────────────────────────────

/// A MemPhi with a single reachable predecessor collapses like a VarPhi.
#[test]
fn single_value_mem_phi_collapses() -> crate::opt::Result<()> {
    let mut b = FunctionBuilder::empty()?;
    let entry = b.create_region()?;
    let body = b.create_region()?;
    b.set_entry_region(entry)?;
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    b.build_branch(body)?;
    b.set_region(body);
    let addr = b.build_int_const(0x1000u64, NodeOutputType::I64)?;
    let data = b.build_int_const(0x42u64, NodeOutputType::I64)?;
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // Anchor on the Store node (its memory input is the body MemPhi).
    let store = fg
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Store(_)))
        .expect("Store present");
    // Store inputs: [mem, addr, data]; the memory input is slot 0.
    let store_mem_in_before = fg.node_inputs(store)[0];
    let body_mem_phi = fg.node_for_output(store_mem_in_before);
    assert!(
        matches!(fg.node_kind(body_mem_phi), NodeKind::MemPhi),
        "Store's memory input must be a MemPhi pre-pass"
    );
    assert_eq!(fg.node_inputs(body_mem_phi).len(), 2, "token + 1 memory value");

    let changed = PhiCollapse
        .optimize(&mut fg, &crate::opt::OptCtx::empty())?
        .changed();
    assert!(changed, "single-value MemPhi must collapse");

    // After the cascade collapses every single-pred MemPhi (body then
    // entry), the Store's memory input no longer flows through any MemPhi —
    // it reads the function's InitialMemory directly.
    let store_mem_in = fg.node_inputs(store)[0];
    let mem_producer = fg.node_for_output(store_mem_in);
    assert!(
        !matches!(fg.node_kind(mem_producer), NodeKind::MemPhi),
        "Store's memory input must rewire past every collapsed MemPhi, got {:?}",
        fg.node_kind(mem_producer)
    );
    assert!(
        matches!(fg.node_kind(mem_producer), NodeKind::InitialMemory),
        "Store's memory input must reach InitialMemory, got {:?}",
        fg.node_kind(mem_producer)
    );
    Ok(())
}

// ── validates after collapse ────────────────────────────────────────────────

/// After collapsing a single-value phi, the graph still validates.
#[test]
fn collapse_then_validates() -> crate::opt::Result<()> {
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region()?;
    let join = b.create_region()?;
    b.set_entry_region(entry)?;
    b.set_region(entry);
    b.build_branch(join)?;
    b.set_region(join);
    let read_back = b.read_variable(&var)?;
    b.build_return(Some(read_back), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    PhiCollapse.optimize(&mut fg, &crate::opt::OptCtx::empty())?;
    strider_ir::validate::validate(&fg, fg.entry().unwrap())
        .map_err(|e| anyhow::anyhow!("post-PhiCollapse validation failed: {e:?}"))?;
    Ok(())
}
