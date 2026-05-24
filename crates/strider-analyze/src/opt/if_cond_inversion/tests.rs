//! Unit tests for the [`IfCondInversion`] canonicalisation pass.

use super::IfCondInversion;
use crate::opt::ConstantFold;
use crate::opt::error::Result;
use crate::opt::pipeline::Optimizer;
use crate::opt::test_support::find_unique_if;

use strider_ir::node::NodeKind;
use strider_ir_test_utils::RegisterSet;

/// Builds `if (!cond) { return 1 } else { return 2 }`, where `cond` is a
/// fresh boolean variable read from a register.  Returns the graph and
/// the `If` node id for downstream assertions.
fn build_if_with_neg_cond() -> Result<(strider_ir::Graph, strider_ir::node::NodeId)> {
    let cond_vn = strider_ir_test_utils::reg_vn(0x1000, 1);
    let (fg, if_node, ()) = RegisterSet::new()
        .tracked(cond_vn)
        .build_if_then_else_returns(|b| {
            let raw = b.read_variable(&cond_vn)?;
            let cond_bool = b.convert_to_bool_if_needed(raw)?;
            let neg_cond =
                b.build_boolean_unary_operation(cond_bool, strider_ir::BoolUnaryOp::Neg)?;
            Ok((neg_cond, ()))
        })?;
    Ok((fg, if_node))
}

/// Returns the cond input (input slot 1) of the given `If` node.
fn if_cond_kind(fg: &strider_ir::Graph, if_node: strider_ir::node::NodeId) -> NodeKind {
    let [_ctrl, cond_out] = fg
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

    let entry = fg.entry().unwrap();
    let r = IfCondInversion.optimize(fg.graph_mut(), entry)?;
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
    let entry = fg.entry().unwrap();
    let first = IfCondInversion.optimize(fg.graph_mut(), entry)?;
    assert!(first.changed());
    let entry = fg.entry().unwrap();
    let second = IfCondInversion.optimize(fg.graph_mut(), entry)?;
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
    let cond_vn = strider_ir_test_utils::reg_vn(0x1000, 1);
    let (mut fg, _if_node, ()) = RegisterSet::new()
        .tracked(cond_vn)
        .build_if_then_else_returns(|b| {
            let raw = b.read_variable(&cond_vn)?;
            let cond_bool = b.convert_to_bool_if_needed(raw)?;
            let n1 = b.build_boolean_unary_operation(cond_bool, strider_ir::BoolUnaryOp::Neg)?;
            let n2 = b.build_boolean_unary_operation(n1, strider_ir::BoolUnaryOp::Neg)?;
            Ok((n2, ()))
        })?;

    // ConstantFold first: collapses `!!x → x`.
    let mut changed = true;
    while changed {
        let entry = fg.entry().unwrap();
        changed = ConstantFold.optimize(fg.graph_mut(), entry)?.changed();
    }
    // After ConstantFold the cond is no longer `BoolNeg`, so
    // IfCondInversion must NOT fire.  Even-parity → no branch swap.
    let entry = fg.entry().unwrap();
    let r = IfCondInversion.optimize(fg.graph_mut(), entry)?;
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
    // `Region` consumer of each output before the pass and
    // verifying they are now consumed in the swapped slots.
    let (mut fg, if_node) = build_if_with_neg_cond()?;
    let consumer_of = |fg: &strider_ir::Graph, out: strider_ir::node::NodeOutputId| -> strider_ir::node::NodeId {
        let (consumer, _idx) = fg
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
        "pre-pass consumers must be distinct Region nodes"
    );

    let entry = fg.entry().unwrap();
    IfCondInversion.optimize(fg.graph_mut(), entry)?;

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
    let cond_vn = strider_ir_test_utils::reg_vn(0x2000, 1);
    let (mut fg, _if_node, ()) = RegisterSet::new()
        .tracked(cond_vn)
        .build_if_then_else_returns(|b| {
            // Stamp distinct lift_addrs on the cond producer and the
            // BoolNeg so we can observe absorption.
            b.set_lift_addr(Some(0x500));
            let raw = b.read_variable(&cond_vn)?;
            let cond_bool = b.convert_to_bool_if_needed(raw)?;
            b.set_lift_addr(Some(0x504));
            let neg_cond =
                b.build_boolean_unary_operation(cond_bool, strider_ir::BoolUnaryOp::Neg)?;
            b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
            Ok((neg_cond, ()))
        })?;

    // Capture the BoolNeg's NodeId BEFORE optimisation; after the rewrite
    // it becomes dead but stays in the arena.
    let bool_neg_node = fg
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::BoolUnaryOp(strider_ir::BoolUnaryOp::Neg)))
        .expect("BoolUnaryOp::Neg present pre-pass");

    let entry = fg.entry().unwrap();
    let r = IfCondInversion.optimize(fg.graph_mut(), entry)?;
    assert!(r.changed());

    // The BoolNeg's fingerprint MUST have been absorbed into the
    // inner-cond node (the new If cond input's producer).
    let if_node = find_unique_if(&fg);
    let [_ctrl, cond_out] = fg.node_inputs_exact::<2>(if_node)?;
    let inner_node = fg.get_node_from_output(cond_out);
    let inner_fp = fg.asm_fingerprint(inner_node);
    let bool_neg_fp = fg.asm_fingerprint(bool_neg_node);
    for addr in bool_neg_fp {
        assert!(
            inner_fp.contains(addr),
            "BoolNeg's address {addr:#x} must survive into inner-cond fingerprint after \
             IfCondInversion: inner_fp={inner_fp:?}, bool_neg_fp={bool_neg_fp:?}"
        );
    }
    Ok(())
}

/// Pins **which** node receives the absorbed fingerprint: the producer
/// of the BoolNeg's *input* (i.e. the new cond input's producer), not
/// the `If` node, not the BoolNeg itself, and not any unrelated reachable
/// node.  Guards against a buggy implementation that absorbs into the
/// wrong neighbour (e.g. unioning into `if_node` instead of the inner
/// cond, which would lose the contributing-asm history when the If gets
/// rewritten by a later pass).
#[test]
fn fingerprint_absorption_targets_inner_cond_producer_only() -> Result<()> {
    let cond_vn = strider_ir_test_utils::reg_vn(0x3000, 1);
    // Distinct addresses on the cond producer (0x800), the BoolNeg
    // (0x804), and the If (0x808) so we can prove the BoolNeg's
    // address lands on exactly one of the three.
    let (mut fg, _if_node, ()) = RegisterSet::new()
        .tracked(cond_vn)
        .build_if_then_else_returns(|b| {
            b.set_lift_addr(Some(0x800));
            let raw = b.read_variable(&cond_vn)?;
            let cond_bool = b.convert_to_bool_if_needed(raw)?;
            b.set_lift_addr(Some(0x804));
            let neg_cond =
                b.build_boolean_unary_operation(cond_bool, strider_ir::BoolUnaryOp::Neg)?;
            b.set_lift_addr(Some(0x808));
            Ok((neg_cond, ()))
        })?;

    // Identify pre-pass: the BoolNeg, the If, and the BoolNeg's input
    // producer (the "inner cond producer" that should receive the
    // absorbed fingerprint).
    let bool_neg_node = fg
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::BoolUnaryOp(strider_ir::BoolUnaryOp::Neg)))
        .expect("BoolNeg pre-pass");
    let if_node_pre = find_unique_if(&fg);
    let [bool_neg_input] = fg.node_inputs_exact::<1>(bool_neg_node)?;
    let inner_producer_pre = fg.get_node_from_output(bool_neg_input);

    // 0x804 (the BoolNeg's address) must be present on BoolNeg pre-pass,
    // absent on inner_producer_pre, and absent on if_node_pre.  Sanity-check
    // the fixture before running the pass.
    assert!(fg.asm_fingerprint(bool_neg_node).contains(&0x804));
    assert!(!fg.asm_fingerprint(inner_producer_pre).contains(&0x804));
    assert!(!fg.asm_fingerprint(if_node_pre).contains(&0x804));

    let entry = fg.entry().unwrap();
    let r = IfCondInversion.optimize(fg.graph_mut(), entry)?;
    assert!(r.changed());

    // After the pass, BoolNeg's address (0x804) must land on exactly the
    // inner producer — NOT on the If, and NOT on any sibling reachable
    // node that wasn't an ancestor of the cond input.
    assert!(
        fg.asm_fingerprint(inner_producer_pre).contains(&0x804),
        "BoolNeg's address 0x804 must be absorbed into the inner cond producer"
    );
    assert!(
        !fg.asm_fingerprint(if_node_pre).contains(&0x804),
        "BoolNeg's address 0x804 must NOT be absorbed into the If node"
    );

    // After absorption, the inner producer's fingerprint should contain
    // BOTH its own original address AND the BoolNeg's.
    let inner_fp = fg.asm_fingerprint(inner_producer_pre);
    assert!(
        inner_fp.contains(&0x804),
        "inner cond producer must carry the absorbed BoolNeg address 0x804"
    );
    Ok(())
}
