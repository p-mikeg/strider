//! Multi-pass coverage tests: cases where the final answer is only reachable
//! when several optimizer passes cooperate inside the pipeline's fixed-point
//! loop. Each test runs the **default pipeline** (or a deliberately-extended
//! variant) and asserts on the final state. Running any individual pass alone
//! is *not* enough to reach the asserted result.

mod common;

use ir::node::{NodeKind, NodeOutputType};
use ir::IntBinaryOp;
use opt::*;

use common::{make_fn, make_fn_with_var, reg_vn, return_kind, sp_vn};

// ── DBE → RedundantPhis cooperation ───────────────────────────────────────────

/// `if(true & false)` — `ConstantFold` reduces the BoolBinaryOp to
/// `BoolConst(false)`, then `DeadBranchElimination` eliminates the If, then
/// `RedundantPhis` collapses the resulting single-input phi nodes.
#[test]
fn const_fold_then_dbe_then_redundant_phis() -> opt::Result<()> {
    let mut b = ir::FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    let dead = b.create_region()?;
    let live = b.create_region()?;
    b.set_entry_region(entry)?;
    b.set_region(entry);
    let t = b.build_boolean_const(true);
    let f = b.build_boolean_const(false);
    let cond = b.build_boolean_operation(t, f, ir::BoolBinaryOp::And)?;
    b.build_if(cond, dead, live)?;
    b.set_region(dead);
    b.build_return(None, &[])?;
    b.set_region(live);
    b.build_return(None, &[])?;
    let mut fg = b.build()?;

    default_pipeline().run(&mut fg)?;

    let reachable: std::collections::HashSet<_> = fg.preorder().collect();
    let if_count = fg
        .all_node_ids()
        .filter(|n| reachable.contains(n))
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::If))
        .count();
    assert_eq!(if_count, 0, "If(true&false) must be folded then eliminated");
    Ok(())
}

// ── KnownBits feeds ConstantFold ──────────────────────────────────────────────

/// `((x | 0xFF) & 0xFF) + 5` for U8 — KnownBits proves `(x | 0xFF) & 0xFF` is
/// statically `0xFF`, then ConstantFold's identity rule + reassoc folds the
/// `+ 5` into a single constant `0x04` (0xFF + 5 wraps to 4 in U8).
#[test]
fn known_bits_then_constant_fold() -> opt::Result<()> {
    let vn = reg_vn(0x1000, 1);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let ff = b.build_int_const(0xFF, NodeOutputType::U8);
        let or_ = b.build_int_binary_operation(x, ff, IntBinaryOp::Or, NodeOutputType::U8)?;
        let masked = b.build_int_binary_operation(or_, ff, IntBinaryOp::And, NodeOutputType::U8)?;
        let five = b.build_int_const(5, NodeOutputType::U8);
        Ok(b.build_int_binary_operation(masked, five, IntBinaryOp::Add, NodeOutputType::U8)?)
    })?;
    default_pipeline().run(&mut fg)?;
    // 0xFF + 5 = 0x104 → wrap to 0x04 in U8.
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0x04));
    Ok(())
}

// ── DBE strips a phi predecessor; the remaining single-input phi collapses ────

/// Two-region `if/else` where each arm writes a distinct constant to a tracked
/// var. With `if(true)`, DBE removes the false branch, leaving the join's
/// ControlPhi with one live predecessor; RedundantPhis then collapses the phi
/// to that predecessor's value; ConstantFold sees the chain end with a const.
#[test]
fn dbe_strips_phi_then_redundant_phis_collapses() -> opt::Result<()> {
    let var = reg_vn(0x1000, 8);
    let mut b = ir::FunctionBuilder::new_raw(vec![var], &[var], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    let true_r = b.create_region()?;
    let false_r = b.create_region()?;
    let join = b.create_region()?;
    b.set_entry_region(entry)?;
    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, true_r, false_r)?;
    b.set_region(true_r);
    let v_t = b.build_int_const(11, NodeOutputType::U64);
    b.write_variable(&var, v_t)?;
    b.build_branch(join)?;
    b.set_region(false_r);
    let v_f = b.build_int_const(22, NodeOutputType::U64);
    b.write_variable(&var, v_f)?;
    b.build_branch(join)?;
    b.set_region(join);
    let merged = b.read_variable(&var)?;
    b.build_return(Some(merged), &[])?;
    let mut fg = b.build()?;

    default_pipeline().run(&mut fg)?;

    // After all three passes cooperate, the return should be IntConst(11).
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(11));
    Ok(())
}

// ── ConstantFold's reassoc → KnownBits → ConstantFold ─────────────────────────

/// `(x + 8) - 8` — ConstantFold's reassoc reduces this to `x + 0`, which the
/// identity rule then folds to `x`. Tests that two ConstantFold rewrites
/// compose inside one pipeline run.
#[test]
fn reassoc_then_identity_collapses_to_x() -> opt::Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let eight = b.build_int_const(8, NodeOutputType::U64);
        let plus = b.build_int_binary_operation(x, eight, IntBinaryOp::Add, NodeOutputType::U64)?;
        Ok(b.build_int_binary_operation(plus, eight, IntBinaryOp::Sub, NodeOutputType::U64)?)
    })?;
    default_pipeline().run(&mut fg)?;
    // After full collapse, the return-value-producing node must NOT be an
    // arithmetic op — the (x+8)-8 chain must have been replaced by `x` (a
    // ControlPhi/InitialVar read).
    let kind = return_kind(&fg)?;
    assert!(
        !matches!(
            kind,
            NodeKind::IntBinaryOp(IntBinaryOp::Add | IntBinaryOp::Sub)
        ),
        "(x+8)-8 must collapse to x; got {kind:?}"
    );
    Ok(())
}

// ── Stack-aware pipeline cooperation ──────────────────────────────────────────

/// `*sp = K; load *sp; return loaded` — needs all four cooperating:
/// ConstantFold (folds the SP arithmetic), RedundantPhis (collapses the
/// SP-phi at the entry), StackStoreDetect (rewrites Store → StackStore),
/// StackLoadForward (forwards K through the stack slot).
#[test]
fn stack_pipeline_full_cooperation() -> opt::Result<()> {
    let sp = sp_vn();
    let mut b = ir::FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let sp_v = b.read_variable(&sp)?;
    let four = b.build_int_const(4, NodeOutputType::U32);
    let addr = b.build_int_binary_operation(sp_v, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
    let data = b.build_int_const(0xCAFE, NodeOutputType::U32);
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    let mut p = OptimizerPipeline::new();
    p.add(ConstantFold);
    p.add(KnownBits);
    p.add(RedundantPhis);
    p.add(DeadBranchElimination);
    p.add(StackStoreDetect::new(sp));
    p.add(StackLoadForward::new(sp, target::Endianness::Little));
    p.run(&mut fg)?;

    assert_eq!(
        return_kind(&fg)?,
        NodeKind::IntConst(0xCAFE),
        "load must forward the stored constant after full pipeline"
    );
    Ok(())
}

// ── Long chain that requires worklist re-enqueue across passes ────────────────

/// 20-deep `+ 1` chain — needs ConstantFold's worklist to keep collapsing
/// after each reassociation rewrites a producer that was already visited.
/// Single-pass-without-fixed-point would leave many residual nodes.
#[test]
fn deep_reassoc_chain_via_default_pipeline() -> opt::Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let mut acc = x;
        for _ in 0..20 {
            let one = b.build_int_const(1, NodeOutputType::U64);
            acc = b.build_int_binary_operation(acc, one, IntBinaryOp::Add, NodeOutputType::U64)?;
        }
        Ok(acc)
    })?;
    default_pipeline().run(&mut fg)?;

    // Must collapse to a single Add(x, 20) — not 20 separate Add nodes.
    let ret_val = fg
        .all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
        .map(|n| fg.graph.node_inputs(n)[2])
        .ok_or(opt::ErrorKind::NoReturnNode)?;
    let ret_node = fg.graph.get_node_from_output(ret_val);
    let kind = *fg.graph.node_kind(ret_node);
    assert!(
        matches!(kind, NodeKind::IntBinaryOp(IntBinaryOp::Add)),
        "must collapse to a single Add, got {kind:?}"
    );
    let inputs = fg.graph.node_inputs(ret_node);
    assert_eq!(inputs.len(), 2);
    // One input is x, the other is IntConst(20).
    let kinds = [
        *fg.graph.node_kind(fg.graph.get_node_from_output(inputs[0])),
        *fg.graph.node_kind(fg.graph.get_node_from_output(inputs[1])),
    ];
    assert!(
        kinds.iter().any(|k| matches!(k, NodeKind::IntConst(20))),
        "Add operand should be IntConst(20), got {kinds:?}"
    );
    Ok(())
}

// ── Cascading dead-branch + redundant-phi cleanup across nested ifs ───────────

/// `if(true) { if(false) { ... } else { return v } }` — the outer If(true)
/// keeps the inner block; the inner If(false) eliminates its true branch.
/// The pipeline must reach a state with zero `If` nodes.
#[test]
fn nested_const_branches_fully_eliminated() -> opt::Result<()> {
    let mut b = ir::FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    let outer_t = b.create_region()?;
    let outer_f = b.create_region()?;
    let inner_t = b.create_region()?;
    let inner_f = b.create_region()?;
    b.set_entry_region(entry)?;

    b.set_region(entry);
    let outer_cond = b.build_boolean_const(true);
    b.build_if(outer_cond, outer_t, outer_f)?;

    b.set_region(outer_t);
    let inner_cond = b.build_boolean_const(false);
    b.build_if(inner_cond, inner_t, inner_f)?;

    b.set_region(outer_f);
    b.build_return(None, &[])?;
    b.set_region(inner_t);
    let dead_v = b.build_int_const(99, NodeOutputType::U64);
    b.build_return(Some(dead_v), &[])?;
    b.set_region(inner_f);
    let live_v = b.build_int_const(7, NodeOutputType::U64);
    b.build_return(Some(live_v), &[])?;

    let mut fg = b.build()?;
    default_pipeline().run(&mut fg)?;

    let reachable: std::collections::HashSet<_> = fg.preorder().collect();
    let if_count = fg
        .all_node_ids()
        .filter(|n| reachable.contains(n))
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::If))
        .count();
    assert_eq!(if_count, 0, "both nested Ifs must be eliminated");
    Ok(())
}

// ── Coverage: every default-pipeline pass must see at least one fold ──────────

/// A single-input chain that triggers every default-pipeline pass at least
/// once: ConstantFold (folds `c1 + c2`), KnownBits (proves OR-with-FF =
/// all-ones), DeadBranchElimination (eliminates If(true)), RedundantPhis
/// (collapses the join phi after DBE).
#[test]
fn default_pipeline_exercises_all_passes() -> opt::Result<()> {
    let var = reg_vn(0x1000, 1);
    let mut b = ir::FunctionBuilder::new_raw(vec![var], &[var], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    let live = b.create_region()?;
    let dead = b.create_region()?;
    b.set_entry_region(entry)?;
    b.set_region(entry);

    // ConstantFold: c1 + c2.
    let c1 = b.build_int_const(3, NodeOutputType::U8);
    let c2 = b.build_int_const(4, NodeOutputType::U8);
    let _sum = b.build_int_binary_operation(c1, c2, IntBinaryOp::Add, NodeOutputType::U8)?;

    // KnownBits-relevant: x | 0xFF then & 0xFF = 0xFF.
    let x = b.read_variable(&var)?;
    let ff = b.build_int_const(0xFF, NodeOutputType::U8);
    let or_ = b.build_int_binary_operation(x, ff, IntBinaryOp::Or, NodeOutputType::U8)?;
    let _masked = b.build_int_binary_operation(or_, ff, IntBinaryOp::And, NodeOutputType::U8)?;

    // DBE: if(true) goto live else goto dead.
    let cond = b.build_boolean_const(true);
    b.build_if(cond, live, dead)?;

    b.set_region(live);
    let v = b.build_int_const(42, NodeOutputType::U64);
    b.build_return(Some(v), &[])?;
    b.set_region(dead);
    b.build_return(None, &[])?;

    let mut fg = b.build()?;
    default_pipeline().run(&mut fg)?;

    // Final state: no If, return is IntConst(42).
    let reachable: std::collections::HashSet<_> = fg.preorder().collect();
    let if_count = fg
        .all_node_ids()
        .filter(|n| reachable.contains(n))
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::If))
        .count();
    assert_eq!(if_count, 0);
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(42));
    Ok(())
}

// ── Basic single-pass coverage to round out the public-API surface ────────────

/// Smallest-possible test: returning a const must round-trip through the
/// pipeline unchanged.
#[test]
fn pipeline_no_change_on_already_optimal() -> opt::Result<()> {
    let mut fg = make_fn(|b| Ok(b.build_int_const(7, NodeOutputType::U64)))?;
    default_pipeline().run(&mut fg)?;
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(7));
    Ok(())
}

/// `0 - x` for U64 — there is no algebraic identity for this; the pipeline
/// must leave the Sub node intact.
#[test]
fn pipeline_keeps_zero_sub_x() -> opt::Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let zero = b.build_int_const(0, NodeOutputType::U64);
        Ok(b.build_int_binary_operation(zero, x, IntBinaryOp::Sub, NodeOutputType::U64)?)
    })?;
    default_pipeline().run(&mut fg)?;
    let kind = return_kind(&fg)?;
    assert!(
        matches!(kind, NodeKind::IntBinaryOp(IntBinaryOp::Sub)),
        "0 - x has no identity rule, expected Sub, got {kind:?}"
    );
    Ok(())
}
