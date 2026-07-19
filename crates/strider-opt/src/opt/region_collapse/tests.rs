use super::*;
use strider_ir::node::NodeKind;
use strider_ir::{IRBuilderExt, IRWalker};
use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR, reg_vn};

use crate::{OptimizerPipeline, PhiCollapse};

#[test]
fn single_input_region_collapses() -> crate::Result<()> {
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region_all()?;
    let body = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    b.build_branch(body)?;
    b.set_region(body);
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let body_region = fg
        .graph()
        .all_node_ids()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Region))
        .find(|&n| fg.node_inputs(n).len() == 1)
        .expect("single-input body Region");
    let sole_ctrl_value = fg.node_inputs(body_region)[0];
    let body_ctrl_value = fg.node_outputs(body_region)[0];
    let ctrl_consumer = fg
        .graph()
        .value_uses(body_ctrl_value)
        .map(|(n, _)| n)
        .next()
        .expect("a control consumer of the body Region");

    let changed =
        crate::pipeline::run_one(&RegionCollapse, &mut fg, &mut crate::OptCtx::new(None))?
            .changed();
    assert!(changed, "single-input Region must collapse");

    let consumer_ctrl_value = fg.node_inputs(ctrl_consumer)[0];
    assert_eq!(
        consumer_ctrl_value, sole_ctrl_value,
        "control consumer must rewire past the collapsed Region"
    );
    Ok(())
}

#[test]
fn multi_input_region_unchanged() -> crate::Result<()> {
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region_all()?;
    let true_r = b.create_region_all()?;
    let false_r = b.create_region_all()?;
    let join = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let cond = b.build_boolean_const(true);
    b.build_if(cond, true_r, false_r)?;
    b.set_region(true_r);
    b.build_branch(join)?;
    b.set_region(false_r);
    b.build_branch(join)?;
    b.set_region(join);
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let join_node = fg
        .graph()
        .all_node_ids()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Region))
        .max_by_key(|&n| fg.node_inputs(n).len())
        .expect("a join Region");
    assert_eq!(fg.node_inputs(join_node).len(), 2, "2-way join");
    let join_ctrl_value = fg.node_outputs(join_node)[0];
    let consumer_before = fg
        .graph()
        .value_uses(join_ctrl_value)
        .map(|(n, _)| n)
        .next()
        .expect("join consumer");

    // Not asserting on the pass result: the entry/branch single-input Regions
    // do collapse, so `Changed` is expected regardless of the join.
    crate::pipeline::run_one(&RegionCollapse, &mut fg, &mut crate::OptCtx::new(None))?;

    let consumer_after = fg
        .graph()
        .value_uses(join_ctrl_value)
        .map(|(n, _)| n)
        .next()
        .expect("join consumer still present");
    assert_eq!(
        consumer_before, consumer_after,
        "2-way join's control output must keep its consumer"
    );
    Ok(())
}

/// An orphan phi-token consumer is unreachable from entry, so it must not count
/// as a live use; otherwise it pins the Region forever.
#[test]
fn orphan_phi_consumer_does_not_block_detach() -> crate::Result<()> {
    use strider_ir::node::{ValueKind, ValueType};

    // `fn() { branch body; body: return; }`, so `body` is a single-pred Region
    // whose only control consumer is the Return.
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region_all()?;
    let body = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    b.build_branch(body)?;
    b.set_region(body);
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let body_region = fg
        .graph()
        .all_node_ids()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Region))
        .find(|&n| fg.node_inputs(n).len() == 1)
        .expect("single-input body Region");

    // A builder-emitted dead VarPhi has exactly this shape: consumes the
    // phi-token, own output never read, hence unreachable from entry.
    let phi_token = fg.node_outputs(body_region)[1];
    let val = {
        let mut ef = strider_ir::EditFunction::new(&mut fg);
        ef.build_int_const(0u64, ValueType::I64)?
    };
    let orphan_phi = strider_ir_test_utils::sentinel_node(
        &mut fg,
        NodeKind::Phi,
        [phi_token, val],
        [ValueKind::Typed(ValueType::I64)],
    );
    assert!(
        !fg.walk().any(|n| n == orphan_phi),
        "fixture must leave the grafted Phi unreachable from entry"
    );

    // PhiCollapse takes out the body Region's reachable single-pred MemPhi,
    // leaving the orphan Phi as the sole remaining phi-token consumer, which is
    // the case RegionCollapse must not be blocked by.
    let mut p = OptimizerPipeline::new();
    p.add(PhiCollapse);
    p.add(RegionCollapse);
    p.run(&mut fg, &mut crate::OptCtx::new(None))?;

    assert!(
        !fg.walk().any(|n| n == orphan_phi),
        "orphan Phi stays unreachable residue in the arena"
    );
    assert_eq!(
        fg.node_inputs(body_region).len(),
        0,
        "Region must be detached despite the unreachable orphan phi-token consumer"
    );
    Ok(())
}

#[test]
fn collapse_with_phi_collapse_validates() -> crate::Result<()> {
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

    let mut p = OptimizerPipeline::new();
    p.add(PhiCollapse);
    p.add(RegionCollapse);
    // `run` validates at the end, so reaching here is the assertion.
    p.run(&mut fg, &mut crate::OptCtx::new(None))?;
    Ok(())
}
