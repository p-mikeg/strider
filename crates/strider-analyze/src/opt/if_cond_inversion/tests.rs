//! Unit tests for the [`IfCondInversion`] canonicalisation pass.

use super::IfCondInversion;
use crate::opt::ConstantFold;
use crate::opt::error::Result;
use crate::opt::pipeline::OptimizerRaw;
use crate::opt::test_support::find_unique_if;

use strider_ir::FunctionBuilder;
use strider_ir::node::{NodeKind, NodeOutputType};

/// Builds `if (!cond) { return 1 } else { return 2 }`, where `cond` is a
/// fresh boolean variable read from a register.  Returns the graph and
/// the `If` node id for downstream assertions.
fn build_if_with_neg_cond() -> Result<(strider_ir::BuiltFunctionGraph, strider_ir::node::NodeId)> {
    let cond_vn = strider_ir::test_utils::reg_vn(0x1000, 1);
    let mut b = FunctionBuilder::new_raw(vec![cond_vn], &[], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    let t = b.create_region()?;
    let f = b.create_region()?;

    b.set_entry_region(entry)?;
    b.set_region(entry);
    b.set_lift_addr(Some(strider_ir::test_utils::SENTINEL_LIFT_ADDR));
    let raw = b.read_variable(&cond_vn)?;
    let cond_bool = b.convert_to_bool_if_needed(raw)?;
    let neg_cond = b.build_boolean_unary_operation(cond_bool, strider_ir::BoolUnaryOp::Neg)?;
    b.build_if(neg_cond, t, f)?;

    b.set_region(t);
    let one = b.build_int_const(1u64, NodeOutputType::U64)?;
    b.build_return(Some(one), &[])?;

    b.set_region(f);
    let two = b.build_int_const(2u64, NodeOutputType::U64)?;
    b.build_return(Some(two), &[])?;
    b.set_lift_addr(None);

    let fg = b.build()?;
    let if_node = find_unique_if((&fg).into());
    Ok((fg, if_node))
}

/// Returns the cond input (input slot 1) of the given `If` node.
fn if_cond_kind(fg: &strider_ir::BuiltFunctionGraph, if_node: strider_ir::node::NodeId) -> NodeKind {
    let [_ctrl, cond_out] = fg
        .graph
        .node_inputs_exact::<2>(if_node)
        .expect("If has exactly two inputs");
    *fg.kind_of_output(cond_out)
}

#[test]
fn if_with_bool_neg_cond_is_canonicalised() -> Result<()> {
    let (mut fg, if_node) = build_if_with_neg_cond()?;
    // Before: cond is BoolUnaryOp::Neg.
    assert!(matches!(
        if_cond_kind(&fg, if_node),
        NodeKind::BoolUnaryOp(strider_ir::BoolUnaryOp::Neg)
    ));

    let r = IfCondInversion.optimize_raw(&mut fg.graph, fg.entry)?;
    assert!(r.changed());

    // After: cond is the inner CastToBool (the BoolNeg's input was the
    // CastToBool of the register read).  No `BoolUnaryOp::Neg` remains
    // on the If's cond input.
    assert!(!matches!(
        if_cond_kind(&fg, if_node),
        NodeKind::BoolUnaryOp(strider_ir::BoolUnaryOp::Neg)
    ));
    Ok(())
}

#[test]
fn idempotent_after_one_application() -> Result<()> {
    let (mut fg, _if_node) = build_if_with_neg_cond()?;
    let first = IfCondInversion.optimize_raw(&mut fg.graph, fg.entry)?;
    assert!(first.changed());
    let second = IfCondInversion.optimize_raw(&mut fg.graph, fg.entry)?;
    assert!(!second.changed(), "second pass must be a no-op");
    Ok(())
}

#[test]
fn double_neg_collapses_after_constant_fold() -> Result<()> {
    // Build `if (!!cond) { ... }`.  After ConstantFold's
    // `BoolNeg(BoolNeg(x)) → x` rule, the cond is bare `cond`; after
    // IfCondInversion (running on the same fixed-point loop in real
    // pipelines) the If is canonical with no swap.  Pin the
    // even-parity-no-swap invariant.
    let cond_vn = strider_ir::test_utils::reg_vn(0x1000, 1);
    let mut b = FunctionBuilder::new_raw(vec![cond_vn], &[], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    let t = b.create_region()?;
    let f = b.create_region()?;
    b.set_entry_region(entry)?;
    b.set_region(entry);
    b.set_lift_addr(Some(strider_ir::test_utils::SENTINEL_LIFT_ADDR));
    let raw = b.read_variable(&cond_vn)?;
    let cond_bool = b.convert_to_bool_if_needed(raw)?;
    let n1 = b.build_boolean_unary_operation(cond_bool, strider_ir::BoolUnaryOp::Neg)?;
    let n2 = b.build_boolean_unary_operation(n1, strider_ir::BoolUnaryOp::Neg)?;
    b.build_if(n2, t, f)?;
    b.set_region(t);
    let one = b.build_int_const(1u64, NodeOutputType::U64)?;
    b.build_return(Some(one), &[])?;
    b.set_region(f);
    let two = b.build_int_const(2u64, NodeOutputType::U64)?;
    b.build_return(Some(two), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // ConstantFold first: collapses `!!x → x`.
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize_raw(&mut fg.graph, fg.entry)?.changed();
    }
    // After ConstantFold the cond is no longer `BoolNeg`, so
    // IfCondInversion must NOT fire.  Even-parity → no branch swap.
    let r = IfCondInversion.optimize_raw(&mut fg.graph, fg.entry)?;
    assert!(
        !r.changed(),
        "IfCondInversion must be a no-op after !!x simplification — even parity preserves direct layout"
    );
    Ok(())
}

#[test]
fn swap_consumers_preserves_value_semantics() -> Result<()> {
    // The pass must swap the consumers of the two `If` control outputs
    // alongside dropping the `BoolNeg`.  Pin this by recording the
    // `ControlState` consumer of each output before the pass and
    // verifying they are now consumed in the swapped slots.
    let (mut fg, if_node) = build_if_with_neg_cond()?;
    let consumer_of = |fg: &strider_ir::BuiltFunctionGraph, out: strider_ir::node::NodeOutputId| -> strider_ir::node::NodeId {
        let (consumer, _idx) = fg
            .graph
            .output_uses(out)
            .next()
            .expect("each If output has exactly one consumer in this fixture");
        consumer
    };
    let [out0_pre, out1_pre] = fg.node_outputs_exact::<2>(if_node)?;
    let pre_true_consumer = consumer_of(&fg, out0_pre);
    let pre_false_consumer = consumer_of(&fg, out1_pre);
    assert_ne!(
        pre_true_consumer, pre_false_consumer,
        "pre-pass consumers must be distinct ControlState nodes"
    );

    IfCondInversion.optimize_raw(&mut fg.graph, fg.entry)?;

    let [out0_post, out1_post] = fg.node_outputs_exact::<2>(if_node)?;
    let post_true_consumer = consumer_of(&fg, out0_post);
    let post_false_consumer = consumer_of(&fg, out1_post);

    // Post-swap: output[0]'s consumer is what used to be output[1]'s,
    // and vice versa.
    assert_eq!(
        post_true_consumer, pre_false_consumer,
        "after inversion, output[0]'s consumer was previously output[1]'s consumer"
    );
    assert_eq!(
        post_false_consumer, pre_true_consumer,
        "after inversion, output[1]'s consumer was previously output[0]'s consumer"
    );
    Ok(())
}

/// Regression: when the `If`'s
/// `BoolNeg(cond)` becomes dead after the rewrite (no other consumers
/// of the `BoolNeg` exist), its asm-fingerprint must be absorbed into
/// the surviving inner-cond node so the contributing-asm history is
/// preserved.  Without the fix, the inner cond would carry only its
/// own lift_addr; the BoolNeg's address would be silently dropped.
#[test]
fn bool_neg_fingerprint_absorbed_into_inner_cond() -> Result<()> {
    let cond_vn = strider_ir::test_utils::reg_vn(0x2000, 1);
    let mut b = FunctionBuilder::new_raw(vec![cond_vn], &[], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    let t = b.create_region()?;
    let f = b.create_region()?;

    b.set_entry_region(entry)?;
    b.set_region(entry);
    // Stamp distinct lift_addrs on the cond producer and the BoolNeg so we
    // can observe absorption.
    b.set_lift_addr(Some(0x500));
    let raw = b.read_variable(&cond_vn)?;
    let cond_bool = b.convert_to_bool_if_needed(raw)?;
    b.set_lift_addr(Some(0x504));
    let neg_cond = b.build_boolean_unary_operation(cond_bool, strider_ir::BoolUnaryOp::Neg)?;
    b.set_lift_addr(Some(strider_ir::test_utils::SENTINEL_LIFT_ADDR));
    b.build_if(neg_cond, t, f)?;

    b.set_region(t);
    let one = b.build_int_const(1u64, NodeOutputType::U64)?;
    b.build_return(Some(one), &[])?;
    b.set_region(f);
    let two = b.build_int_const(2u64, NodeOutputType::U64)?;
    b.build_return(Some(two), &[])?;
    b.set_lift_addr(None);

    let mut fg = b.build()?;

    // Capture the BoolNeg's NodeId BEFORE optimisation; after the rewrite
    // it becomes dead but stays in the arena.
    let bool_neg_node = fg
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::BoolUnaryOp(strider_ir::BoolUnaryOp::Neg)))
        .expect("BoolUnaryOp::Neg present pre-pass");

    let r = IfCondInversion.optimize_raw(&mut fg.graph, fg.entry)?;
    assert!(r.changed());

    // The BoolNeg's fingerprint MUST have been absorbed into the
    // inner-cond node (the new If cond input's producer).
    let if_node = find_unique_if((&fg).into());
    let [_ctrl, cond_out] = fg.graph.node_inputs_exact::<2>(if_node)?;
    let inner_node = fg.graph.get_node_from_output(cond_out);
    let inner_fp = fg.graph.asm_fingerprint(inner_node);
    let bool_neg_fp = fg.graph.asm_fingerprint(bool_neg_node);
    for addr in bool_neg_fp {
        assert!(
            inner_fp.contains(addr),
            "BoolNeg's address {addr:#x} must survive into inner-cond fingerprint after \
             IfCondInversion: inner_fp={inner_fp:?}, bool_neg_fp={bool_neg_fp:?}"
        );
    }
    Ok(())
}
