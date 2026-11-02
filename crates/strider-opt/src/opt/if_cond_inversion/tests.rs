use super::IfCondInversion;
use crate::ConstantFold;
use crate::Result;
use crate::test_support::{bool_not, find_unique_if, is_bool_not, run_pass, run_to_fixed_point};
use strider_ir::{IRBuilderExt, IRViewer};

use strider_ir::node::{NodeKind, ValueId};
use strider_ir_test_utils::RegisterSet;

/// `if (!cond) { return 1 } else { return 2 }`, `cond` read from a register.
fn build_if_with_neg_cond() -> Result<(strider_ir::Function, strider_ir::node::NodeId)> {
    let cond_vn = strider_ir_test_utils::reg_vn(0x1000, 1);
    let (fg, if_node, ()) = RegisterSet::new()
        .tracked(cond_vn)
        .build_if_then_else_returns(|b| {
            let raw = b.read_variable(&cond_vn)?;
            let cond_bool = b.convert_to_int_if_needed(raw, strider_ir::node::ValueType::I1)?;
            let neg_cond = bool_not(b, cond_bool)?;
            Ok((neg_cond, ()))
        })?;
    Ok((fg, if_node))
}

#[test]
fn inverts_a_negated_condition_once() -> Result<()> {
    let (mut fg, if_node) = build_if_with_neg_cond()?;
    let cond_pre = fg.producer(fg.if_cond(if_node));
    assert!(is_bool_not(&fg, cond_pre));

    let r = run_pass(&IfCondInversion, &mut fg)?;
    assert!(r, "the pass should invert the cond");

    let cond_post = fg.producer(fg.if_cond(if_node));
    assert!(!is_bool_not(&fg, cond_post));
    assert!(
        !run_pass(&IfCondInversion, &mut fg)?,
        "a second run is a no-op"
    );
    Ok(())
}

#[test]
fn double_neg_collapses_after_constant_fold() -> Result<()> {
    // `if (!!cond)`: ConstantFold collapses `Xor(Xor(x, 1), 1):I1` to `x`, so
    // even parity must leave the branch layout alone.
    let cond_vn = strider_ir_test_utils::reg_vn(0x1000, 1);
    let (mut fg, _if_node, ()) = RegisterSet::new()
        .tracked(cond_vn)
        .build_if_then_else_returns(|b| {
            let raw = b.read_variable(&cond_vn)?;
            let cond_bool = b.convert_to_int_if_needed(raw, strider_ir::node::ValueType::I1)?;
            let n1 = bool_not(b, cond_bool)?;
            let n2 = bool_not(b, n1)?;
            Ok((n2, ()))
        })?;

    run_to_fixed_point(&ConstantFold, &mut fg)?;
    let r = run_pass(&IfCondInversion, &mut fg)?;
    assert!(
        !r,
        "IfCondInversion must be a no-op after !!x simplification: even parity preserves direct layout"
    );
    Ok(())
}

#[test]
fn swap_consumers_preserves_value_semantics() -> Result<()> {
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

    run_pass(&IfCondInversion, &mut fg)?;

    let [value0_post, value1_post] = fg.node_outputs_exact::<2>(if_node)?;
    let post_true_consumer = consumer_of(fg.graph(), value0_post);
    let post_false_consumer = consumer_of(fg.graph(), value1_post);

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

/// When the rewrite kills the `Xor` (it had no other consumers), its
/// asm-fingerprint must land on the surviving inner cond rather than vanish.
#[test]
fn bool_neg_fingerprint_absorbed_into_inner_cond() -> Result<()> {
    let cond_vn = strider_ir_test_utils::reg_vn(0x2000, 1);
    let (mut fg, _if_node, ()) = RegisterSet::new()
        .tracked(cond_vn)
        .build_if_then_else_returns(|b| {
            // Distinct lift_addrs on cond producer vs Xor, to observe absorption.
            b.set_lift_addr(Some(0x500));
            let raw = b.read_variable(&cond_vn)?;
            let cond_bool = b.convert_to_int_if_needed(raw, strider_ir::node::ValueType::I1)?;
            b.set_lift_addr(Some(0x504));
            let neg_cond = bool_not(b, cond_bool)?;
            b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
            Ok((neg_cond, ()))
        })?;

    // Grab the Xor before the pass: it goes dead but stays in the arena.
    let bool_neg_node = fg
        .graph()
        .all_node_ids()
        .find(|&n| is_bool_not(&fg, n))
        .expect("I1 Xor(_, 1) (logical NOT) present pre-pass");

    let r = run_pass(&IfCondInversion, &mut fg)?;
    assert!(r);

    let if_node = find_unique_if(fg.graph());
    let [_ctrl, cond_value] = fg.graph().node_inputs_exact::<2>(if_node)?;
    let inner_node = fg.producer(cond_value);
    let inner_fp = fg.side_tables().asm_fingerprint(inner_node);
    let bool_neg_fp = fg.side_tables().asm_fingerprint(bool_neg_node);
    for addr in &bool_neg_fp {
        assert!(
            inner_fp.contains(addr),
            "BoolNeg's address {addr:#x} must survive into inner-cond fingerprint after \
             IfCondInversion: inner_fp={inner_fp:?}, bool_neg_fp={bool_neg_fp:?}"
        );
    }
    Ok(())
}

/// Pins WHICH node receives the absorbed fingerprint: the producer of the
/// Xor's input, not the `If`.  Absorbing into the `If` would lose the history
/// as soon as a later pass rewrites that `If`.
#[test]
fn fingerprint_absorption_targets_inner_cond_producer_only() -> Result<()> {
    let cond_vn = strider_ir_test_utils::reg_vn(0x3000, 1);
    // Cond producer 0x800, Xor 0x804, If 0x808: distinct, pinning where 0x804
    // lands.
    let (mut fg, _if_node, ()) = RegisterSet::new()
        .tracked(cond_vn)
        .build_if_then_else_returns(|b| {
            b.set_lift_addr(Some(0x800));
            let raw = b.read_variable(&cond_vn)?;
            let cond_bool = b.convert_to_int_if_needed(raw, strider_ir::node::ValueType::I1)?;
            b.set_lift_addr(Some(0x804));
            let neg_cond = bool_not(b, cond_bool)?;
            b.set_lift_addr(Some(0x808));
            Ok((neg_cond, ()))
        })?;

    let bool_neg_node = fg
        .graph()
        .all_node_ids()
        .find(|&n| is_bool_not(&fg, n))
        .expect("I1 Xor(_, 1) pre-pass");
    let if_node_pre = find_unique_if(fg.graph());
    // The inner cond is whichever Xor operand is not the I1 `IntConst(1)`.
    let [lhs, rhs] = fg.graph().node_inputs_exact::<2>(bool_neg_node)?;
    let inner_producer_pre = {
        let pick = |value: ValueId| {
            !(matches!(fg.kind_of_value(value), NodeKind::IntConst(_))
                && fg.int_const_u128(value) == Some(1))
        };
        let chosen = if pick(lhs) { lhs } else { rhs };
        fg.producer(chosen)
    };

    // Fixture sanity: 0x804 starts on the Xor alone.
    assert!(
        fg.side_tables()
            .asm_fingerprint(bool_neg_node)
            .contains(&0x804)
    );
    assert!(
        !fg.side_tables()
            .asm_fingerprint(inner_producer_pre)
            .contains(&0x804)
    );
    assert!(
        !fg.side_tables()
            .asm_fingerprint(if_node_pre)
            .contains(&0x804)
    );

    let r = run_pass(&IfCondInversion, &mut fg)?;
    assert!(r);

    assert!(
        fg.side_tables()
            .asm_fingerprint(inner_producer_pre)
            .contains(&0x804),
        "Xor-with-1's address 0x804 must be absorbed into the inner cond producer"
    );
    assert!(
        !fg.side_tables()
            .asm_fingerprint(if_node_pre)
            .contains(&0x804),
        "Xor-with-1's address 0x804 must NOT be absorbed into the If node"
    );

    let inner_fp = fg.side_tables().asm_fingerprint(inner_producer_pre);
    assert!(
        inner_fp.contains(&0x804),
        "inner cond producer must carry the absorbed Xor-with-1 address 0x804"
    );
    Ok(())
}

/// The mirror case: a `Xor` that keeps other live consumers still computes its
/// own value, so absorbing its addresses into the inner cond would be
/// false-positive attribution.
#[test]
fn bool_neg_fingerprint_not_absorbed_when_boolneg_has_other_consumers() -> Result<()> {
    let cond_vn = strider_ir_test_utils::reg_vn(0x4000, 1);
    // `if (!cond)` plus a chained `Xor(Xor(cond, 1), 1)` as a second consumer
    // of the same `!cond` value.
    let (mut fg, _if_node, second_neg_node) = RegisterSet::new()
        .tracked(cond_vn)
        .build_if_then_else_returns(|b| {
            b.set_lift_addr(Some(0x900));
            let raw = b.read_variable(&cond_vn)?;
            let cond_bool = b.convert_to_int_if_needed(raw, strider_ir::node::ValueType::I1)?;
            b.set_lift_addr(Some(0x904));
            let neg_cond = bool_not(b, cond_bool)?;
            b.set_lift_addr(Some(0x908));
            let second_neg = bool_not(b, neg_cond)?;
            let second_neg_node = b.function().producer(second_neg);
            // The Store anchors `second_neg` against the initial dead-node cull.
            // Unanchored it is a dead cone, and culling it would leave the first
            // Xor single-use, which is the opposite of what this test needs.
            let store_addr = b.build_int_const(0x5000u64, strider_ir::node::ValueType::int(64))?;
            b.build_store(store_addr, second_neg, rsleigh::VnSpace::RAM)?;
            b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
            Ok((neg_cond, second_neg_node))
        })?;

    // The first Xor is the one the pass redirects around.
    let bool_neg_node = fg
        .graph()
        .all_node_ids()
        .find(|&n| n != second_neg_node && is_bool_not(&fg, n))
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

    // Fixture sanity: two uses on the first Xor, 0x904 not yet on the inner cond.
    let bool_neg_outs = fg.node_outputs(bool_neg_node).to_vec();
    assert_eq!(
        fg.graph().value_uses(bool_neg_outs[0]).count(),
        2,
        "fixture must have the first Xor(_, 1) with 2 consumers (If + second Xor)"
    );
    assert!(
        !fg.side_tables()
            .asm_fingerprint(inner_producer_pre)
            .contains(&0x904)
    );

    let r = run_pass(&IfCondInversion, &mut fg)?;
    assert!(r, "pass must still fire: the If's cond is Xor(_, 1)(...)");

    assert!(
        !fg.side_tables()
            .asm_fingerprint(inner_producer_pre)
            .contains(&0x904),
        "Xor-with-1's address 0x904 must NOT leak into inner_producer when Xor has \
         remaining consumers (Xor is still live; would be false-positive attribution)"
    );
    assert!(
        fg.side_tables()
            .asm_fingerprint(bool_neg_node)
            .contains(&0x904),
        "Xor-with-1's own fingerprint must still carry its address"
    );
    Ok(())
}
