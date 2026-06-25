use super::*;
use strider_ir::node::NodeKind;
use strider_ir::{IRBuilderExt, IRWalker};
use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR, reg_vn};

use crate::pipeline::OptimizerTestExt;
use crate::{OptimizerPipeline, PhiCollapse};

// ── single-input Region collapses ───────────────────────────────────────────

/// A single-input Region's control consumers must be rewired to its lone
/// control input.
#[test]
fn single_input_region_collapses() -> crate::Result<()> {
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region()?;
    let body = b.create_region()?;
    b.set_entry_region(entry)?;
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
    // The Return consumes the body Region's control output.
    let ctrl_consumer = fg
        .graph()
        .value_uses(body_ctrl_value)
        .map(|(n, _)| n)
        .next()
        .expect("a control consumer of the body Region");

    let changed = RegionCollapse
        .run_one(&mut fg, &mut crate::OptCtx::new(None))?
        .changed();
    assert!(changed, "single-input Region must collapse");

    // The consumer's control input now points at the Region's predecessor.
    let consumer_ctrl_value = fg.node_inputs(ctrl_consumer)[0];
    assert_eq!(
        consumer_ctrl_value, sole_ctrl_value,
        "control consumer must rewire past the collapsed Region"
    );
    Ok(())
}

// ── multi-input Region untouched ────────────────────────────────────────────

/// A multi-predecessor join Region must NOT collapse.
#[test]
fn multi_input_region_unchanged() -> crate::Result<()> {
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region()?;
    let true_r = b.create_region()?;
    let false_r = b.create_region()?;
    let join = b.create_region()?;
    b.set_entry_region(entry)?;
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

    // The 2-way join doesn't collapse; the entry/branch single-input
    // Regions DO, so the overall result may be Changed — but the join's
    // own control output must keep its consumer.
    RegionCollapse.run_one(&mut fg, &mut crate::OptCtx::new(None))?;

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

// ── orphan phi-token consumers don't pin the Region ─────────────────────────

/// A single-pred Region whose control output ends up unused and whose
/// phi-token is consumed ONLY by an unreachable orphan `Phi` must still be
/// detached (its input edge cleared): an orphan phi-token consumer is not
/// reachable from entry, so it must not count as a live use that pins the
/// Region forever.
#[test]
fn orphan_phi_consumer_does_not_block_detach() -> crate::Result<()> {
    use strider_ir::node::{ValueKind, ValueType};

    // `fn() { branch body; body: return; }` — `body` is a single-pred
    // Region whose only control consumer is the Return.
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region()?;
    let body = b.create_region()?;
    b.set_entry_region(entry)?;
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

    // Graft an UNREACHABLE orphan Phi consuming the body Region's phi-token
    // (a builder-emitted dead VarPhi has exactly this shape).  Its own value
    // output is never read, so it is unreachable from entry.
    let phi_token = fg.node_outputs(body_region)[1];
    let val = {
        let mut ef = strider_ir::EditFunction::new(&mut fg)?;
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

    // `PhiCollapse` collapses + detaches the body Region's reachable
    // single-pred `MemPhi`, leaving the orphan Phi as the SOLE remaining
    // phi-token consumer.  `RegionCollapse` then rewires the body Region's
    // control consumer (Return) to the entry predecessor and must detach the
    // Region despite that lone unreachable orphan consumer.
    let mut p = OptimizerPipeline::new();
    p.add(PhiCollapse);
    p.add(RegionCollapse);
    p.run(&mut fg, &mut crate::OptCtx::new(None))?;

    // The orphan Phi is still the lone phi-token consumer, but the Region's
    // input must have been detached anyway.
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

// ── collapse + PhiCollapse over single-pred join validates ──────────────────

/// After RegionCollapse + PhiCollapse over a single-predecessor join, the
/// graph validates.
#[test]
fn collapse_with_phi_collapse_validates() -> crate::Result<()> {
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

    let mut p = OptimizerPipeline::new();
    p.add(PhiCollapse);
    p.add(RegionCollapse);
    p.run(&mut fg, &mut crate::OptCtx::new(None))?;
    // pipeline.run validates at the end; reaching here means it's valid.
    Ok(())
}
