use super::*;
use crate::pipeline::Optimizer;
use crate::{OptimizerPipeline, RedundantPhis};
use ir::FunctionBuilder;
use ir::node::{NodeKind, NodeOutputType};

/// Counts the reachable `CallOther` nodes whose recorded user-op name
/// matches `name`.  Filtering by reachability avoids mistakenly counting
/// the zero-input zombies that `CallOtherElide` leaves behind in the
/// arena.
fn count_reachable_call_other_named(
    fg: &ir::BuiltFunctionGraph,
    name: &str,
) -> usize {
    let reachable: std::collections::HashSet<_> = fg.preorder().collect();
    fg.all_node_ids()
        .filter(|n| reachable.contains(n))
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::CallOther { .. }))
        .filter(|&n| fg.graph.call_other_name(n) == Some(name))
        .count()
}

/// A graph with a single `CallOther` whose name is in `NO_OP_USER_OPS`
/// must be elided after one pass; the function's control / memory
/// chains must thread through what used to be the CallOther's slot.
#[test]
fn elides_callother_with_known_nop_name() -> crate::Result<()> {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    b.set_entry_region(entry)?;
    b.set_region(entry);
    let (co_id, _val) = b.build_call_other(0xdead, &[], None)?;
    // Pick a name from the no-op list so the pass should fire.
    b.body_mut()
        .graph
        .set_call_other_name(co_id, "setISAMode".to_string());
    b.build_return(None, &[])?;

    let mut fg = b.build()?;
    // Sanity: the CallOther exists and is reachable before the pass.
    assert_eq!(count_reachable_call_other_named(&fg, "setISAMode"), 1);

    let result = CallOtherElide.optimize(&mut fg.graph, fg.entry)?;
    assert!(
        result.changed(),
        "elision must report Changed when at least one CallOther is removed",
    );
    assert_eq!(
        count_reachable_call_other_named(&fg, "setISAMode"),
        0,
        "the elided CallOther must no longer be reachable",
    );
    // The graph still validates after the rewrite.
    ir::validate::validate(&fg.graph, fg.entry)?;
    Ok(())
}

/// `CallOther` whose name is NOT in `NO_OP_USER_OPS` is preserved.
/// The pass returns `NoChange` and the node is still reachable.
#[test]
fn preserves_callother_with_unknown_name() -> crate::Result<()> {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    b.set_entry_region(entry)?;
    b.set_region(entry);
    let (co_id, _val) = b.build_call_other(0xbeef, &[], None)?;
    b.body_mut()
        .graph
        .set_call_other_name(co_id, "PleaseDoNotElideMe".to_string());
    b.build_return(None, &[])?;

    let mut fg = b.build()?;
    assert_eq!(
        count_reachable_call_other_named(&fg, "PleaseDoNotElideMe"),
        1
    );

    let result = CallOtherElide.optimize(&mut fg.graph, fg.entry)?;
    assert!(
        !result.changed(),
        "no name in NO_OP_USER_OPS matched → pass must report NoChange",
    );
    assert_eq!(
        count_reachable_call_other_named(&fg, "PleaseDoNotElideMe"),
        1,
    );
    ir::validate::validate(&fg.graph, fg.entry)?;
    Ok(())
}

/// Three CallOthers in a row, all with names in `NO_OP_USER_OPS`,
/// must all be elided in one sweep of the pass.
#[test]
fn elides_multiple_in_chain() -> crate::Result<()> {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    b.set_entry_region(entry)?;
    b.set_region(entry);
    for _ in 0..3 {
        let (co_id, _) = b.build_call_other(0x42, &[], None)?;
        b.body_mut()
            .graph
            .set_call_other_name(co_id, "setISAMode".to_string());
    }
    b.build_return(None, &[])?;

    let mut fg = b.build()?;
    assert_eq!(count_reachable_call_other_named(&fg, "setISAMode"), 3);

    let result = CallOtherElide.optimize(&mut fg.graph, fg.entry)?;
    assert!(result.changed());
    assert_eq!(
        count_reachable_call_other_named(&fg, "setISAMode"),
        0,
        "all three CallOthers in the chain must be elided in one pass",
    );
    ir::validate::validate(&fg.graph, fg.entry)?;
    Ok(())
}

/// A `CallOther` whose **value** output has a live consumer must NOT be
/// elided, even when its name is in `NO_OP_USER_OPS`.  Erasing the node
/// would leave the consumer dangling — the pass falls back to `NoChange`
/// in this case.  Exercises the defensive guard at `mod.rs` (the
/// `has_live_value_output` early-return).
#[test]
fn preserves_callother_when_value_output_has_consumer() -> crate::Result<()> {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    b.set_entry_region(entry)?;
    b.set_region(entry);

    // CallOther emits a 64-bit value output...
    let (co_id, value_out) =
        b.build_call_other(0xdead, &[], Some(NodeOutputType::U64))?;
    let value_out =
        value_out.expect("Some(NodeOutputType::U64) must produce a value output");
    // ...and the user-op name is one we'd normally elide.
    b.body_mut()
        .graph
        .set_call_other_name(co_id, "setISAMode".to_string());
    // The Return consumes the value, anchoring it as a live use.
    b.build_return(Some(value_out), &[])?;

    let mut fg = b.build()?;
    assert_eq!(count_reachable_call_other_named(&fg, "setISAMode"), 1);

    let result = CallOtherElide.optimize(&mut fg.graph, fg.entry)?;
    assert!(
        !result.changed(),
        "CallOther with a live value-output consumer must be preserved",
    );
    assert_eq!(
        count_reachable_call_other_named(&fg, "setISAMode"),
        1,
        "the CallOther must still be reachable after the pass",
    );
    ir::validate::validate(&fg.graph, fg.entry)?;
    Ok(())
}

/// `CallOtherElide` and `RedundantPhis` run in the same pipeline without
/// stepping on each other's invariants and produce the expected joint
/// shape (no surviving no-op CallOther, no single-input ControlState).
///
/// Note: the two passes are largely independent — CallOtherElide rewires
/// control/memory *through* the eliminated node, so it doesn't change
/// any ControlState's predecessor count, and therefore cannot itself
/// *enable* a RedundantPhis collapse that would otherwise be impossible.
/// What this test proves is the weaker but still useful property that
/// the two passes co-exist cleanly in a single pipeline run.
#[test]
fn coexists_with_redundant_phis_in_pipeline() -> crate::Result<()> {
    // Two-region function: entry → body, where `body`'s ControlState has
    // a single predecessor (entry).  The CallOther sits in the entry
    // region between the start of the function and the branch to body.
    // After the pipeline runs, the entry region's ctrl chain is just
    // `Entry → branch(body)`, body's single-pred ControlState is gone,
    // and validate succeeds.
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    let body = b.create_region()?;
    b.set_entry_region(entry)?;
    b.set_region(entry);
    let (co_id, _) = b.build_call_other(7, &[], None)?;
    b.body_mut()
        .graph
        .set_call_other_name(co_id, "setISAMode".to_string());
    b.build_branch(body)?;
    b.set_region(body);
    b.build_return(None, &[])?;

    let mut fg = b.build()?;

    // Run a pipeline: CallOtherElide first, then RedundantPhis sees the
    // single-pred ControlState and collapses it.
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(CallOtherElide);
    pipeline.add(RedundantPhis);
    pipeline.run(&mut fg.graph, fg.entry)?;

    // No `setISAMode` CallOther survives.
    assert_eq!(count_reachable_call_other_named(&fg, "setISAMode"), 0);

    // Reachable single-input `ControlState` nodes must be 0 after the
    // composed run (RedundantPhis bypasses them).
    let reachable: std::collections::HashSet<_> = fg.preorder().collect();
    let single_pred_cs = fg
        .all_node_ids()
        .filter(|n| reachable.contains(n))
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::ControlState))
        .filter(|&n| fg.graph.node_inputs(n).len() == 1)
        .count();
    assert_eq!(
        single_pred_cs, 0,
        "RedundantPhis must collapse the now-single-pred ControlState left \
         after elision",
    );
    Ok(())
}
