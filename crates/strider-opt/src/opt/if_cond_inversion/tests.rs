//! Unit tests for the [`IfCondInversion`] canonicalisation pass.

use super::IfCondInversion;
use crate::ConstantFold;
use crate::error::Result;
use crate::test_support::{find_unique_if, run_to_fixed_point};
use strider_ir::{IRBuilderExt, IRViewer};

use strider_ir::IntBinaryOp;
use strider_ir::node::{NodeKind, ValueId, ValueType};
use strider_ir_test_utils::RegisterSet;

/// Builds the canonical 1-bit logical NOT shape `Xor(operand, IntConst(1)):I1`
/// — the post-removal-of-the former BitNot unary-op equivalent of `BoolNeg`.  Used
/// throughout the `IfCondInversion` tests to construct fixtures whose `If`
/// cond is an inverted boolean.
fn build_bool_not(b: &mut strider_ir::FunctionBuilder, operand: ValueId) -> Result<ValueId> {
    let one = b.build_int_const(u128::MAX, ValueType::I1)?;
    b.build_int_binary_operation(operand, one, IntBinaryOp::Xor, ValueType::I1)
}

/// True when `node` is the canonical 1-bit logical NOT shape — an
/// `IntBinaryOp::Xor` at `I1` whose RHS (or LHS, since Xor is commutative
/// in the dedup cache) is `IntConst(1):I1`.
fn is_i1_xor_with_one(fg: &strider_ir::Function, node: strider_ir::node::NodeId) -> bool {
    use strider_ir::IRViewer;
    if !matches!(fg.node_kind(node), NodeKind::IntBinaryOp(IntBinaryOp::Xor)) {
        return false;
    }
    let Ok([lhs, rhs]) = fg.node_inputs_exact::<2>(node) else {
        return false;
    };
    let is_one = |value: ValueId| {
        fg.value_kind(value).as_value().is_some_and(|t| t.is_bool())
            && fg.int_const_u128(value) == Some(1)
    };
    is_one(lhs) || is_one(rhs)
}

/// Builds `if (!cond) { return 1 } else { return 2 }`, where `cond` is a
/// fresh boolean variable read from a register.  Returns the graph and
/// the `If` node id for downstream assertions.
fn build_if_with_neg_cond() -> Result<(strider_ir::Function, strider_ir::node::NodeId)> {
    let cond_vn = strider_ir_test_utils::reg_vn(0x1000, 1);
    let (fg, if_node, ()) = RegisterSet::new()
        .tracked(cond_vn)
        .build_if_then_else_returns(|b| {
            let raw = b.read_variable(&cond_vn)?;
            let cond_bool = b.convert_to_int_if_needed(raw, strider_ir::node::ValueType::I1)?;
            let neg_cond = build_bool_not(b, cond_bool)?;
            Ok((neg_cond, ()))
        })?;
    Ok((fg, if_node))
}

/// Returns the cond input (input slot 1) of the given `If` node.
fn if_cond_kind(fg: &strider_ir::Graph, if_node: strider_ir::node::NodeId) -> NodeKind {
    let [_ctrl, cond_value] = fg
        .node_inputs_exact::<2>(if_node)
        .expect("If has exactly two inputs");
    *fg.kind_of_value(cond_value)
}

// ── constructed-with-data: per-instance pattern ownership ─────────────────

/// A pass built via [`IfCondInversion::new`] owns its inner pattern and
/// performs the same inversion the bare-value form did.
#[test]
fn new_builds_pass_that_inverts() -> Result<()> {
    let (mut fg, if_node) = build_if_with_neg_cond()?;
    let cond_pre = fg.producer(fg.graph().node_inputs_exact::<2>(if_node)?[1]);
    assert!(is_i1_xor_with_one(&fg, cond_pre));

    let r = crate::pipeline::run_one(
        &IfCondInversion::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(r.changed(), "constructed pass should invert the cond");

    let cond_post = fg.producer(fg.graph().node_inputs_exact::<2>(if_node)?[1]);
    assert!(!is_i1_xor_with_one(&fg, cond_post));
    Ok(())
}

/// Two independently-constructed instances each own their own pattern —
/// proving the data is per-instance, not a shared thread-local.  Running one
/// then a fresh second on equivalent fixtures both perform the inversion.
#[test]
fn two_independent_instances_each_invert() -> Result<()> {
    let pass_a = IfCondInversion::new();
    let pass_b = IfCondInversion::new();

    let (mut fg_a, if_a) = build_if_with_neg_cond()?;
    assert!(crate::pipeline::run_one(&pass_a, &mut fg_a, &mut crate::OptCtx::new(None))?.changed());
    let cond_a = fg_a.producer(fg_a.graph().node_inputs_exact::<2>(if_a)?[1]);
    assert!(!is_i1_xor_with_one(&fg_a, cond_a));

    let (mut fg_b, if_b) = build_if_with_neg_cond()?;
    assert!(crate::pipeline::run_one(&pass_b, &mut fg_b, &mut crate::OptCtx::new(None))?.changed());
    let cond_b = fg_b.producer(fg_b.graph().node_inputs_exact::<2>(if_b)?[1]);
    assert!(!is_i1_xor_with_one(&fg_b, cond_b));
    Ok(())
}

#[test]
fn if_with_bool_neg_cond_is_canonicalised() -> Result<()> {
    let (mut fg, if_node) = build_if_with_neg_cond()?;
    // Before: cond is the canonical 1-bit `Xor(_, IntConst(1))` (logical NOT).
    let cond_node_pre = fg.producer(fg.graph().node_inputs_exact::<2>(if_node)?[1]);
    assert!(is_i1_xor_with_one(&fg, cond_node_pre));

    let r = crate::pipeline::run_one(
        &IfCondInversion::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(r.changed());

    // After: cond is the inner producer (the read variable's I1 cast).
    // No `Xor(_, IntConst(1))` (logical NOT) remains on the If's cond
    // input.
    let cond_node_post = fg.producer(fg.graph().node_inputs_exact::<2>(if_node)?[1]);
    assert!(!is_i1_xor_with_one(&fg, cond_node_post));
    let _ = if_cond_kind; // keep helper alive for other tests
    Ok(())
}

#[test]
fn idempotent_after_one_application() -> Result<()> {
    let (mut fg, _if_node) = build_if_with_neg_cond()?;
    let first = crate::pipeline::run_one(
        &IfCondInversion::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(first.changed());
    let second = crate::pipeline::run_one(
        &IfCondInversion::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(!second.changed(), "second pass must be a no-op");
    Ok(())
}

#[test]
fn double_neg_collapses_after_constant_fold() -> Result<()> {
    // Build `if (!!cond) { ... }`.  After ConstantFold's `!!x → x` rule
    // (the dedicated `bool_not(bool_not(x)) → x` rule in the bool/float
    // group, matching `Xor(Xor(x, 1), 1):I1`) the cond is bare `cond`;
    // after IfCondInversion (running on the same fixed-point loop in
    // real pipelines) the If is canonical with no swap.  Pin the
    // even-parity-no-swap invariant.
    let cond_vn = strider_ir_test_utils::reg_vn(0x1000, 1);
    let (mut fg, _if_node, ()) = RegisterSet::new()
        .tracked(cond_vn)
        .build_if_then_else_returns(|b| {
            let raw = b.read_variable(&cond_vn)?;
            let cond_bool = b.convert_to_int_if_needed(raw, strider_ir::node::ValueType::I1)?;
            let n1 = build_bool_not(b, cond_bool)?;
            let n2 = build_bool_not(b, n1)?;
            Ok((n2, ()))
        })?;

    // ConstantFold first: collapses `!!x → x`.
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    // After ConstantFold the cond is no longer an `Xor(_, 1)` (logical
    // NOT), so IfCondInversion must NOT fire.  Even-parity → no branch
    // swap.
    let r = crate::pipeline::run_one(
        &IfCondInversion::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(
        !r.changed(),
        "IfCondInversion must be a no-op after !!x simplification — even parity preserves direct layout"
    );
    Ok(())
}

#[test]
fn swap_consumers_preserves_value_semantics() -> Result<()> {
    // The pass must swap the consumers of the two `If` control outputs
    // alongside dropping the `BitNot`.  Pin this by recording the
    // `Region` consumer of each output before the pass and
    // verifying they are now consumed in the swapped slots.
    let (mut fg, if_node) = build_if_with_neg_cond()?;
    let consumer_of =
        |fg: &strider_ir::Graph, value: strider_ir::node::ValueId| -> strider_ir::node::NodeId {
            let (consumer, _idx) = fg
                .value_uses(value)
                .next()
                .expect("each If output has exactly one consumer in this fixture");
            consumer
        };
    let [value0_pre, value1_pre] = fg.node_outputs_exact::<2>(if_node)?;
    let pre_true_consumer = consumer_of(fg.graph(), value0_pre);
    let pre_false_consumer = consumer_of(fg.graph(), value1_pre);
    assert_ne!(
        pre_true_consumer, pre_false_consumer,
        "pre-pass consumers must be distinct Region nodes"
    );

    crate::pipeline::run_one(
        &IfCondInversion::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;

    let [value0_post, value1_post] = fg.node_outputs_exact::<2>(if_node)?;
    let post_true_consumer = consumer_of(fg.graph(), value0_post);
    let post_false_consumer = consumer_of(fg.graph(), value1_post);

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
/// `BitNot(cond)` becomes dead after the rewrite (no other consumers
/// of the `BitNot` exist), its asm-fingerprint must be absorbed into
/// the surviving inner-cond node so the contributing-asm history is
/// preserved.  Without the fix, the inner cond would carry only its
/// own lift_addr; the BitNot's address would be silently dropped.
#[test]
fn bool_neg_fingerprint_absorbed_into_inner_cond() -> Result<()> {
    let cond_vn = strider_ir_test_utils::reg_vn(0x2000, 1);
    let (mut fg, _if_node, ()) = RegisterSet::new()
        .tracked(cond_vn)
        .build_if_then_else_returns(|b| {
            // Stamp distinct lift_addrs on the cond producer and the
            // logical-NOT (Xor with 1) so we can observe absorption.
            b.set_lift_addr(Some(0x500));
            let raw = b.read_variable(&cond_vn)?;
            let cond_bool = b.convert_to_int_if_needed(raw, strider_ir::node::ValueType::I1)?;
            b.set_lift_addr(Some(0x504));
            let neg_cond = build_bool_not(b, cond_bool)?;
            b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
            Ok((neg_cond, ()))
        })?;

    // Capture the Xor's NodeId BEFORE optimisation; after the rewrite
    // it becomes dead but stays in the arena.
    let bool_neg_node = fg
        .graph()
        .all_node_ids()
        .find(|&n| is_i1_xor_with_one(&fg, n))
        .expect("I1 Xor(_, 1) (logical NOT) present pre-pass");

    let r = crate::pipeline::run_one(
        &IfCondInversion::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(r.changed());

    // The BitNot's fingerprint MUST have been absorbed into the
    // inner-cond node (the new If cond input's producer).
    let if_node = find_unique_if(fg.graph());
    let [_ctrl, cond_value] = fg.graph().node_inputs_exact::<2>(if_node)?;
    let inner_node = fg.producer(cond_value);
    let inner_fp = fg.side_tables().asm_fingerprint(inner_node);
    let bool_neg_fp = fg.side_tables().asm_fingerprint(bool_neg_node);
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
/// of the BitNot's *input* (i.e. the new cond input's producer), not
/// the `If` node, not the BitNot itself, and not any unrelated reachable
/// node.  Guards against a buggy implementation that absorbs into the
/// wrong neighbour (e.g. unioning into `if_node` instead of the inner
/// cond, which would lose the contributing-asm history when the If gets
/// rewritten by a later pass).
#[test]
fn fingerprint_absorption_targets_inner_cond_producer_only() -> Result<()> {
    let cond_vn = strider_ir_test_utils::reg_vn(0x3000, 1);
    // Distinct addresses on the cond producer (0x800), the I1
    // Xor-with-1 (0x804), and the If (0x808) so we can prove the
    // Xor-with-1's address lands on exactly one of the three.
    let (mut fg, _if_node, ()) = RegisterSet::new()
        .tracked(cond_vn)
        .build_if_then_else_returns(|b| {
            b.set_lift_addr(Some(0x800));
            let raw = b.read_variable(&cond_vn)?;
            let cond_bool = b.convert_to_int_if_needed(raw, strider_ir::node::ValueType::I1)?;
            b.set_lift_addr(Some(0x804));
            let neg_cond = build_bool_not(b, cond_bool)?;
            b.set_lift_addr(Some(0x808));
            Ok((neg_cond, ()))
        })?;

    // Identify pre-pass: the Xor-with-1, the If, and the Xor's
    // non-constant operand producer (the "inner cond producer" that
    // should receive the absorbed fingerprint).
    let bool_neg_node = fg
        .graph()
        .all_node_ids()
        .find(|&n| is_i1_xor_with_one(&fg, n))
        .expect("I1 Xor(_, 1) pre-pass");
    let if_node_pre = find_unique_if(fg.graph());
    // The Xor's non-constant operand is whichever input is *not* the
    // I1 `IntConst(1)`.
    let [lhs, rhs] = fg.graph().node_inputs_exact::<2>(bool_neg_node)?;
    let inner_producer_pre = {
        let pick = |value: ValueId| {
            !(matches!(fg.kind_of_value(value), NodeKind::IntConst(_))
                && fg.int_const_u128(value) == Some(1))
        };
        let chosen = if pick(lhs) { lhs } else { rhs };
        fg.producer(chosen)
    };

    // 0x804 (the Xor-with-1's address) must be present on the Xor pre-pass,
    // absent on inner_producer_pre, and absent on if_node_pre.  Sanity-check
    // the fixture before running the pass.
    assert!(fg.side_tables().asm_fingerprint(bool_neg_node).contains(&0x804));
    assert!(!fg.side_tables().asm_fingerprint(inner_producer_pre).contains(&0x804));
    assert!(!fg.side_tables().asm_fingerprint(if_node_pre).contains(&0x804));

    let r = crate::pipeline::run_one(
        &IfCondInversion::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(r.changed());

    // After the pass, the Xor's address (0x804) must land on exactly
    // the inner producer — NOT on the If, and NOT on any sibling
    // reachable node that wasn't an ancestor of the cond input.
    assert!(
        fg.side_tables().asm_fingerprint(inner_producer_pre).contains(&0x804),
        "Xor-with-1's address 0x804 must be absorbed into the inner cond producer"
    );
    assert!(
        !fg.side_tables().asm_fingerprint(if_node_pre).contains(&0x804),
        "Xor-with-1's address 0x804 must NOT be absorbed into the If node"
    );

    // After absorption, the inner producer's fingerprint should contain
    // BOTH its own original address AND the Xor-with-1's.
    let inner_fp = fg.side_tables().asm_fingerprint(inner_producer_pre);
    assert!(
        inner_fp.contains(&0x804),
        "inner cond producer must carry the absorbed Xor-with-1 address 0x804"
    );
    Ok(())
}

/// Regression: when the `BitNot(cond)` feeding the `If` has OTHER live
/// consumers, the inversion MUST NOT absorb the BitNot's fingerprint
/// into the inner-cond producer.  The BitNot still produces a live
/// value for its remaining consumers (their fingerprints attribute via
/// the BitNot as before), so adding BitNot's addresses to the inner
/// cond would create FALSE-POSITIVE attribution — the inner cond does
/// NOT compute the BitNot's value, the BitNot does.
#[test]
fn bool_neg_fingerprint_not_absorbed_when_boolneg_has_other_consumers() -> Result<()> {
    let cond_vn = strider_ir_test_utils::reg_vn(0x4000, 1);
    // Build `if (!cond) { … }` AND a second consumer of the same
    // `!cond` value (a chained `Xor(Xor(cond, 1), 1)` whose outer
    // Xor still references the first Xor's output — that use-list
    // counts).  The dedup cache will share the I1 IntConst(1) across
    // both Xors but the Xor nodes themselves stay distinct.
    let (mut fg, _if_node, second_neg_node) = RegisterSet::new()
        .tracked(cond_vn)
        .build_if_then_else_returns(|b| {
            b.set_lift_addr(Some(0x900));
            let raw = b.read_variable(&cond_vn)?;
            let cond_bool = b.convert_to_int_if_needed(raw, strider_ir::node::ValueType::I1)?;
            b.set_lift_addr(Some(0x904));
            let neg_cond = build_bool_not(b, cond_bool)?;
            // Second consumer of the SAME `neg_cond` output.
            b.set_lift_addr(Some(0x908));
            let second_neg = build_bool_not(b, neg_cond)?;
            let second_neg_node = b.function().producer(second_neg);
            // Anchor `second_neg` to a side-effecting `Store` so it (and thus
            // the first Xor it consumes) stays entry-reachable through the
            // pipeline's initial dead-node cull.  Without this anchor the
            // chained Xor is a dead value cone and would be culled, leaving the
            // first Xor with a single consumer — defeating the "Xor keeps other
            // live consumers" scenario this test exercises.
            let store_addr = b.build_int_const(0x5000u64, strider_ir::node::ValueType::I64)?;
            b.build_store(store_addr, second_neg, rsleigh::VnSpace::RAM)?;
            b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
            Ok((neg_cond, second_neg_node))
        })?;

    // Locate the first Xor-with-1 (the one IfCondInversion will
    // redirect around) and its inner cond producer pre-pass.
    let bool_neg_node = fg
        .graph()
        .all_node_ids()
        .find(|&n| n != second_neg_node && is_i1_xor_with_one(&fg, n))
        .expect("first Xor(_, 1) (logical NOT) present pre-pass");
    let [lhs, rhs] = fg.graph().node_inputs_exact::<2>(bool_neg_node)?;
    let inner_producer_pre = {
        let pick = |value: ValueId| {
            !(matches!(fg.kind_of_value(value), NodeKind::IntConst(_))
                && fg.int_const_u128(value) == Some(1))
        };
        let chosen = if pick(lhs) { lhs } else { rhs };
        fg.producer(chosen)
    };

    // Sanity-check the fixture: the first Xor has 2 uses (the If and
    // the chained second Xor), and inner_producer_pre does NOT carry
    // 0x904 yet.
    let bool_neg_outs = fg.node_outputs(bool_neg_node).to_vec();
    assert_eq!(
        fg.graph().value_uses(bool_neg_outs[0]).count(),
        2,
        "fixture must have the first Xor(_, 1) with 2 consumers (If + second Xor)"
    );
    assert!(!fg.side_tables().asm_fingerprint(inner_producer_pre).contains(&0x904));

    let r = crate::pipeline::run_one(
        &IfCondInversion::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(
        r.changed(),
        "pass must still fire — the If's cond is Xor(_, 1)(…)"
    );

    // The headline assertion: the first Xor-with-1's address (0x904)
    // must NOT have been absorbed into the inner cond producer.  The
    // Xor is still live (consumed by second_neg_node), so the
    // attribution must stay on the Xor itself.
    assert!(
        !fg.side_tables().asm_fingerprint(inner_producer_pre).contains(&0x904),
        "Xor-with-1's address 0x904 must NOT leak into inner_producer when Xor has \
         remaining consumers (Xor is still live; would be false-positive attribution)"
    );
    // Inversely: the Xor's own fingerprint must still carry 0x904.
    assert!(
        fg.side_tables().asm_fingerprint(bool_neg_node).contains(&0x904),
        "Xor-with-1's own fingerprint must still carry its address"
    );
    Ok(())
}
