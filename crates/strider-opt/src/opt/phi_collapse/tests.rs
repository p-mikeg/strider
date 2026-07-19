use super::*;
use strider_ir::IRBuilderExt;
use strider_ir::node::{NodeKind, ValueType};
use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR, reg_vn};

fn find_return(fg: &strider_ir::Function) -> NodeId {
    fg.graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
        .expect("Return present")
}

fn find_var_phi(fg: &strider_ir::Function, var: rsleigh::Vn) -> NodeId {
    fg.graph()
        .all_node_ids()
        .find(|&n| {
            matches!(fg.node_kind(n), NodeKind::Phi)
                && fg.get_vn_for_value(fg.node_outputs(n)[0]) == Some(var)
        })
        .expect("VarPhi present")
}

#[test]
fn single_value_phi_collapses() -> crate::Result<()> {
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region_all()?;
    let join = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

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

    let changed =
        crate::pipeline::run_one(&PhiCollapse, &mut fg, &mut crate::OptCtx::new(None))?.changed();
    assert!(changed, "single-value phi must collapse");

    let ret_val = fg.node_inputs(find_return(&fg))[2];
    assert_eq!(
        ret_val, lone_value,
        "Return must rewire to the phi's only value input"
    );
    Ok(())
}

/// A real 2-predecessor join whose value inputs are the same `ValueId`.
///
/// The builder dedups two structurally-equal writes into one phi input, so the
/// `[token, V, V]` shape has to be produced by surgery on a 2-distinct join.
#[test]
fn multi_value_all_equal_phi_collapses() -> crate::Result<()> {
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region_all()?;
    let a = b.create_region_all()?;
    let bb = b.create_region_all()?;
    let join = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, a, bb)?;

    b.set_region(a);
    let v_a = b.build_int_const(1u64, ValueType::I64)?;
    b.write_variable(&var, v_a)?;
    b.build_branch(join)?;

    b.set_region(bb);
    let v_b = b.build_int_const(2u64, ValueType::I64)?;
    b.write_variable(&var, v_b)?;
    b.build_branch(join)?;

    b.set_region(join);
    let merged = b.read_variable(&var)?;
    b.build_return(Some(merged), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // Anchor on the phi the Return consumes.
    let phi = fg.producer(fg.node_inputs(find_return(&fg))[2]);
    assert!(matches!(fg.node_kind(phi), NodeKind::Phi));
    assert_eq!(fg.node_inputs(phi).len(), 3, "token + 2 values");

    let first_value = fg.node_inputs(phi)[1];
    let input2_id = fg.graph().node_input_id_at(phi, 2)?;
    fg.graph_mut().update_input(input2_id, first_value);
    assert_eq!(
        fg.node_inputs(phi)[1],
        fg.node_inputs(phi)[2],
        "both values equal"
    );

    let changed =
        crate::pipeline::run_one(&PhiCollapse, &mut fg, &mut crate::OptCtx::new(None))?.changed();
    assert!(changed, "all-equal phi must collapse");

    let ret_val = fg.node_inputs(find_return(&fg))[2];
    assert_eq!(
        ret_val, first_value,
        "Return must rewire to the shared value"
    );
    Ok(())
}

/// `entry -> mid -> tail`, a phi per join. Collapsing mid's phi leaves tail's
/// trivial too, so the cascade must reach all the way to the base `InitialVar`
/// with no `Phi` left on the value path.
#[test]
fn chained_single_pred_phis_cascade_to_base_value() -> crate::Result<()> {
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region_all()?;
    let mid = b.create_region_all()?;
    let tail = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    b.set_region(entry);
    b.build_branch(mid)?;
    // Each read materialises a phi; tail's phi takes mid's phi as its value.
    b.set_region(mid);
    let _mid_read = b.read_variable(&var)?;
    b.build_branch(tail)?;
    b.set_region(tail);
    let read_back = b.read_variable(&var)?;
    b.build_return(Some(read_back), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let changed =
        crate::pipeline::run_one(&PhiCollapse, &mut fg, &mut crate::OptCtx::new(None))?.changed();
    assert!(changed, "chained trivial phis must collapse");

    let ret_val = fg.node_inputs(find_return(&fg))[2];
    let producer = fg.producer(ret_val);
    assert!(
        matches!(fg.node_kind(producer), NodeKind::InitialVar(v) if fg.initial_vn(*v) == var),
        "cascade must land on the base InitialVar, got {:?}",
        fg.node_kind(producer)
    );
    Ok(())
}

/// `[token, x, phi_self]` collapses to `x`; Braun's rule discards the self-ref.
#[test]
fn loop_carried_self_ref_phi_collapses() -> crate::Result<()> {
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region_all()?;
    let join = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

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
    let region = fg.value_definition(phi_inputs_pre[0]).0;

    // Build the back edge by hand: a self-loop ctrl predecessor, and for that
    // new slot every phi over the region gets its own output.
    let region_outputs = fg.node_outputs(region);
    let region_ctrl_value = region_outputs[0];
    let region_phi_value = region_outputs[1];
    fg.graph_mut().add_node_input(region, region_ctrl_value);
    let phi_consumers: Vec<NodeId> = fg
        .graph()
        .value_uses(region_phi_value)
        .map(|(n, _)| n)
        .collect();
    for p in phi_consumers {
        let self_value = fg.node_outputs_exact::<1>(p)?[0];
        fg.graph_mut().add_node_input(p, self_value);
    }
    assert_eq!(
        fg.node_inputs(phi).len(),
        3,
        "[token, initial, self-ref] after surgery"
    );

    crate::pipeline::run_one(&PhiCollapse, &mut fg, &mut crate::OptCtx::new(None))?;

    let ret_val = fg.node_inputs(find_return(&fg))[2];
    assert_eq!(
        ret_val, initial_value,
        "Return must rewire to the non-self-referential value"
    );
    Ok(())
}

#[test]
fn genuine_two_value_phi_unchanged() -> crate::Result<()> {
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region_all()?;
    let a = b.create_region_all()?;
    let bb = b.create_region_all()?;
    let join = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, a, bb)?;

    b.set_region(a);
    let v_a = b.build_int_const(1u64, ValueType::I64)?;
    b.write_variable(&var, v_a)?;
    b.build_branch(join)?;

    b.set_region(bb);
    let v_b = b.build_int_const(2u64, ValueType::I64)?;
    b.write_variable(&var, v_b)?;
    b.build_branch(join)?;

    b.set_region(join);
    let merged = b.read_variable(&var)?;
    b.build_return(Some(merged), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // The builder may layer phis, so anchor on the value the Return actually
    // consumes rather than on `find_var_phi`.
    let phi_value = fg.node_inputs(find_return(&fg))[2];
    let phi = fg.producer(phi_value);
    assert!(
        matches!(fg.node_kind(phi), NodeKind::Phi),
        "Return's value must be produced by a VarPhi, got {:?}",
        fg.node_kind(phi)
    );
    let phi_inputs = fg.node_inputs(phi);
    assert_eq!(phi_inputs.len(), 3, "token + 2 distinct values");
    assert_ne!(phi_inputs[1], phi_inputs[2], "the two values are distinct");

    // Not asserting on the pass result: other single-pred phis (entry/branch
    // MemPhis) do collapse, so `Changed` is expected either way.
    crate::pipeline::run_one(&PhiCollapse, &mut fg, &mut crate::OptCtx::new(None))?;

    let ret_val_after = fg.node_inputs(find_return(&fg))[2];
    assert_eq!(
        ret_val_after, phi_value,
        "Return must still read the genuine 2-distinct phi output"
    );
    assert!(
        matches!(fg.node_kind(phi), NodeKind::Phi),
        "the genuine join phi must still be a Phi"
    );
    Ok(())
}

#[test]
fn single_value_mem_phi_collapses() -> crate::Result<()> {
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region_all()?;
    let body = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    b.build_branch(body)?;
    b.set_region(body);
    let addr = b.build_int_const(0x1000u64, ValueType::I64)?;
    let data = b.build_int_const(0x42u64, ValueType::I64)?;
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let store = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Store(_)))
        .expect("Store present");
    // Store inputs: [mem, addr, data]; the memory input is slot 0.
    let store_mem_value_before = fg.node_inputs(store)[0];
    let body_mem_phi = fg.producer(store_mem_value_before);
    assert!(
        matches!(fg.node_kind(body_mem_phi), NodeKind::MemPhi),
        "Store's memory input must be a MemPhi pre-pass"
    );
    assert_eq!(
        fg.node_inputs(body_mem_phi).len(),
        2,
        "token + 1 memory value"
    );

    let changed =
        crate::pipeline::run_one(&PhiCollapse, &mut fg, &mut crate::OptCtx::new(None))?.changed();
    assert!(changed, "single-value MemPhi must collapse");

    // The cascade collapses body's MemPhi and then entry's, so the Store ends
    // up reading InitialMemory directly.
    let store_mem_value = fg.node_inputs(store)[0];
    let mem_producer = fg.producer(store_mem_value);
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

#[test]
fn collapse_then_validates() -> crate::Result<()> {
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region_all()?;
    let join = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);
    b.build_branch(join)?;
    b.set_region(join);
    let read_back = b.read_variable(&var)?;
    b.build_return(Some(read_back), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    crate::pipeline::run_one(&PhiCollapse, &mut fg, &mut crate::OptCtx::new(None))?;
    strider_ir::validate::validate(&fg)
        .map_err(|e| anyhow::anyhow!("post-PhiCollapse validation failed: {e:?}"))?;
    Ok(())
}
