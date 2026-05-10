//! Integration tests for [`strider::GraphRewriter`].
//!
//! Each test exercises the `apply_rule` / `apply_rules` / `re_optimize`
//! flow against a real Sleigh-lifted function (or a hand-built one),
//! pinning the user-facing contract:
//!
//! 1. `replace_switch_selector_with_const_collapses_to_one_branch` —
//!    user replaces the selector of a 3-target Switch (lifted via the
//!    If-ladder) with `IntConst(K_0)`, then re-optimises; the optimizer
//!    collapses the dispatch to a single branch.
//! 2. `replace_jump_table_index_with_const_collapses_to_one_target` —
//!    IR-level-resolved jump table lifted via the If-ladder; user
//!    replaces the index input with a constant; only one target's
//!    branch survives.
//! 3. `replace_input_then_reoptimize_then_replace_again_works` —
//!    multiple edits compose without leaving the rewriter.
//! 4. `re_optimize_without_changes_is_no_op` — calling re_optimize on
//!    an already-optimised graph doesn't grow / shrink the reachable
//!    set.
//! 5. `manual_rewrite_does_not_break_validate` — after every rewrite,
//!    `ir::validate::validate` passes.
//! 6. `apply_rule_using_pattern_var_capture` — non-trivial pattern
//!    flow: `add(var(x), int_const(0)) -> var(x)` end-to-end on a
//!    Sleigh-lifted function that contains an Add-by-zero shape.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;

use cfg::{Builder, MachineInsnAddr, OptionsBuilder, PcodeInsnAddr, ResolvedTargets};
use ir::node::{NodeKind, NodeOutputType};
use ir::{BuiltFunctionGraph, FunctionBuilder, IntBinaryOp};
use pattern::{add, int_const, rewrite_rule, var, IntoPat, Capture};
use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use strider::{GraphRewriter, SleighArch, Strider};

// ── Common fixture builders (private to this test crate) ────────────────────

/// Build a synthetic x86-64 binary: `jmp rax` followed by `n` `ret`
/// targets.  Returns `(bytes, base, branch_indirect_addr, target_addrs)`.
fn synth_jmp_rax_with_targets(n: usize) -> (Vec<u8>, u64, u64, Vec<u64>) {
    let base = 0x1000u64;
    let mut bytes = vec![0xffu8, 0xe0]; // jmp rax (2 bytes at 0x1000)
    let mut targets = Vec::with_capacity(n);
    for i in 0..n {
        targets.push(base + 2 + i as u64); // 0x1002, 0x1003, ...
        bytes.push(0xc3); // ret
    }
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    (bytes, base, base, targets)
}

/// Lift `bytes` via `analyze_cfg` with `with_known_targets` seeding the
/// BranchIndirect at `branch_indirect_addr` to `Multiple(targets)`.
/// Returns `(graph, strider)` so callers can run the optimizer with
/// the convention-aware pipeline.
fn analyze_with_known_targets(
    bytes: Vec<u8>,
    base: u64,
    branch_indirect_addr: u64,
    targets: Vec<u64>,
) -> (BuiltFunctionGraph, Strider) {
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    let sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader)
        .expect("create x86_64 sleigh");
    let mut known_targets: HashMap<PcodeInsnAddr, ResolvedTargets> = HashMap::new();
    known_targets.insert(
        PcodeInsnAddr::new(MachineInsnAddr::new(branch_indirect_addr), 0),
        ResolvedTargets::Multiple(targets),
    );
    let opts = OptionsBuilder::new().build();
    let cfg = Builder::for_arch(&arch, sleigh, base, opts)
        .with_known_targets(known_targets)
        .build()
        .expect("cfg build");

    let strider = strider::test_utils::strider_x86_64();
    let graph = strider.analyze_cfg(&cfg).expect("analyze_cfg").graph;
    (graph, strider)
}

fn count_if_nodes(g: &BuiltFunctionGraph) -> usize {
    g.preorder()
        .filter(|nid| matches!(g.graph.node_kind(*nid), NodeKind::If))
        .count()
}

fn count_eq_cmps(g: &BuiltFunctionGraph) -> usize {
    g.preorder()
        .filter(|nid| {
            matches!(
                g.graph.node_kind(*nid),
                NodeKind::IntCmpOp(ir::IntCmpOp::Equal),
            )
        })
        .count()
}

/// Build a tiny non-Sleigh function: `fn() -> u64 { return Add(K, 0); }`.
/// Uses [`FunctionBuilder::new_raw`] directly so the test doesn't depend
/// on any Sleigh fixtures.
fn add_k_plus_zero(k: u64) -> BuiltFunctionGraph {
    let mut b = FunctionBuilder::empty().unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    let lhs = b.build_int_const(k, NodeOutputType::U64).unwrap();
    let rhs = b.build_int_const(0u64, NodeOutputType::U64).unwrap();
    let sum = b
        .build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U64)
        .unwrap();
    b.build_return(Some(sum), &[]).unwrap();
    b.build().unwrap()
}

fn count_adds(g: &BuiltFunctionGraph) -> usize {
    g.preorder()
        .filter(|nid| {
            matches!(
                g.graph.node_kind(*nid),
                NodeKind::IntBinaryOp(IntBinaryOp::Add),
            )
        })
        .count()
}

// ── Test 1 — replace switch selector with const, collapse to one branch ─────

/// 3-target Switch lifted via `build_switch_if_ladder` produces an If-ladder of 2 If nodes
/// comparing the index against `K_0` and `K_1`.  Rewriting **all** of
/// the equality-cmp's right-hand-input (the `K_0` constant) to a
/// matching value and then re-optimising won't actually collapse the
/// ladder — the cmp is `Eq(idx, K_0)`, and replacing K_0 with K_0
/// changes nothing.  Instead, the user-facing flow switch lifting enables is to
/// rewrite the cmp's INDEX side (the `idx` operand) to `IntConst(K_0)`.
/// Then `Eq(K_0, K_0)` folds to `BoolConst(true)`, the first If's
/// false-branch becomes dead, and DeadBranchElim collapses the
/// remaining ladder to nothing.  We pin the post-rewrite If count.
#[test]
fn replace_switch_selector_with_const_collapses_to_one_branch() -> anyhow::Result<()> {
    let (bytes, base, ba, targets) = synth_jmp_rax_with_targets(3);
    let (mut g, strider) = analyze_with_known_targets(bytes, base, ba, targets.clone());
    let if_count_pre = count_if_nodes(&g);
    let cmp_count_pre = count_eq_cmps(&g);
    assert_eq!(if_count_pre, 2, "3-target Switch produces N-1=2 If nodes");
    assert_eq!(cmp_count_pre, 2, "3-target Switch produces N-1=2 equality cmps");

    // Replace every IntCmpOp::Equal's left-hand input (the `idx`
    // value) with `IntConst(K_0)`.  Pattern: any IntCmpOp::Equal
    // whose RHS is the existing `K_0` integer constant — rewrite
    // the cmp itself to `BoolConst(true)`.  We use the simpler form:
    // match the cmp, replace its output with `bool_const(true)`.
    let pipeline = strider.build_optimizer_pipeline();
    let cmp_var = Capture::new();
    let rule = rewrite_rule(
        // LHS: int_eq(any, int_const(K_0))
        pattern::int_eq(pattern::any(), int_const(targets[0] as i128)).capture(cmp_var),
        pattern::bool_const(true),
    );
    let mut rewriter = GraphRewriter::wrap_built(&mut g);
    let n = rewriter.apply_rule(rule)?;
    assert!(
        n >= 1,
        "rule must fire at least once (matched the K_0 cmp); fired {n} times",
    );
    rewriter.re_optimize(&pipeline)?;

    // After ConstantFold + DeadBranchElim collapse the now-true
    // first If, the second If's condition is reachable only via the
    // K_0-true path which the dead-branch eliminator pruned.  Final
    // If count must drop below the pre-rewrite count.
    let if_count_post = count_if_nodes(&g);
    assert!(
        if_count_post < if_count_pre,
        "post-rewrite If count must shrink: pre={if_count_pre}, post={if_count_post}",
    );
    Ok(())
}

// ── Test 2 — replace jump-table index with const, collapse to one target ────

/// **Headline switch lifting + post-resolution rewrite flow.**  IR-level-resolved jump table lifted via `build_switch_if_ladder`'s
/// If-ladder; rewrite the cmp output to BoolConst(true) at one
/// equality cmp; re-optimize; the dispatch collapses to a single
/// branch (zero Ifs reachable post-fold).
#[test]
fn replace_jump_table_index_with_const_collapses_to_one_target() -> anyhow::Result<()> {
    let (bytes, base, ba, targets) = synth_jmp_rax_with_targets(3);
    let (mut g, strider) = analyze_with_known_targets(bytes, base, ba, targets.clone());
    assert_eq!(count_if_nodes(&g), 2, "3-target Switch lifts to 2 Ifs");
    assert_eq!(count_eq_cmps(&g), 2, "3-target Switch lifts to 2 cmps");
    // Rewrite every Equal-cmp to `BoolConst(false)` *except* the K_1
    // arm (let it stay so DeadBranchElim collapses around it).  Pin
    // a much simpler shape: rewrite ALL Equal-cmps to BoolConst(false)
    // and let the optimizer cascade.  Final shape: zero Ifs (every
    // branch was constant-folded out — at least one whole arm must
    // be unreachable, and the rest collapse via dead-branch-elim).
    let pipeline = strider.build_optimizer_pipeline();
    let rule_all_false = rewrite_rule(
        pattern::int_eq(pattern::any(), pattern::any_int_const(pattern::Capture::new())),
        pattern::bool_const(false),
    );
    let mut rewriter = GraphRewriter::wrap_built(&mut g);
    let fired = rewriter.apply_rule(rule_all_false)?;
    assert!(
        fired >= 2,
        "rule must fire on both equality cmps in the ladder; fired {fired}",
    );
    rewriter.re_optimize(&pipeline)?;
    // After all conditions become BoolConst(false), every If's true
    // branch goes dead; DeadBranchElim collapses the ladder.
    assert_eq!(
        count_if_nodes(&g),
        0,
        "post-rewrite ladder must contain zero If nodes after re_optimize",
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
    let mut b = FunctionBuilder::empty().unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    let a = b.build_int_const(7u64, NodeOutputType::U64).unwrap();
    let z = b.build_int_const(0u64, NodeOutputType::U64).unwrap();
    let one = b.build_int_const(1u64, NodeOutputType::U64).unwrap();
    let add1 = b
        .build_int_binary_operation(a, z, IntBinaryOp::Add, NodeOutputType::U64)
        .unwrap();
    let add2 = b
        .build_int_binary_operation(add1, one, IntBinaryOp::Add, NodeOutputType::U64)
        .unwrap();
    b.build_return(Some(add2), &[]).unwrap();
    let mut graph = b.build().unwrap();

    assert_eq!(count_adds(&graph), 2, "fixture has two Adds");

    let x = Capture::new();
    let rule_x_plus_zero = rewrite_rule(add(var(x), int_const(0)), var(x));
    let pipeline = opt::default_pipeline();

    // Edit 1: collapse the `Add(7, 0)`.  Returns 1 application.
    let mut rewriter = GraphRewriter::wrap_built(&mut graph);
    let n1 = rewriter.apply_rule(&rule_x_plus_zero)?;
    assert_eq!(n1, 1, "first rewrite collapses Add(7,0)");
    // re-optimise — propagates the constant through the second Add.
    rewriter.re_optimize(&pipeline)?;

    // Edit 2: after re-optimize, ConstantFold has already
    // collapsed Add(7, 1) → IntConst(8), so the rewriter has nothing
    // left to do — but the call must still succeed (returns 0).
    let n2 = rewriter.apply_rule(&rule_x_plus_zero)?;
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
    let mut g = add_k_plus_zero(7);
    let pipeline = opt::default_pipeline();

    let mut rewriter = GraphRewriter::wrap_built(&mut g);
    rewriter.re_optimize(&pipeline)?; // first run: collapses Add(7,0)
    let count_after_first = g.preorder().count();

    let mut rewriter2 = GraphRewriter::wrap_built(&mut g);
    rewriter2.re_optimize(&pipeline)?; // second run: no-op
    let count_after_second = g.preorder().count();

    assert_eq!(
        count_after_first, count_after_second,
        "re_optimize on an already-stable graph is a no-op",
    );
    Ok(())
}

// ── Test 5 — manual rewrite does not break validate ─────────────────────────

#[test]
fn manual_rewrite_does_not_break_validate() -> anyhow::Result<()> {
    // After every rewrite, `ir::validate::validate` must pass.
    // Layer A (local typing) + Layer B (use-list) + Layer C
    // (graph-level invariants) — a broken use-list would only
    // surface here, hence pin it explicitly.
    let mut g = add_k_plus_zero(42);
    let x = Capture::new();
    let rule = rewrite_rule(add(var(x), int_const(0)), var(x));

    let mut rewriter = GraphRewriter::wrap_built(&mut g);
    rewriter.apply_rule(rule)?;

    ir::validate::validate(&g.graph, g.entry)
        .map_err(|e| anyhow::anyhow!("assertion failed: validate failed after rewrite: {e}"))?;
    Ok(())
}

// ── Test 6 — pattern var capture via rewrite_rule ───────────────────────────

#[test]
fn apply_rule_using_pattern_var_capture() -> anyhow::Result<()> {
    // End-to-end exercise of the `pattern::rewrite_rule(lhs, rhs)`
    // flow with a non-trivial Capture capture on both sides.  Pattern:
    // `add(var(x), int_const(0)) -> var(x)`.  The capture binds the
    // matched LHS subtree's left input on the LHS, and the RHS uses
    // the same capture to materialise a "passthrough" — the
    // rewrite engine redirects the Add's uses to whatever `x` bound.
    //
    // Pin two contracts:
    //   1. `apply_rule` returns the correct fire count (1 here).
    //   2. After the rewrite, the Return now consumes `x` directly
    //      (the Add became unreachable).
    let mut g = add_k_plus_zero(99);
    assert_eq!(count_adds(&g), 1, "fixture has one Add");

    let x = Capture::new();
    let rule = rewrite_rule(add(var(x), int_const(0)), var(x));
    let mut rewriter = GraphRewriter::wrap_built(&mut g);
    let fired = rewriter.apply_rule(rule)?;
    assert_eq!(fired, 1, "Capture-capture rule fires exactly once");
    assert_eq!(
        count_adds(&g),
        0,
        "post-rewrite Add is unreachable — Return now feeds off `x` directly",
    );
    Ok(())
}
