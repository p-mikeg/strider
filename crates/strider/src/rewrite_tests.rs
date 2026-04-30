//! Unit tests for the [`GraphRewriter`] façade.
//!
//! These tests exercise the façade's contract directly using a
//! synthetic [`FunctionBuilder`] — no Sleigh/CFG roundtrip.  Each
//! test pins one slice of the API:
//!
//! 1. `apply_rule_with_no_match_returns_zero_applications`
//! 2. `apply_rule_with_one_match_returns_one_application`
//! 3. `apply_rules_round_robin_reaches_fixed_point`
//! 4. `re_optimize_is_idempotent`
//! 5. `apply_rule_preserves_use_list_integrity` (validate after rewrite)

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ir::node::NodeOutputType;
use ir::{FunctionBuilder, IntBinaryOp};
use pattern::{add, boxed_rule, int_const, rewrite_rule, sub, var, Capture};

use super::GraphRewriter;

/// Build a tiny function:
///
///   fn() -> u64 { return 7; }
///
/// — a single `IntConst(7)` returned via `build_return`.  No Add /
/// Sub / Load nodes — used by the no-match test.
fn one_const_fn(k: u64) -> ir::BuiltFunctionGraph {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    let v = b.build_int_const(k, NodeOutputType::U64).unwrap();
    b.build_return(Some(v), &[]).unwrap();
    b.build().unwrap()
}

/// Build a function:
///
///   fn() -> u64 { return Add(7, 0); }
///
/// — exactly one `Add(IntConst(7), IntConst(0))`.  The `add(x, 0) → x`
/// rule fires once on this fixture.
fn add_x_plus_zero(x: u64) -> ir::BuiltFunctionGraph {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    let lhs = b.build_int_const(x, NodeOutputType::U64).unwrap();
    let rhs = b.build_int_const(0u64, NodeOutputType::U64).unwrap();
    let sum = b
        .build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U64)
        .unwrap();
    b.build_return(Some(sum), &[]).unwrap();
    b.build().unwrap()
}

/// Build a function:
///
///   fn() -> u64 { return Sub(Add(a, 0), Add(b, 0)); }
///
/// Two **distinct** Add subtrees (different LHS constants `a` and
/// `b`) feeding a Sub.  Both subtrees collapse via `add(x, 0) → x`
/// — that's two rule firings.  The Sub stays (its inputs are
/// distinct constants `a` and `b`, so `sub(y, y) → 0` doesn't
/// match).  Pinning the round-robin contract: `apply_rules` walks
/// every reachable node once per call, so 2 firings on a single
/// call.
fn sub_of_two_add_zeros(a: u64, b: u64) -> ir::BuiltFunctionGraph {
    let mut bd = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
    let region = bd.create_region().unwrap();
    bd.set_entry_region(region).unwrap();
    bd.set_region(region);
    let ac = bd.build_int_const(a, NodeOutputType::U64).unwrap();
    let bc = bd.build_int_const(b, NodeOutputType::U64).unwrap();
    let z0 = bd.build_int_const(0u64, NodeOutputType::U64).unwrap();
    let lhs = bd
        .build_int_binary_operation(ac, z0, IntBinaryOp::Add, NodeOutputType::U64)
        .unwrap();
    let rhs = bd
        .build_int_binary_operation(bc, z0, IntBinaryOp::Add, NodeOutputType::U64)
        .unwrap();
    let diff = bd
        .build_int_binary_operation(lhs, rhs, IntBinaryOp::Sub, NodeOutputType::U64)
        .unwrap();
    bd.build_return(Some(diff), &[]).unwrap();
    bd.build().unwrap()
}

/// Counts reachable Add nodes — the easy way to assert "the rule
/// fired" without poking at internal graph slot ids.
fn count_adds(g: &ir::BuiltFunctionGraph) -> usize {
    g.preorder()
        .filter(|nid| {
            matches!(
                g.graph.node_kind(*nid),
                ir::node::NodeKind::IntBinaryOp(IntBinaryOp::Add),
            )
        })
        .count()
}

fn count_subs(g: &ir::BuiltFunctionGraph) -> usize {
    g.preorder()
        .filter(|nid| {
            matches!(
                g.graph.node_kind(*nid),
                ir::node::NodeKind::IntBinaryOp(IntBinaryOp::Sub),
            )
        })
        .count()
}

#[test]
fn apply_rule_with_no_match_returns_zero_applications() -> anyhow::Result<()> {
    // Function returns a bare IntConst — no Add nodes anywhere.
    // `add(x, 0) → x` cannot fire; `apply_rule` must return 0.
    let mut built = one_const_fn(7);
    let x = Capture::new();
    let rule = rewrite_rule(add(var(x), int_const(0)), var(x));
    let mut rewriter = GraphRewriter::wrap_built(&mut built);
    let n = rewriter.apply_rule(rule)?;
    assert_eq!(n, 0, "rule must not fire on a graph without any Add node");
    Ok(())
}

#[test]
fn apply_rule_with_one_match_returns_one_application() -> anyhow::Result<()> {
    // Function returns `Add(7, 0)`.  Exactly one Add reachable;
    // `add(x, 0) → x` must fire exactly once.  After the rewrite,
    // the Return's value-input is rewired to the `7` constant
    // directly — verifiable via `count_adds == 0` on the
    // post-rewrite graph (the Add's only consumer was the Return,
    // which now feeds off `7`).
    let mut built = add_x_plus_zero(7);
    assert_eq!(count_adds(&built), 1, "fixture must have exactly one Add");
    let x = Capture::new();
    let rule = rewrite_rule(add(var(x), int_const(0)), var(x));
    let mut rewriter = GraphRewriter::wrap_built(&mut built);
    let n = rewriter.apply_rule(rule)?;
    assert_eq!(n, 1, "exactly one application expected");
    // After replace_all_uses, the Add becomes unreachable: its
    // only consumer's input was retargeted to the 7 constant.
    assert_eq!(
        count_adds(&built),
        0,
        "post-rewrite reachable graph must have zero Add nodes",
    );
    Ok(())
}

#[test]
fn apply_rules_round_robin_reaches_fixed_point() -> anyhow::Result<()> {
    // Function: `Sub(Add(a, 0), Add(b, 0))` (two distinct Adds).
    // Run two rules round-robin to a fixed point:
    //   1. `add(x, 0) → x`  — fires twice (once per subtree).
    //   2. `sub(y, y) → 0`  — never fires here (a ≠ b after fold,
    //      so the Sub's inputs are distinct).
    // Pins the round-robin walk contract: `apply_rules` calls
    // `apply_rules_in_order` (every rule once per root) over the
    // whole reachable preorder, so on a single call it fires the
    // first rule at both Add candidates.  Subsequent calls return
    // 0 (nothing further to do — the Adds are now unreachable
    // from `Return`'s now-direct input).
    let mut built = sub_of_two_add_zeros(11, 13);
    assert_eq!(count_adds(&built), 2, "fixture has two Adds");
    assert_eq!(count_subs(&built), 1, "fixture has one Sub");

    let y = Capture::new();
    let z = Capture::new();
    let rules: Vec<pattern::BoxedRule> = vec![
        boxed_rule(rewrite_rule(add(var(y), int_const(0)), var(y))),
        boxed_rule(rewrite_rule(sub(var(z), var(z)), int_const(0))),
    ];
    let mut rewriter = GraphRewriter::wrap_built(&mut built);

    // Drive the rewriter to a fixed point by re-applying rules
    // until none fire.  The user-facing contract: "keep going
    // until the graph stabilises".
    let mut total: usize = 0;
    for _ in 0..16 {
        let n = rewriter.apply_rules(&rules)?;
        total += n;
        if n == 0 {
            break;
        }
    }
    assert!(total >= 2, "rule must fire at least twice on the two Adds");
    // Both Adds collapse via the first rule — the post-rewrite
    // reachable graph has zero Adds.  The Sub stays (distinct
    // inputs, so `sub(z, z)` never matched).
    assert_eq!(count_adds(&built), 0, "round-robin must collapse all Adds");
    assert_eq!(count_subs(&built), 1, "Sub stays — its inputs are distinct constants");
    Ok(())
}

#[test]
fn re_optimize_is_idempotent() -> anyhow::Result<()> {
    // After running the default pipeline once on `Add(7, 0)`,
    // ConstantFold has already collapsed the Add to a bare
    // IntConst.  A second run must produce the same graph state
    // (zero new changes, same reachable count).
    let mut built = add_x_plus_zero(7);
    let pipeline = opt::default_pipeline();
    let mut rewriter = GraphRewriter::wrap_built(&mut built);
    rewriter.re_optimize(&pipeline)?;
    let count_after_first = built.preorder().count();
    let mut rewriter2 = GraphRewriter::wrap_built(&mut built);
    rewriter2.re_optimize(&pipeline)?;
    let count_after_second = built.preorder().count();
    assert_eq!(
        count_after_first, count_after_second,
        "re_optimize is idempotent — graph shape stable across repeated runs",
    );
    Ok(())
}

#[test]
fn apply_rule_preserves_use_list_integrity() -> anyhow::Result<()> {
    // The rewriter goes through `pattern::rewrite_rule` →
    // `replace_all_uses`, which uses the bidirectional use-list.
    // After the rewrite, `ir::validate::validate` must pass —
    // Layer B is the use-list-consistency check.  Pin this here
    // so any future change that breaks use-list bookkeeping
    // surfaces as a unit-test failure.
    let mut built = add_x_plus_zero(7);
    let x = Capture::new();
    let rule = rewrite_rule(add(var(x), int_const(0)), var(x));
    let mut rewriter = GraphRewriter::wrap_built(&mut built);
    rewriter.apply_rule(rule)?;
    // Run validate directly (Layer A + B + C).  If any layer
    // fails we surface the bundle as a strider error.
    ir::validate::validate(&built.graph, built.entry)
        .map_err(|e| anyhow::anyhow!("assertion failed: validate failed: {e}"))
}
