//! Integration tests for [`strider_opt::apply_rules_count`] driven via
//! strider-orchestrator's orchestrator pipeline.
//!
//! Each test exercises the rewrite / re-optimize flow against a
//! real Sleigh-lifted function (or a hand-built one), pinning the
//! user-facing contract:
//!
//! 1. `replace_switch_address_with_const_collapses_switch_after_reoptimize` —
//!    directly rewrites a 3-target `Switch`'s `address` input (the
//!    manual-edit analogue of "replace the selector", since there's no
//!    rewrite-rule support for rooting on a `Switch`, which has no value
//!    output) to `IntConst(K_0)`, then re-optimises; `DeadBranchElimination`
//!    collapses the constant-address `Switch` to its matching arm, and the
//!    graph stays valid.
//! 2. `rewrite_rule_targeting_old_if_ladder_shape_is_a_no_op_against_switch_dispatch` —
//!    the OLD if-ladder-shaped rewrite rule (match `Eq(_, K)`, replace
//!    with `BoolConst`) must safely no-op against a `Switch`-lowered
//!    jump table: it fires zero times and leaves the `Switch` untouched.
//! 3. `replace_input_then_reoptimize_then_replace_again_works` —
//!    multiple edits compose without leaving the rewriter.
//! 4. `re_optimize_without_changes_is_no_op` — calling re_optimize on
//!    an already-optimised graph doesn't grow / shrink the reachable
//!    set.
//! 5. `manual_rewrite_does_not_break_validate` — after every rewrite,
//!    `strider_ir::validate::validate` passes.
//! 6. `apply_rule_using_pattern_var_capture` — non-trivial pattern
//!    flow: `add(var(x), int_const(0u128)) -> var(x)` end-to-end on a
//!    Sleigh-lifted function that contains an Add-by-zero shape.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use strider_ir::node::{NodeKind, ValueType};
use strider_ir::{Function, IRBuilderExt, IRViewer, IRWalker, IntBinaryOp};
use strider_ir_test_utils::IrWalkerEx;
use strider_opt::{EditFunction, apply_rules_count, rewrite_rule};
use strider_pattern::{Capture, CaptureExt, add, int_const, var};

mod common;

fn count_switches(function: &Function) -> usize {
    function.count_kind(|k| matches!(k, NodeKind::Switch))
}

/// Locates the unique `Switch` node in `function`. Panics if zero or more
/// than one is present — either case indicates a fixture-construction bug.
fn find_unique_switch(function: &Function) -> strider_ir::node::NodeId {
    let mut iter = function
        .walk()
        .filter(|&nid| matches!(function.node_kind(nid), NodeKind::Switch));
    let first = iter
        .next()
        .expect("fixture must contain exactly one Switch node");
    assert!(
        iter.next().is_none(),
        "fixture has more than one Switch node"
    );
    first
}

/// Build a tiny non-Sleigh function: `fn() -> u64 { return Add(K, 0); }`.
/// Uses [`FunctionBuilder::new_raw`] directly so the test doesn't depend
/// on any Sleigh fixtures.
fn add_k_plus_zero(k: u64) -> Function {
    let mut b = strider_ir_test_utils::empty_builder().unwrap();
    let region = b.create_region_all().unwrap();
    b.set_entry_region_all(region).unwrap();
    b.set_region(region);
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let lhs = b.build_int_const(k, ValueType::I64).unwrap();
    let rhs = b.build_int_const(0u64, ValueType::I64).unwrap();
    let sum = b
        .build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, ValueType::I64)
        .unwrap();
    b.build_return(Some(sum), &[]).unwrap();
    b.set_lift_addr(None);
    b.build().unwrap()
}

fn count_adds(function: &Function) -> usize {
    function.count_kind(|k| matches!(k, NodeKind::IntBinaryOp(IntBinaryOp::Add)))
}

// ── Test 1 — replace switch selector with const, collapse to one branch ─────

/// 3-target Switch lifted via `build_switch` produces exactly one
/// `NodeKind::Switch` node (inputs `[ctrl, address]`) — no cmp, no
/// If-ladder.  A `Switch` has no value output, so it can't root a
/// `rewrite_rule`; the manual-edit analogue of "replace the selector"
/// goes through `EditFunction`'s low-level input-rewrite primitive:
/// directly rewrite the Switch's `address` input (slot 1) to
/// `IntConst(K_0)`.  Because `K_0 == targets[0]` (case 0),
/// `DeadBranchElimination` then collapses the constant-address `Switch`
/// to its single matching arm — the `Switch` is killed, control flows
/// straight to case 0's region, no `If` nodes appear — and the graph
/// must remain valid after the collapse.
#[test]
fn replace_switch_address_with_const_collapses_switch_after_reoptimize() -> anyhow::Result<()> {
    let (bytes, base, ba, targets) = common::synth_jmp_rax_with_targets(3);
    let (mut g, _strider, _cc) = common::analyze_with_known_targets(&bytes, base, ba, &targets);
    assert_eq!(
        common::count_ifs(&g),
        0,
        "3-target dispatch produces zero If nodes"
    );
    assert_eq!(
        count_switches(&g),
        1,
        "3-target dispatch lifts to exactly one Switch node"
    );
    let switch_id = find_unique_switch(&g);
    assert_eq!(
        g.node_outputs(switch_id).len(),
        3,
        "Switch has one Control output per target",
    );

    // Directly rewrite the Switch's `address` input (inputs are
    // `[ctrl, address]`, so slot 1) to `IntConst(K_0)`.  The displaced
    // `idx`-read is an (exempt) `InitialVar` with no asm history to
    // absorb, so stamp the fresh constant's fingerprint from the
    // `Switch` node itself (whose fingerprint traces back to the real
    // `jmp rax` instruction) to satisfy the always-on fingerprint check.
    let addr_use = g.node_input_id_at(switch_id, 1)?;
    {
        let mut ctx = EditFunction::new(&mut g);
        let k0 = ctx.build_int_const(targets[0], ValueType::I64)?;
        let k0_node = ctx.function().producer(k0);
        ctx.function_mut()
            .side_tables_mut()
            .extend_asm_fingerprint_from(k0_node, switch_id);
        ctx.redirect_input(addr_use, k0);
        ctx.clean();
    }

    let pipeline = strider_orchestrator::opt::default_pipeline();
    pipeline.run(&mut g, &mut strider_orchestrator::opt::OptCtx::new(None))?;

    // The address now folds to `IntConst(K_0)` (== `targets[0]`, i.e. case 0),
    // so `DeadBranchElimination` collapses the constant-address Switch to its
    // single matching arm: the Switch node is killed and control flows directly
    // to case 0's region. No Switch survives, and (as always for a
    // Switch-lowered dispatch) no If nodes are introduced.
    assert_eq!(
        count_switches(&g),
        0,
        "constant-address Switch collapses to its matching arm (DeadBranchElimination)",
    );
    assert_eq!(
        common::count_ifs(&g),
        0,
        "Switch-lowered dispatch never produces If nodes, even after the collapse",
    );
    strider_ir::validate::validate(&g).map_err(|e| {
        anyhow::anyhow!("assertion failed: validate failed after switch-address rewrite: {e}")
    })?;
    Ok(())
}

// ── Test 2 — old if-ladder rewrite rule is a no-op against a Switch ─────────

/// **Regression guard: rewrite rules written against the old if-ladder
/// shape must not spuriously match the new `Switch` shape.**
/// `handle_switch` used to lower a jump table into an if-ladder of
/// `IntCmpOp::Equal` + `If` nodes, so a pattern-rewrite rule written
/// against that shape (match any `Eq(_, K)` cmp, replace with
/// `BoolConst`) used to fire once per ladder arm and, after
/// re-optimizing, collapse the dispatch to a single branch.  Now that
/// `handle_switch` emits a single `Switch` node instead (no cmp, no
/// If), the SAME rule must be a safe no-op: zero matches, the `Switch`
/// left completely untouched by the (no-op) rewrite + re-optimize.
#[test]
fn rewrite_rule_targeting_old_if_ladder_shape_is_a_no_op_against_switch_dispatch()
-> anyhow::Result<()> {
    let (bytes, base, ba, targets) = common::synth_jmp_rax_with_targets(3);
    let (mut g, _strider, _cc) = common::analyze_with_known_targets(&bytes, base, ba, &targets);
    assert_eq!(
        common::count_ifs(&g),
        0,
        "3-target Switch produces zero Ifs"
    );
    assert_eq!(
        count_switches(&g),
        1,
        "3-target dispatch lifts to exactly one Switch node"
    );

    let pipeline = strider_orchestrator::opt::default_pipeline();
    let rule_all_false = rewrite_rule(
        strider_pattern::int_eq(
            strider_pattern::any(),
            strider_pattern::any_int_const().capture(strider_pattern::Capture::new()),
        ),
        strider_pattern::bool_const(false),
    );
    let fired = {
        let mut ctx = EditFunction::new(&mut g);
        apply_rules_count(&mut ctx, std::slice::from_ref(&rule_all_false))?
    };
    assert_eq!(
        fired, 0,
        "an if-ladder-shaped rewrite rule must not match anything against a Switch-lowered dispatch",
    );
    pipeline.run(&mut g, &mut strider_orchestrator::opt::OptCtx::new(None))?;

    // The rule found nothing to rewrite, so the Switch (and its
    // zero-If shape) must be exactly as it was before the (no-op)
    // rewrite + re-optimize.
    assert_eq!(
        count_switches(&g),
        1,
        "Switch node must be untouched by the no-op rewrite + re_optimize",
    );
    assert_eq!(
        common::count_ifs(&g),
        0,
        "Switch-lowered dispatch produces zero If nodes before or after the no-op rewrite",
    );
    Ok(())
}

// ── Test 3 — multi-edit: rewrite, re-optimize, rewrite again ────────────────

#[test]
fn replace_input_then_reoptimize_then_replace_again_works() -> anyhow::Result<()> {
    // Hand-built fixture: two Adds, one in the entry, one downstream
    // via a second IntConst.  After rewrite + re-optimize, run a
    // second rewrite — the rewriter must support being called again
    // on the same graph after re_optimize ran.
    let mut b = strider_ir_test_utils::empty_builder().unwrap();
    let region = b.create_region_all().unwrap();
    b.set_entry_region_all(region).unwrap();
    b.set_region(region);
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let a = b.build_int_const(7u64, ValueType::I64).unwrap();
    let z = b.build_int_const(0u64, ValueType::I64).unwrap();
    let one = b.build_int_const(1u64, ValueType::I64).unwrap();
    let add1 = b
        .build_int_binary_operation(a, z, IntBinaryOp::Add, ValueType::I64)
        .unwrap();
    let add2 = b
        .build_int_binary_operation(add1, one, IntBinaryOp::Add, ValueType::I64)
        .unwrap();
    b.build_return(Some(add2), &[]).unwrap();
    b.set_lift_addr(None);
    let mut function = b.build().unwrap();

    assert_eq!(count_adds(&function), 2, "fixture has two Adds");

    let x = Capture::new();
    let rule_x_plus_zero = rewrite_rule(add(var(x), int_const(0u128)), var(x));
    let pipeline = strider_orchestrator::opt::default_pipeline();

    // Edit 1: collapse the `Add(7, 0)`.  Returns 1 application.
    let n1 = {
        let mut ctx = EditFunction::new(&mut function);
        apply_rules_count(&mut ctx, std::slice::from_ref(&rule_x_plus_zero))?
    };
    assert_eq!(n1, 1, "first rewrite collapses Add(7,0)");
    // re-optimise — propagates the constant through the second Add.
    pipeline.run(
        &mut function,
        &mut strider_orchestrator::opt::OptCtx::new(None),
    )?;

    // Edit 2: after re-optimize, ConstantFold has already
    // collapsed Add(7, 1) → IntConst(8), so the rewriter has nothing
    // left to do — but the call must still succeed (returns 0).
    let n2 = {
        let mut ctx = EditFunction::new(&mut function);
        apply_rules_count(&mut ctx, std::slice::from_ref(&rule_x_plus_zero))?
    };
    assert_eq!(
        n2, 0,
        "second rewrite finds no Add-by-zero patterns after re_optimize collapsed everything",
    );
    Ok(())
}

// ── Test 4 — re_optimize without changes is a no-op ─────────────────────────

#[test]
fn re_optimize_without_changes_is_no_op() -> anyhow::Result<()> {
    // Already-optimised graph.  Calling re_optimize must not change
    // the reachable-node count.
    let mut function = add_k_plus_zero(7);
    let pipeline = strider_orchestrator::opt::default_pipeline();

    pipeline.run(
        &mut function,
        &mut strider_orchestrator::opt::OptCtx::new(None),
    )?; // first run: collapses Add(7,0)
    let count_after_first = function.walk().count();

    pipeline.run(
        &mut function,
        &mut strider_orchestrator::opt::OptCtx::new(None),
    )?; // second run: no-op
    let count_after_second = function.walk().count();

    assert_eq!(
        count_after_first, count_after_second,
        "re_optimize on an already-stable graph is a no-op",
    );
    Ok(())
}

// ── Test 5 — manual rewrite does not break validate ─────────────────────────

#[test]
fn manual_rewrite_does_not_break_validate() -> anyhow::Result<()> {
    // After every rewrite, `strider_ir::validate::validate` must pass.
    // Local typing + use-list consistency + graph invariants — a broken
    // use-list would only surface here, hence pin it explicitly.
    let mut function = add_k_plus_zero(42);
    let x = Capture::new();
    let rule = rewrite_rule(add(var(x), int_const(0u128)), var(x));

    {
        let mut ctx = EditFunction::new(&mut function);
        apply_rules_count(&mut ctx, std::slice::from_ref(&rule))?;
    }

    strider_ir::validate::validate(&function)
        .map_err(|e| anyhow::anyhow!("assertion failed: validate failed after rewrite: {e}"))?;
    Ok(())
}

// ── Test 6 — pattern var capture via rewrite_rule ───────────────────────────

#[test]
fn apply_rule_using_pattern_var_capture() -> anyhow::Result<()> {
    // End-to-end exercise of the `strider_opt::rewrite_rule(lhs, rhs)`
    // flow with a non-trivial Capture capture on both sides.  Pattern:
    // `add(var(x), int_const(0u128)) -> var(x)`.  The capture binds the
    // matched LHS subtree's left input on the LHS, and the RHS uses
    // the same capture to materialise a "passthrough" — the
    // rewrite engine redirects the Add's uses to whatever `x` bound.
    //
    // Pin two contracts:
    //   1. `apply_count` returns the correct fire count (1 here).
    //   2. After the rewrite, the Return now consumes `x` directly
    //      (the Add became unreachable).
    let mut function = add_k_plus_zero(99);
    assert_eq!(count_adds(&function), 1, "fixture has one Add");

    let x = Capture::new();
    let rule = rewrite_rule(add(var(x), int_const(0u128)), var(x));
    let fired = {
        let mut ctx = EditFunction::new(&mut function);
        apply_rules_count(&mut ctx, std::slice::from_ref(&rule))?
    };
    assert_eq!(fired, 1, "Capture-capture rule fires exactly once");
    assert_eq!(
        count_adds(&function),
        0,
        "post-rewrite Add is unreachable — Return now feeds off `x` directly",
    );
    Ok(())
}
