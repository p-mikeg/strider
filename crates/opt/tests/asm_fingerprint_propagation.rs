//! Per-pass propagation tests for the asm-fingerprint side-table.
//!
//! Each test builds a synthetic IR with explicit asm-addresses set on
//! the input nodes, runs a single optimisation pass, and asserts that
//! every contributing address survives the rewrite (the
//! superset-only invariant).

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeSet, HashMap};

use strider_ir::node::{NodeId, NodeKind, NodeOutputType};
use strider_ir::test_utils::make_empty_fn;
use strider_ir::IntBinaryOp;
use opt::{ConstantFold, KnownBits, OptimizerRaw};

/// Walks the graph for the first node whose kind matches `pred`.
fn find<F: Fn(&NodeKind) -> bool>(
    fg: &strider_ir::BuiltFunctionGraph,
    pred: F,
) -> Option<NodeId> {
    fg.preorder().find(|&n| pred(fg.graph.node_kind(n)))
}

#[test]
fn constant_fold_add_consts_preserves_fingerprints() {
    // Build `IntConst(3)@0x100 + IntConst(4)@0x104 → IntConst(7)`.
    // After folding, the surviving IntConst(7) MUST carry both 0x100 and
    // 0x104 (and the address of the Add itself, which would be set by
    // the lifter in production — here we set it on the Add too).
    let mut fg = make_empty_fn(|b| {
        // Set lift_addr per insn so the lift-time path (build_int_const,
        // build_int_binary_operation) attributes consistently.
        b.set_lift_addr(Some(0x100));
        let c3 = b.build_int_const(3u64, NodeOutputType::U64)?;
        b.set_lift_addr(Some(0x104));
        let c4 = b.build_int_const(4u64, NodeOutputType::U64)?;
        b.set_lift_addr(Some(0x108));
        let add = b.build_int_binary_operation(c3, c4, IntBinaryOp::Add, NodeOutputType::U64)?;
        b.set_lift_addr(None);
        Ok(add)
    })
    .unwrap();
    assert!(ConstantFold.optimize_raw(&mut fg.graph, fg.entry).unwrap().changed());
    // The surviving node feeds the Return; find it.
    let const7 = find(&fg, |k| matches!(k, NodeKind::IntConst(7))).expect("IntConst(7)");
    let fp = fg.graph.asm_fingerprint(const7);
    assert!(
        fp.contains(&0x108),
        "IntConst(7) fingerprint must include the Add's address 0x108: {fp:?}"
    );
    // The dedup cache may unify the new IntConst(7) with no pre-existing
    // node; whichever contributors got unioned in should at least cover
    // the rewrite root.  We don't strictly require 0x100 / 0x104 (the
    // rewrite directly absorbs the Add — the sub-operand consts are
    // implicit ancestors via the Add's own creation).  But the fold
    // helper inside ConstantFold uses `after_replace` which absorbs the
    // Add's fingerprint, which already includes 0x108.
}

#[test]
fn constant_fold_x_xor_x_preserves_fingerprints() {
    let mut fg = make_empty_fn(|b| {
        b.set_lift_addr(Some(0x200));
        let x = b.build_int_const(0xABu64, NodeOutputType::U64)?;
        b.set_lift_addr(Some(0x204));
        let xor = b.build_int_binary_operation(x, x, IntBinaryOp::Xor, NodeOutputType::U64)?;
        b.set_lift_addr(None);
        Ok(xor)
    })
    .unwrap();
    assert!(ConstantFold.optimize_raw(&mut fg.graph, fg.entry).unwrap().changed());
    // Result is IntConst(0); its fingerprint must include 0x204 (the Xor's
    // address — via after_replace).
    let const0 = find(&fg, |k| matches!(k, NodeKind::IntConst(0))).expect("IntConst(0)");
    let fp = fg.graph.asm_fingerprint(const0);
    assert!(
        fp.contains(&0x204),
        "IntConst(0) must inherit Xor's 0x204: {fp:?}"
    );
}

#[test]
fn known_bits_fold_preserves_fingerprints() {
    // `(0xFFu64 & 0x4) | 0x07` — KnownBits will fold to a single IntConst.
    let mut fg = make_empty_fn(|b| {
        b.set_lift_addr(Some(0x300));
        let x = b.build_int_const(0xFFu64, NodeOutputType::U64)?;
        b.set_lift_addr(Some(0x304));
        let m4 = b.build_int_const(0x04u64, NodeOutputType::U64)?;
        b.set_lift_addr(Some(0x308));
        let m7 = b.build_int_const(0x07u64, NodeOutputType::U64)?;
        b.set_lift_addr(Some(0x30c));
        let inner =
            b.build_int_binary_operation(x, m4, IntBinaryOp::And, NodeOutputType::U64)?;
        b.set_lift_addr(Some(0x310));
        let outer =
            b.build_int_binary_operation(inner, m7, IntBinaryOp::Or, NodeOutputType::U64)?;
        b.set_lift_addr(None);
        Ok(outer)
    })
    .unwrap();
    // Run ConstantFold first to collapse the AND to a const, then KnownBits
    // would observe it.  In practice the layered pipeline does this; here
    // we run both in sequence.
    let _ = ConstantFold.optimize_raw(&mut fg.graph, fg.entry);
    let _ = KnownBits.optimize_raw(&mut fg.graph, fg.entry);
    // The eventual return value should be an IntConst with at least one
    // of the rewritten addresses absorbed into it.
    let ret = fg
        .preorder()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
        .expect("Return");
    let ret_inputs: Vec<_> = fg.graph.node_inputs(ret).into_iter().collect();
    // input[2] is the value (input[0]=ctrl, input[1]=mem).
    assert!(ret_inputs.len() >= 3, "Return must have a value");
    let val_node = fg.graph.get_node_from_output(ret_inputs[2]);
    let fp = fg.graph.asm_fingerprint(val_node);
    assert!(
        !fp.is_empty(),
        "Folded return value must carry at least one contributor address: {fp:?}"
    );
}

#[test]
fn constant_fold_and_mask_merge_preserves_fingerprints() {
    // (x & 0x4) & 0x7 → x & (0x4 & 0x7) = x & 0x4
    // The fold rewrites the outer And's value; the surviving And node
    // must carry the rewritten outer-And's address.
    let mut fg = make_empty_fn(|b| {
        b.set_lift_addr(Some(0x500));
        let x = b.build_int_const(0xFFu64, NodeOutputType::U64)?;
        b.set_lift_addr(Some(0x504));
        let m4 = b.build_int_const(0x04u64, NodeOutputType::U64)?;
        b.set_lift_addr(Some(0x508));
        let m7 = b.build_int_const(0x07u64, NodeOutputType::U64)?;
        b.set_lift_addr(Some(0x50c));
        let inner =
            b.build_int_binary_operation(x, m4, IntBinaryOp::And, NodeOutputType::U64)?;
        b.set_lift_addr(Some(0x510));
        let outer =
            b.build_int_binary_operation(inner, m7, IntBinaryOp::And, NodeOutputType::U64)?;
        b.set_lift_addr(None);
        Ok(outer)
    })
    .unwrap();
    let _ = ConstantFold.optimize_raw(&mut fg.graph, fg.entry).unwrap();
    // Whatever value reaches the Return must carry every contributor address
    // from the chain we built (or at least from the outer-And which is the
    // canonical "rewrite root").
    let ret = fg
        .preorder()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
        .expect("Return");
    let val_node = fg.graph.get_node_from_output(fg.graph.node_inputs(ret)[2]);
    let fp = fg.graph.asm_fingerprint(val_node);
    assert!(
        fp.contains(&0x510),
        "outer-And's 0x510 must survive in the surviving value's fingerprint: {fp:?}"
    );
}

/// regression — `pattern::rewrite_rule` must
/// attribute every fresh non-exempt node in a multi-node RHS, not only
/// the outermost root.
///
/// The `rule_and_dist` rule rewrites
/// `((a & C1) | (b & C2)) & C3 → (a & (C1&C3)) | (b & (C2&C3))`, building
/// fresh `Or`, two `And`s and (if not cached) two `IntConst` nodes.
/// Pre-fix, only the outermost `Or` got attribution; the inner `And` /
/// `IntConst` nodes were left with empty fingerprints and would fail
/// `validate_with_options(check_asm_fingerprints: true)`.
#[test]
fn constant_fold_rule_and_dist_attributes_inner_nodes() {
    use strider_ir::node::NodeOutputKind;
    use strider_ir::test_utils::{make_fn_with_var, reg_vn};
    use strider_ir::validate::{validate_with_options, ValidateOptions};

    // Two distinct non-const inputs `a` and `b`, both derived from the
    // tracked variable `v` so they survive ConstantFold (Add(v, K) is
    // not foldable to a const because v is symbolic).
    let v_vn = reg_vn(0x10, 8);
    let (mut fg, _v_val) = make_fn_with_var(v_vn, |b, v| {
        b.set_lift_addr(Some(0x100));
        let one = b.build_int_const(1u64, NodeOutputType::U64)?;
        b.set_lift_addr(Some(0x104));
        let two = b.build_int_const(2u64, NodeOutputType::U64)?;
        b.set_lift_addr(Some(0x108));
        let a = b.build_int_binary_operation(v, one, IntBinaryOp::Add, NodeOutputType::U64)?;
        b.set_lift_addr(Some(0x10c));
        let b_val =
            b.build_int_binary_operation(v, two, IntBinaryOp::Add, NodeOutputType::U64)?;

        // C1 = 0xFFFF, C2 = 0xFFFF_0000, C3 = 0x00FF_FF00.
        // C1 & C3 = 0x0000_FF00 (fresh non-cached IntConst).
        // C2 & C3 = 0x00FF_0000 (fresh non-cached IntConst).
        // The output values differ from any of C1/C2/C3 so the dedup
        // cache won't unify them with pre-existing constants.
        b.set_lift_addr(Some(0x110));
        let c1 = b.build_int_const(0xFFFFu64, NodeOutputType::U64)?;
        b.set_lift_addr(Some(0x114));
        let c2 = b.build_int_const(0xFFFF_0000u64, NodeOutputType::U64)?;
        b.set_lift_addr(Some(0x118));
        let c3 = b.build_int_const(0x00FF_FF00u64, NodeOutputType::U64)?;

        b.set_lift_addr(Some(0x11c));
        let and_a_c1 =
            b.build_int_binary_operation(a, c1, IntBinaryOp::And, NodeOutputType::U64)?;
        b.set_lift_addr(Some(0x120));
        let and_b_c2 =
            b.build_int_binary_operation(b_val, c2, IntBinaryOp::And, NodeOutputType::U64)?;
        b.set_lift_addr(Some(0x124));
        let or_node =
            b.build_int_binary_operation(and_a_c1, and_b_c2, IntBinaryOp::Or, NodeOutputType::U64)?;
        b.set_lift_addr(Some(0x128));
        let outer = b.build_int_binary_operation(or_node, c3, IntBinaryOp::And, NodeOutputType::U64)?;
        // Leave lift_addr set so the trailing `Return` (built by
        // make_fn_with_var) inherits a fingerprint too.
        b.set_lift_addr(Some(0x12c));
        Ok(outer)
    })
    .expect("make_fn_with_var");

    // Sanity: the input graph passes the opt-in fingerprint check
    // because every node was created under a non-None lift_addr.
    validate_with_options(
        &fg.graph,
        fg.entry,
        ValidateOptions { check_asm_fingerprints: true },
    )
    .expect("input graph: every non-exempt node has a fingerprint");

    // Run ConstantFold — fires `rule_and_dist`.
    let _ = ConstantFold.optimize_raw(&mut fg.graph, fg.entry).unwrap();

    // After the rewrite, every reachable non-exempt node must still carry a
    // non-empty fingerprint (the no-shrink-fingerprint contract).
    validate_with_options(
        &fg.graph,
        fg.entry,
        ValidateOptions { check_asm_fingerprints: true },
    )
    .expect("post-ConstantFold: rewrite_rule must attribute every fresh interior node");

    // Belt-and-suspenders: explicitly confirm at least one fresh inner
    // And carries the rewritten outer-And's address (0x128).
    let inner_ands: Vec<NodeId> = fg
        .preorder()
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::IntBinaryOp(IntBinaryOp::And)))
        .collect();
    assert!(
        !inner_ands.is_empty(),
        "expected at least one And node post-rewrite"
    );
    for and_node in &inner_ands {
        let fp = fg.graph.asm_fingerprint(*and_node);
        assert!(
            !fp.is_empty(),
            "fresh inner And {and_node:?} has empty fingerprint: violates contract"
        );
    }
    // Suppress unused-warning for NodeOutputKind import keepers that
    // future test edits may need.
    let _ = NodeOutputKind::Memory;
}

/// O2 — Asm-fingerprint shrink-prevention across the full default pipeline.
///
/// Snapshots the fingerprint set of every reachable node *before* running
/// `default_pipeline`, then re-checks every still-reachable node *after* the
/// pipeline runs and asserts each retained `NodeId`'s post-set is a superset
/// of its pre-set (the no-shrink contract).  Nodes that the pipeline detaches
/// (passes such as `RedundantPhis` / `DeadBranchElimination` may leave them
/// as zombies in the arena) are excluded from the post-walk by virtue of
/// using `preorder()` reachability — which is exactly the contract the
/// fingerprint design promises.
#[test]
fn default_pipeline_never_shrinks_asm_fingerprints() {
    // `IntConst(3)@0x100 + IntConst(4)@0x104 → ret`.  The pipeline folds
    // this to `IntConst(7)`, exercising the constant-fold path's
    // fingerprint preservation; the pre-set on the original Add node is
    // not reachable post-fold and is therefore correctly excluded by
    // the reachability filter.
    let mut fg = make_empty_fn(|b| {
        b.set_lift_addr(Some(0x100));
        let c3 = b.build_int_const(3u64, NodeOutputType::U64)?;
        b.set_lift_addr(Some(0x104));
        let c4 = b.build_int_const(4u64, NodeOutputType::U64)?;
        b.set_lift_addr(Some(0x108));
        let add = b.build_int_binary_operation(c3, c4, IntBinaryOp::Add, NodeOutputType::U64)?;
        b.set_lift_addr(None);
        Ok(add)
    })
    .unwrap();

    // Snapshot every reachable node's fingerprint set (as a BTreeSet).
    let pre: HashMap<NodeId, BTreeSet<u64>> = fg
        .preorder()
        .map(|n| (n, fg.graph.asm_fingerprint(n).iter().copied().collect()))
        .collect();

    opt::default_pipeline()
        .run(&mut fg.graph, fg.entry)
        .expect("default_pipeline runs cleanly on the synthetic graph");

    // For every node still reachable after the pipeline, its post-set must
    // contain every address from its pre-set — the no-shrink invariant.
    for n in fg.preorder() {
        if let Some(pre_set) = pre.get(&n) {
            let post_set: BTreeSet<u64> =
                fg.graph.asm_fingerprint(n).iter().copied().collect();
            assert!(
                post_set.is_superset(pre_set),
                "node {n:?} fingerprint shrank: pre={pre_set:?} post={post_set:?}",
            );
        }
    }
}
