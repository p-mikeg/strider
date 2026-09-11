use super::*;
use strider_ir::node::{NodeKind, ValueType};
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

    // Both Regions are single-predecessor, so the Return ends up on `Entry`'s
    // control directly.
    let entry_node = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Entry))
        .expect("Entry node");
    let entry_ctrl = fg.node_outputs(entry_node)[0];
    let ret = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
        .expect("Return node");

    let changed =
        crate::pipeline::run_one(&RegionCollapse, &mut fg, &mut crate::OptCtx::new(None))?
            .changed();
    assert!(changed, "single-input Region must collapse");

    assert_eq!(
        fg.node_inputs(ret)[0],
        entry_ctrl,
        "the control consumer must rewire past every collapsed Region"
    );
    Ok(())
}

/// An empty ENTRY region takes its one control input from the `Entry` node, so
/// it collapses like any interior pass-through. That is what lets the CFG
/// builder emit an empty entry region for a leading zero-pcode-op insn
/// (`endbr64` / `paciasp`).
#[test]
fn empty_entry_region_collapses_through_entry_node() -> crate::Result<()> {
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

    let entry_node = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Entry))
        .expect("Entry node");
    let entry_ctrl = fg.node_outputs(entry_node)[0];
    let entry_region = fg
        .graph()
        .value_uses(entry_ctrl)
        .map(|(n, _)| n)
        .next()
        .expect("a Region consuming Entry's control");
    assert!(matches!(fg.node_kind(entry_region), NodeKind::Region));
    assert_eq!(
        fg.node_inputs(entry_region).len(),
        1,
        "entry Region's sole control input is the Entry node"
    );

    crate::default_pipeline().run(&mut fg, &mut crate::OptCtx::new(None))?;

    // The pipeline splices Entry's control past the empty region but does not
    // sweep the arena (the orchestrator's `retain_reachable` does that), so the
    // spliced-out region survives as unreachable residue.
    let reachable: std::collections::HashSet<_> = fg.walk().collect();
    assert!(
        !reachable.contains(&entry_region),
        "the empty entry Region must be unreachable (spliced out) after the pipeline"
    );
    assert!(
        fg.graph()
            .value_uses(fg.node_outputs(entry_node)[0])
            .any(|(c, _)| reachable.contains(&c)),
        "Entry's control must feed a live successor after the entry region is spliced out"
    );
    Ok(())
}

/// An empty entry region that is ALSO a loop
/// header has two control predecessors (the `Entry` node and the back-edge), so
/// its phi is a genuine merge (initial value vs loop-carried value).  Neither
/// `RegionCollapse` (multi-input) nor `PhiCollapse` (>1 distinct input) may
/// touch it, so both the entry region and its non-trivial phi stay REACHABLE
/// through the full pipeline.
#[test]
fn empty_entry_region_with_nontrivial_phi_survives() -> crate::Result<()> {
    let var = strider_ir_test_utils::reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region_all()?;
    let body = b.create_region_all()?;
    let exit = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    // Entry is the loop header: reading `var` forces a phi merging the incoming
    // argument with the value written on the back-edge.  A data-dependent
    // condition (not a const) keeps both edges live so the phi stays needed.
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let cur = b.read_variable(&var)?;
    let zero = b.build_int_const(0u64, strider_ir::node::ValueType::I64)?;
    let cond =
        b.build_int_cmp_operation(cur, zero, strider_ir::node::IntCmpOp::Equal, ValueType::I64)?;
    b.build_if(cond, body, exit)?;

    b.set_region(body);
    let one = b.build_int_const(1u64, ValueType::I64)?;
    let next =
        b.build_int_binary_operation(cur, one, strider_ir::node::IntBinaryOp::Add, ValueType::I64)?;
    b.write_variable(&var, next)?;
    b.build_branch(entry)?; // BACK-EDGE -> entry becomes a 2-predecessor join

    b.set_region(exit);
    let out = b.read_variable(&var)?;
    b.build_return(Some(out), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let entry_node = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Entry))
        .expect("Entry node");
    let entry_region = fg
        .graph()
        .value_uses(fg.node_outputs(entry_node)[0])
        .map(|(n, _)| n)
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Region))
        .expect("entry Region");
    assert!(
        fg.node_inputs(entry_region).len() >= 2,
        "loop-header entry region is a genuine multi-predecessor join"
    );

    crate::default_pipeline().run(&mut fg, &mut crate::OptCtx::new(None))?;

    let reachable: std::collections::HashSet<_> = fg.walk().collect();
    assert!(
        reachable.contains(&entry_region),
        "a multi-predecessor entry region must NOT collapse: it carries a real merge"
    );
    assert!(
        reachable
            .iter()
            .any(|&n| matches!(fg.node_kind(n), NodeKind::Phi)),
        "the non-trivial merge phi must survive the full pipeline"
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

    // `body` is a single-pred Region whose only control consumer is the
    // Return.
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

/// The pass is individually selectable, so it must leave a valid graph on its
/// own: a Region that survives the rewire is a second consumer of the spliced
/// control value.
#[test]
fn standalone_collapse_leaves_a_valid_graph() -> crate::Result<()> {
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

    crate::pipeline::run_one(&RegionCollapse, &mut fg, &mut crate::OptCtx::new(None))?;
    strider_ir::validate::validate(&fg)?;
    Ok(())
}

/// Two phis over one single-predecessor Region where the FIRST's sole input is
/// the second's output.  Collapsing off a pre-mutation snapshot replaces the
/// first with a value whose producer the same call kills two lines later.
#[test]
fn chained_phis_over_one_region_do_not_dangle() -> crate::Result<()> {
    let v1 = reg_vn(0x1000, 8);
    let v2 = reg_vn(0x1008, 8);
    let mut b = RegisterSet::new()
        .tracked(v1)
        .tracked(v2)
        .arg(v1)
        .arg(v2)
        .build_fn()?;
    let entry = b.create_region_all()?;
    let join = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    b.build_branch(join)?;

    b.set_region(join);
    let read1 = b.read_variable(&v1)?;
    let read2 = b.read_variable(&v2)?;
    let sum =
        b.build_int_binary_operation(read1, read2, strider_ir::IntBinaryOp::Add, ValueType::I64)?;
    b.build_return(Some(sum), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // Chain the phis: the builder cannot emit this shape, since a phi's inputs
    // are the predecessor's values, computed before any phi exists.
    let phi1 = fg.producer(read1);
    let phi2 = fg.producer(read2);
    assert!(matches!(fg.node_kind(phi1), NodeKind::Phi));
    assert!(matches!(fg.node_kind(phi2), NodeKind::Phi));
    let slot = fg.graph().node_input_id_at(phi1, 1)?;
    fg.graph_mut().update_input(slot, read2);

    crate::pipeline::run_one(&RegionCollapse, &mut fg, &mut crate::OptCtx::new(None))?;

    strider_ir::validate::validate(&fg).expect("IR must stay valid");
    Ok(())
}

/// A single-predecessor Region whose control output has no consumer still
/// collapses its phis and is killed, so reporting the control rewire's `false`
/// as the whole outcome hides two mutations from the pipeline's fixed point.
#[test]
fn phi_collapse_without_a_control_rewire_reports_changed() -> crate::Result<()> {
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region_all()?;
    let join = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    b.build_branch(join)?;
    b.set_region(join);
    let read = b.read_variable(&var)?;
    b.build_return(Some(read), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let phi = fg.producer(read);
    let join_node = fg.producer(fg.node_inputs(phi)[0]);
    assert!(matches!(fg.node_kind(join_node), NodeKind::Region));
    let ret = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
        .expect("Return");
    // Hang the Return off the join's own predecessor control, leaving the
    // join's control output unconsumed while its phi stays live.
    let pred_ctrl = fg.node_inputs(join_node)[0];
    let ret_ctrl_slot = fg.graph().node_input_id_at(ret, 0)?;
    fg.graph_mut().update_input(ret_ctrl_slot, pred_ctrl);

    let mut edit = crate::EditFunction::new(&mut fg);
    let result = RegionCollapse.try_collapse(&mut edit, join_node)?;
    assert!(!edit.is_live(phi), "the phi was collapsed and killed");
    assert!(
        result.changed(),
        "the phi collapse and the kill are mutations"
    );
    Ok(())
}
