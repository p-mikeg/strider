//! Phase 3 Task 3.3b parity test — `IfCondInversion` v1 vs
//! `IfCondInversionEgg` v2.
//!
//! `If` is a CONTROL node, so it is not in the egraph's value slice.
//! The v2 pass operates on the strider `Graph` directly (with the
//! egraph only used as an analysis aid if needed); v1 also operates on
//! the strider `Graph` directly.  Both MUST produce structurally
//! identical IR.
//!
//! Per the plan: "If after reading v1's IfCondInversion you find it
//! doesn't need the egraph at all (it's a simple structural match),
//! you can skip the egg integration for this pass and just port the
//! structural rewrite."  v2's pass is a straight port of v1's
//! structural match (see commit msg for rationale).
//!
//! Parity surface for IfCondInversion (single rule):
//!   - rule 1: `If(BoolNeg(C)) {A}{B}` → `If(C) {B}{A}`
//!
//! Each test builds a fixture, runs v1 on one copy and v2 on a fresh
//! copy, and asserts both produce the same NodeKind on the If's cond
//! AND the same control-output→consumer linkage (i.e. branches were
//! swapped identically).

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use strider_analyze::opt::{
    IfCondInversion, OptimizerRaw, if_cond_inversion_egg::IfCondInversionEgg,
};
use strider_ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputType};
use strider_ir::{BoolUnaryOp, BuiltFunctionGraph, FunctionBuilder};

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Builds `if (!cond) { return 1 } else { return 2 }`.  Returns the
/// graph and the unique `If` node id.
fn build_if_with_neg_cond() -> (BuiltFunctionGraph, NodeId) {
    let cond_vn = strider_ir::test_utils::reg_vn(0x1000, 1);
    let mut b = FunctionBuilder::new_raw(vec![cond_vn], &[], &[], &[], None, 0).unwrap();
    let entry = b.create_region().unwrap();
    let t = b.create_region().unwrap();
    let f = b.create_region().unwrap();

    b.set_entry_region(entry).unwrap();
    b.set_region(entry);
    b.set_lift_addr(Some(strider_ir::test_utils::SENTINEL_LIFT_ADDR));
    let raw = b.read_variable(&cond_vn).unwrap();
    let cond_bool = b.convert_to_bool_if_needed(raw).unwrap();
    let neg_cond = b
        .build_boolean_unary_operation(cond_bool, BoolUnaryOp::Neg)
        .unwrap();
    b.build_if(neg_cond, t, f).unwrap();

    b.set_region(t);
    let one = b.build_int_const(1u64, NodeOutputType::U64).unwrap();
    b.build_return(Some(one), &[]).unwrap();

    b.set_region(f);
    let two = b.build_int_const(2u64, NodeOutputType::U64).unwrap();
    b.build_return(Some(two), &[]).unwrap();
    b.set_lift_addr(None);

    let fg = b.build().unwrap();
    let if_node = find_unique_if(&fg);
    (fg, if_node)
}

/// Builds `if (!!cond) {...}` for testing double-negation behavior.
fn build_if_with_double_neg_cond() -> (BuiltFunctionGraph, NodeId) {
    let cond_vn = strider_ir::test_utils::reg_vn(0x1000, 1);
    let mut b = FunctionBuilder::new_raw(vec![cond_vn], &[], &[], &[], None, 0).unwrap();
    let entry = b.create_region().unwrap();
    let t = b.create_region().unwrap();
    let f = b.create_region().unwrap();
    b.set_entry_region(entry).unwrap();
    b.set_region(entry);
    b.set_lift_addr(Some(strider_ir::test_utils::SENTINEL_LIFT_ADDR));
    let raw = b.read_variable(&cond_vn).unwrap();
    let cond_bool = b.convert_to_bool_if_needed(raw).unwrap();
    let n1 = b
        .build_boolean_unary_operation(cond_bool, BoolUnaryOp::Neg)
        .unwrap();
    let n2 = b
        .build_boolean_unary_operation(n1, BoolUnaryOp::Neg)
        .unwrap();
    b.build_if(n2, t, f).unwrap();
    b.set_region(t);
    let one = b.build_int_const(1u64, NodeOutputType::U64).unwrap();
    b.build_return(Some(one), &[]).unwrap();
    b.set_region(f);
    let two = b.build_int_const(2u64, NodeOutputType::U64).unwrap();
    b.build_return(Some(two), &[]).unwrap();
    b.set_lift_addr(None);
    let fg = b.build().unwrap();
    let if_node = find_unique_if(&fg);
    (fg, if_node)
}

/// Builds `if (cond) {...}` (no BoolNeg) — should not be rewritten.
fn build_if_with_plain_cond() -> (BuiltFunctionGraph, NodeId) {
    let cond_vn = strider_ir::test_utils::reg_vn(0x1000, 1);
    let mut b = FunctionBuilder::new_raw(vec![cond_vn], &[], &[], &[], None, 0).unwrap();
    let entry = b.create_region().unwrap();
    let t = b.create_region().unwrap();
    let f = b.create_region().unwrap();
    b.set_entry_region(entry).unwrap();
    b.set_region(entry);
    b.set_lift_addr(Some(strider_ir::test_utils::SENTINEL_LIFT_ADDR));
    let raw = b.read_variable(&cond_vn).unwrap();
    let cond_bool = b.convert_to_bool_if_needed(raw).unwrap();
    b.build_if(cond_bool, t, f).unwrap();
    b.set_region(t);
    let one = b.build_int_const(1u64, NodeOutputType::U64).unwrap();
    b.build_return(Some(one), &[]).unwrap();
    b.set_region(f);
    let two = b.build_int_const(2u64, NodeOutputType::U64).unwrap();
    b.build_return(Some(two), &[]).unwrap();
    b.set_lift_addr(None);
    let fg = b.build().unwrap();
    let if_node = find_unique_if(&fg);
    (fg, if_node)
}

fn find_unique_if(fg: &BuiltFunctionGraph) -> NodeId {
    let ifs: Vec<NodeId> = fg
        .all_node_ids()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::If))
        .collect();
    assert_eq!(ifs.len(), 1, "fixture must have exactly one If node");
    ifs[0]
}

/// Returns the `NodeKind` of the producer of the If's cond input.
fn if_cond_producer_kind(fg: &BuiltFunctionGraph, if_node: NodeId) -> NodeKind {
    let [_ctrl, cond_out] = fg
        .graph
        .node_inputs_exact::<2>(if_node)
        .expect("If has 2 inputs");
    *fg.kind_of_output(cond_out)
}

/// Returns the single consumer of the given output.  Panics if not exactly one.
fn unique_consumer(fg: &BuiltFunctionGraph, out: NodeOutputId) -> NodeId {
    let consumers: Vec<NodeId> = fg.graph.output_uses(out).map(|(c, _)| c).collect();
    assert_eq!(
        consumers.len(),
        1,
        "fixture invariant: each If output has one ControlState consumer"
    );
    consumers[0]
}

// ── Parity tests ─────────────────────────────────────────────────────────────

/// Rule 1: `If(BoolNeg(C))` is rewritten — both v1 and v2 produce a
/// non-`BoolNeg` cond and swap the control-output consumers identically.
#[test]
fn parity_basic_bool_neg_cond() {
    // v1
    let (mut fg_v1, if_node_v1) = build_if_with_neg_cond();
    let [t_out_pre, f_out_pre] = fg_v1.node_outputs_exact::<2>(if_node_v1).unwrap();
    let v1_pre_true_consumer = unique_consumer(&fg_v1, t_out_pre);
    let v1_pre_false_consumer = unique_consumer(&fg_v1, f_out_pre);
    let r1 = IfCondInversion
        .optimize_raw(&mut fg_v1.graph, fg_v1.entry)
        .unwrap();
    assert!(r1.changed(), "v1: BoolNeg cond should be rewritten");
    let v1_post_cond_kind = if_cond_producer_kind(&fg_v1, if_node_v1);
    let [t_out_post_v1, f_out_post_v1] = fg_v1.node_outputs_exact::<2>(if_node_v1).unwrap();
    let v1_post_true_consumer = unique_consumer(&fg_v1, t_out_post_v1);
    let v1_post_false_consumer = unique_consumer(&fg_v1, f_out_post_v1);

    // v2
    let (mut fg_v2, if_node_v2) = build_if_with_neg_cond();
    let [t_out_pre2, f_out_pre2] = fg_v2.node_outputs_exact::<2>(if_node_v2).unwrap();
    let v2_pre_true_consumer = unique_consumer(&fg_v2, t_out_pre2);
    let v2_pre_false_consumer = unique_consumer(&fg_v2, f_out_pre2);
    let r2 = IfCondInversionEgg::new()
        .optimize_raw(&mut fg_v2.graph, fg_v2.entry)
        .unwrap();
    assert!(r2.changed(), "v2: BoolNeg cond should be rewritten");
    let v2_post_cond_kind = if_cond_producer_kind(&fg_v2, if_node_v2);
    let [t_out_post_v2, f_out_post_v2] = fg_v2.node_outputs_exact::<2>(if_node_v2).unwrap();
    let v2_post_true_consumer = unique_consumer(&fg_v2, t_out_post_v2);
    let v2_post_false_consumer = unique_consumer(&fg_v2, f_out_post_v2);

    // Parity: both should produce the same cond kind (CastToBool, not BoolNeg).
    assert!(
        !matches!(v1_post_cond_kind, NodeKind::BoolUnaryOp(BoolUnaryOp::Neg)),
        "v1 should have removed the BoolNeg cond"
    );
    assert!(
        !matches!(v2_post_cond_kind, NodeKind::BoolUnaryOp(BoolUnaryOp::Neg)),
        "v2 should have removed the BoolNeg cond"
    );
    // Pre-conditions on both fixtures are equal (they were built identically).
    assert_eq!(v1_pre_true_consumer, v2_pre_true_consumer);
    assert_eq!(v1_pre_false_consumer, v2_pre_false_consumer);
    // Post-conditions on both fixtures: the true/false consumers were
    // swapped identically.  We use the v1's pre→post swap as the oracle.
    assert_eq!(
        v1_post_true_consumer, v1_pre_false_consumer,
        "v1: output[0]'s consumer became what was output[1]'s consumer"
    );
    assert_eq!(
        v1_post_false_consumer, v1_pre_true_consumer,
        "v1: output[1]'s consumer became what was output[0]'s consumer"
    );
    assert_eq!(
        v2_post_true_consumer, v2_pre_false_consumer,
        "v2: matches v1 — output[0]'s consumer swapped"
    );
    assert_eq!(
        v2_post_false_consumer, v2_pre_true_consumer,
        "v2: matches v1 — output[1]'s consumer swapped"
    );
}

/// Double-neg without ConstantFold first: each pass fires once and
/// strips one of the BoolNegs.  Both must behave identically.
#[test]
fn parity_double_neg_cond_one_pass() {
    let (mut fg_v1, if_node_v1) = build_if_with_double_neg_cond();
    let r1 = IfCondInversion
        .optimize_raw(&mut fg_v1.graph, fg_v1.entry)
        .unwrap();
    let v1_changed = r1.changed();
    let v1_kind = if_cond_producer_kind(&fg_v1, if_node_v1);

    let (mut fg_v2, if_node_v2) = build_if_with_double_neg_cond();
    let r2 = IfCondInversionEgg::new()
        .optimize_raw(&mut fg_v2.graph, fg_v2.entry)
        .unwrap();
    let v2_changed = r2.changed();
    let v2_kind = if_cond_producer_kind(&fg_v2, if_node_v2);

    assert_eq!(v1_changed, v2_changed, "parity: changed flag matches");
    assert_eq!(v1_kind, v2_kind, "parity: post-pass cond kind matches");
}

/// Negative test: an If with a non-BoolNeg cond is left alone by BOTH passes.
#[test]
fn parity_no_op_on_plain_cond() {
    let (mut fg_v1, if_node_v1) = build_if_with_plain_cond();
    let v1_pre_kind = if_cond_producer_kind(&fg_v1, if_node_v1);
    let r1 = IfCondInversion
        .optimize_raw(&mut fg_v1.graph, fg_v1.entry)
        .unwrap();
    let v1_post_kind = if_cond_producer_kind(&fg_v1, if_node_v1);

    let (mut fg_v2, if_node_v2) = build_if_with_plain_cond();
    let r2 = IfCondInversionEgg::new()
        .optimize_raw(&mut fg_v2.graph, fg_v2.entry)
        .unwrap();
    let v2_post_kind = if_cond_producer_kind(&fg_v2, if_node_v2);

    assert!(!r1.changed(), "v1: plain cond should not fire");
    assert!(!r2.changed(), "v2: plain cond should not fire");
    assert_eq!(v1_pre_kind, v1_post_kind, "v1: kind unchanged");
    assert_eq!(v1_post_kind, v2_post_kind, "parity: both kinds equal");
}

/// Asm-fingerprint absorption: when the BoolNeg becomes dead after the
/// rewrite, its fingerprint must be absorbed into the inner-cond node.
/// v1 already enforces this; v2 must match.
#[test]
fn parity_bool_neg_fingerprint_absorbed() {
    // Build with distinct lift_addrs on the cond producer and BoolNeg.
    fn build() -> (BuiltFunctionGraph, NodeId, NodeId) {
        let cond_vn = strider_ir::test_utils::reg_vn(0x2000, 1);
        let mut b = FunctionBuilder::new_raw(vec![cond_vn], &[], &[], &[], None, 0).unwrap();
        let entry = b.create_region().unwrap();
        let t = b.create_region().unwrap();
        let f = b.create_region().unwrap();
        b.set_entry_region(entry).unwrap();
        b.set_region(entry);
        b.set_lift_addr(Some(0x500));
        let raw = b.read_variable(&cond_vn).unwrap();
        let cond_bool = b.convert_to_bool_if_needed(raw).unwrap();
        b.set_lift_addr(Some(0x504));
        let neg_cond = b
            .build_boolean_unary_operation(cond_bool, BoolUnaryOp::Neg)
            .unwrap();
        b.set_lift_addr(Some(strider_ir::test_utils::SENTINEL_LIFT_ADDR));
        b.build_if(neg_cond, t, f).unwrap();
        b.set_region(t);
        let one = b.build_int_const(1u64, NodeOutputType::U64).unwrap();
        b.build_return(Some(one), &[]).unwrap();
        b.set_region(f);
        let two = b.build_int_const(2u64, NodeOutputType::U64).unwrap();
        b.build_return(Some(two), &[]).unwrap();
        b.set_lift_addr(None);
        let fg = b.build().unwrap();
        let if_node = find_unique_if(&fg);
        let bool_neg_node = fg
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::BoolUnaryOp(BoolUnaryOp::Neg)))
            .unwrap();
        (fg, if_node, bool_neg_node)
    }

    fn run(opt: &dyn OptimizerRaw, fg: &mut BuiltFunctionGraph) {
        opt.optimize_raw(&mut fg.graph, fg.entry).unwrap();
    }

    fn check(fg: &BuiltFunctionGraph, if_node: NodeId, bool_neg_node: NodeId) {
        let [_ctrl, cond_out] = fg.graph.node_inputs_exact::<2>(if_node).unwrap();
        let inner_node = fg.graph.get_node_from_output(cond_out);
        let inner_fp = fg.graph.asm_fingerprint(inner_node);
        let bool_neg_fp = fg.graph.asm_fingerprint(bool_neg_node);
        for addr in bool_neg_fp {
            assert!(
                inner_fp.contains(addr),
                "fingerprint {addr:#x} from BoolNeg must be absorbed into inner cond"
            );
        }
    }

    let (mut fg_v1, if_v1, bn_v1) = build();
    run(&IfCondInversion, &mut fg_v1);
    check(&fg_v1, if_v1, bn_v1);

    let (mut fg_v2, if_v2, bn_v2) = build();
    run(&IfCondInversionEgg::new(), &mut fg_v2);
    check(&fg_v2, if_v2, bn_v2);
}
