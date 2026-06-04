//! Rewrite-rule engine tests: `rewrite_rule`, `boxed_rule`, and the
//! error paths surfaced via the public anyhow surface and the rule's
//! `Ok(bool)` contract.
//!
//! Relocated from `strider-pattern`'s `pattern_matching` integration
//! harness when the rewrite machinery moved into `strider-opt`: the
//! rule constructors (`rewrite_rule`, `boxed_rule`, …) and the
//! `EditFunction` / `GraphRewriter` types now live in `strider_opt`, while
//! the LHS/RHS pattern builders (`add`, `var`, `int_const`, …) stay in
//! `strider_pattern`. The minimal `Tb` test-graph builder and the two
//! assertion helpers this file needs are inlined below so the test is
//! self-contained.
//!
//! A wildcard / predicate / control RHS is a compile-time error
//! (`rewrite_rule`'s RHS requires `TemplatePat`), so the former runtime
//! "RHS not buildable" tests are obsolete-by-design and dropped — the
//! constraint is still enforced, just earlier (see `rewrite_build.rs`'s
//! compile-fail note).

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{IntBinaryOp, IntUnaryOp};
use strider_ir_test_utils::RegisterSet;

use strider_opt::{
    BoxedRule, GraphEditFunctionExt, GraphRewriter, EditFunction, apply_rules_in_order, boxed_rule,
    rewrite_rule, rewrite_rule_runtime,
};
use strider_pattern::{
    Capture, MatchPat, Matcher, Match, Pattern, TemplatePat, add, any, call, int_const, is_skip,
    skip, sub, var,
};

// ── Minimal test-graph builder ───────────────────────────────────────────────

/// Test graph builder: wraps a `FunctionBuilder` with a single active entry
/// region pre-created, finalised via `ret_val`.
struct Tb {
    fb: strider_ir::FunctionBuilder,
}

impl Tb {
    fn empty() -> Self {
        let fb = RegisterSet::new()
            .build_fn_single_region()
            .expect("build_fn_single_region");
        Self { fb }
    }

    fn u64(&mut self, v: u64) -> ValueId {
        self.fb.build_int_const(v, ValueType::I64).unwrap()
    }

    fn add(&mut self, l: ValueId, r: ValueId) -> ValueId {
        self.fb
            .build_int_binary_operation(l, r, IntBinaryOp::Add, ValueType::I64)
            .expect("int_binary_operation")
    }

    /// Canonical lowered shape for `l - r`: `Add(l, Neg(r))`.
    /// `IntBinaryOp::Sub` is not a primitive; pcode-lift produces this shape.
    fn sub(&mut self, l: ValueId, r: ValueId) -> ValueId {
        let neg = self
            .fb
            .build_int_unary_operation(r, IntUnaryOp::Neg, ValueType::I64)
            .expect("int_unary_operation");
        self.add(l, neg)
    }

    /// Emits `Return(v)` in the current region and finalises the graph.
    fn ret_val(mut self, v: ValueId) -> strider_ir::Function {
        self.fb.build_return(Some(v), &[]).expect("build_return");
        self.fb.build().expect("FunctionBuilder::build (validator)")
    }
}

// ── Assertion helpers ────────────────────────────────────────────────────────

/// Returns the first node whose kind satisfies `pred`, panicking if none.
#[track_caller]
fn find_node<F: Fn(&NodeKind) -> bool>(function: &strider_ir::Function, pred: F) -> NodeId {
    function
        .walk()
        .find(|&n| pred(function.node_kind(n)))
        .expect("expected node kind not found in graph")
}

/// Asserts `pat` matches exactly `expected` times and returns the hits.
#[track_caller]
fn match_count(function: &strider_ir::Function, pat: Pattern, expected: usize) -> Vec<Match> {
    let hits = Matcher::try_new(function).unwrap().find_all(&pat).unwrap();
    assert_eq!(
        hits.len(),
        expected,
        "expected {expected} match(es), got {}",
        hits.len()
    );
    hits
}

// ── Fixtures: small graphs rewrite tests mutate ──────────────────────────────

/// `return(add(x, 0))` where `x` is `add(7, 1)` so the outer Add has a
/// non-const LHS — useful for testing `add(var(x), int_const(0))` rewrites.
fn graph_add_x_zero() -> strider_ir::Function {
    let mut t = Tb::empty();
    let c7 = t.u64(7);
    let c1 = t.u64(1);
    let x = t.add(c7, c1);
    let zero = t.u64(0);
    let sum = t.add(x, zero);
    t.ret_val(sum)
}

/// `return(sub(x, x))` — prime candidate for `sub(var(x), var(x)) → 0`.
fn graph_sub_x_x() -> strider_ir::Function {
    let mut t = Tb::empty();
    let c7 = t.u64(7);
    let c1 = t.u64(1);
    let x = t.add(c7, c1);
    let diff = t.sub(x, x);
    t.ret_val(diff)
}

/// `return(add(IntConst(a), IntConst(b)))` — prime candidate for
/// constant folding.
fn graph_add_const_const(a: u64, b: u64) -> strider_ir::Function {
    let mut t = Tb::empty();
    let ca = t.u64(a);
    let cb = t.u64(b);
    let s = t.add(ca, cb);
    t.ret_val(s)
}

// ── Assertion helpers local to this module ──────────────────────────────────

#[track_caller]
fn find_add(function: &strider_ir::Function) -> NodeId {
    find_node(function, |k| matches!(k, NodeKind::IntBinaryOp(IntBinaryOp::Add)))
}

/// Locate the lowered-`Sub` Add — `Add(_, Neg(_))` — distinguishing it
/// from any plain `Add(a, b)` in the fixture by its `Neg`-producing
/// second operand (lift lowers `IntSub(a, b)` to `Add(a, Neg(b))`).
#[track_caller]
fn find_sub(function: &strider_ir::Function) -> NodeId {
    function
        .walk()
        .find(|&n| {
            matches!(function.node_kind(n), NodeKind::IntBinaryOp(IntBinaryOp::Add))
                && function
                    .node_inputs(n)
                    .into_iter()
                    .map(|inp| function.producer(inp))
                    .any(|src| {
                        matches!(function.node_kind(src), NodeKind::IntUnaryOp(IntUnaryOp::Neg))
                    })
        })
        .expect("fixture must contain a lowered Sub: Add(_, Neg(_))")
}

/// Returns the `NodeKind` of the node producing the Return's data input.
fn return_data_input_kind(function: &strider_ir::Function) -> NodeKind {
    let ret = find_node(function, |k| matches!(k, NodeKind::Return));
    let inputs: Vec<ValueId> = function.node_inputs(ret).into_iter().collect();
    // Return inputs: [ctrl(0), mem(1), retval0(2), ...].
    let data_value = inputs[2];
    *function.kind_of_value(data_value)
}

/// Helper: run rule on every node, OR-ing results.
fn fire_anywhere<F>(function: &mut strider_ir::Function, rule: F) -> bool
where
    F: Fn(&mut EditFunction<'_>, NodeId) -> strider_pattern::Result<Option<ValueId>>,
{
    let nodes: Vec<NodeId> = function.walk().collect();
    function
        .with_rewrite_ctx(|ctx| {
            let mut any = false;
            for n in nodes {
                if rule(ctx, n)?.is_some() {
                    any = true;
                }
            }
            Ok(any)
        })
        .expect("test fixture is built")
}

// ── Basic firing ─────────────────────────────────────────────────────────────

#[test]
fn identity_rule_redirects_consumers_and_returns_true() {
    let mut function = graph_add_x_zero();
    let x = Capture::new();
    let rule = rewrite_rule(add(var(x), int_const(0u128)), var(x));

    let fired = fire_anywhere(&mut function, rule);
    assert!(fired, "rule should have fired on the outer Add");

    // After the rewrite the Return consumes the inner `add(7, 1)` directly.
    let kind = return_data_input_kind(&function);
    assert!(matches!(kind, NodeKind::IntBinaryOp(IntBinaryOp::Add)));
}

#[test]
fn rule_returns_false_when_lhs_does_not_match() {
    let mut function = graph_add_const_const(5, 3);
    let x = Capture::new();
    let rule = rewrite_rule(sub(var(x), var(x)), int_const(0u128));
    let add_node = find_add(&function);
    let fired = function
        .with_rewrite_ctx(|ctx| rule(ctx, add_node))
        .expect("test fixture is built")
        .is_some();
    assert!(!fired);

    assert!(matches!(
        return_data_input_kind(&function),
        NodeKind::IntBinaryOp(IntBinaryOp::Add)
    ));
}

#[test]
fn sub_x_x_to_zero_rule() {
    let mut function = graph_sub_x_x();
    let x = Capture::new();
    let rule = rewrite_rule(sub(var(x), var(x)), int_const(0u128));

    let sub_node = find_sub(&function);
    let fired = function
        .with_rewrite_ctx(|ctx| rule(ctx, sub_node))
        .expect("test fixture is built")
        .is_some();
    assert!(fired);

    let kind = return_data_input_kind(&function);
    assert!(matches!(kind, NodeKind::IntConst(0)));
}

// ── Error paths: multi-value-output LHS root ────────────────────────────────

/// Pin the documented `node_outputs_exact::<1>` constraint: rewriting on
/// a multi-output node (a `Call` whose outputs are
/// `[Control, Memory, ret-val0...]`) must surface an Err rather than
/// a silent rewire-of-the-wrong-slot.  Expressed with the runtime rule
/// variant since `call()` is a control builder, not a `MatchPat` LHS.
#[test]
fn rewrite_rule_on_call_root_returns_err() {
    use strider_ir_test_utils::SENTINEL_LIFT_ADDR;
    // `build_call` reads the stack pointer through the variable table and
    // errors if it is absent (no SP minting), so track one.
    let sp = strider_ir_test_utils::reg_vn(0x7000, 8);
    let mut fb = RegisterSet::new()
        .tracked(sp)
        .stack_vn(sp)
        .build_fn_single_region()
        .unwrap();
    fb.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let tgt = fb.build_int_const(0x1234u64, ValueType::I64).unwrap();
    fb.build_call(tgt, None).unwrap();
    fb.build_return(None, &[]).unwrap();
    fb.set_lift_addr(None);
    let mut function = fb.build().unwrap();

    let rule = rewrite_rule_runtime(call().build(), int_const(0u128).into_template()).unwrap();
    let call_node = function
        .walk()
        .find(|n| matches!(function.node_kind(*n), NodeKind::Call))
        .expect("Call node");
    let err = function
        .with_rewrite_ctx(|ctx| match rule(ctx, call_node) {
            Ok(_) => panic!("multi-output root must error"),
            Err(e) => Ok(e),
        })
        .expect("test fixture is built");
    let dbg = format!("{err:?}");
    assert!(
        dbg.contains("output") || dbg.contains("exactly"),
        "expected node_outputs_exact failure, got {err:?}"
    );
}

// ── boxed_rule heterogeneous composition ───────────────────────────────────

#[test]
fn boxed_rule_allows_heterogeneous_vec() {
    let x = Capture::new();
    let y = Capture::new();
    let rules: Vec<BoxedRule> = vec![
        boxed_rule(rewrite_rule(add(var(x), int_const(0u128)), var(x))),
        boxed_rule(rewrite_rule(sub(var(y), var(y)), int_const(0u128))),
    ];
    assert_eq!(rules.len(), 2);
}

// ── replace_all_uses zero-user case ───────────────────────────────────────

#[test]
fn rewrite_returns_false_when_no_consumer() {
    let mut t = Tb::empty();
    let _dead_a = t.u64(5);
    let _dead_b = t.u64(3);
    let other = t.u64(7);
    let mut function = t.ret_val(other);

    let x = Capture::new();
    let rule = rewrite_rule(add(var(x), int_const(0u128)), var(x));
    let fired = fire_anywhere(&mut function, rule);
    assert!(!fired);
}

// ── Smoke: run rewrite via Matcher + rule, compare before/after ─────────────

#[test]
fn pattern_match_before_and_after_rewrite() {
    let mut function = graph_add_x_zero();
    match_count(&function, add(any(), int_const(0u128)).into_pattern(), 1);

    let x = Capture::new();
    let rule = rewrite_rule(add(var(x), int_const(0u128)), var(x));
    fire_anywhere(&mut function, rule);

    let ret_kind = return_data_input_kind(&function);
    assert!(matches!(ret_kind, NodeKind::IntBinaryOp(IntBinaryOp::Add)));
}

// ── GraphRewriter::apply_count / apply_rules_count facade ─────────────────────

/// Counts reachable Add nodes.
fn count_adds(function: &strider_ir::Function) -> usize {
    function
        .walk()
        .filter(|nid| matches!(function.node_kind(*nid), NodeKind::IntBinaryOp(IntBinaryOp::Add)))
        .count()
}

/// `apply_count` returns 0 on a graph with no candidate Add node.
#[test]
fn apply_count_with_no_match_returns_zero() {
    let mut t = Tb::empty();
    let v = t.u64(7);
    let mut function = t.ret_val(v);
    let x = Capture::new();
    let rule = rewrite_rule(add(var(x), int_const(0u128)), var(x));
    let n = GraphRewriter::apply_count(&mut function, rule).unwrap();
    assert_eq!(n, 0, "rule must not fire on a graph without any Add node");
}

/// `apply_count` returns exactly one application on `Add(7, 0)`, and the
/// rewritten Add becomes unreachable afterwards.
#[test]
fn apply_count_with_one_match_returns_one() {
    let mut t = Tb::empty();
    let a = t.u64(7);
    let z = t.u64(0);
    let sum = t.add(a, z);
    let mut function = t.ret_val(sum);
    assert_eq!(count_adds(&function), 1, "fixture must have exactly one Add");

    let x = Capture::new();
    let rule = rewrite_rule(add(var(x), int_const(0u128)), var(x));
    let n = GraphRewriter::apply_count(&mut function, rule).unwrap();
    assert_eq!(n, 1, "exactly one application expected");
    assert_eq!(
        count_adds(&function),
        0,
        "post-rewrite reachable graph must have zero Add nodes"
    );
}

/// `apply_rules_count` walks every reachable node once per call; driven
/// to a fixed point, the two inner identity-Adds collapse while the
/// outer lowered-Sub Add stays.
#[test]
fn apply_rules_count_round_robin_reaches_fixed_point() {
    let mut t = Tb::empty();
    let ac = t.u64(11);
    let bc = t.u64(13);
    let z = t.u64(0);
    let lhs = t.add(ac, z);
    let rhs = t.add(bc, z);
    let diff = t.sub(lhs, rhs); // Tb::sub lowers to Add(lhs, Neg(rhs)).
    let mut function = t.ret_val(diff);
    // Three Adds: two inner identity-Adds + the outer Sub-lowering Add.
    assert_eq!(count_adds(&function), 3);

    let y = Capture::new();
    let z_cap = Capture::new();
    let rules: Vec<BoxedRule> = vec![
        boxed_rule(rewrite_rule(add(var(y), int_const(0u128)), var(y))),
        boxed_rule(rewrite_rule(sub(var(z_cap), var(z_cap)), int_const(0u128))),
    ];

    let mut total: usize = 0;
    for _ in 0..16 {
        let n = GraphRewriter::apply_rules_count(&mut function, &rules).unwrap();
        total += n;
        if n == 0 {
            break;
        }
    }
    assert!(total >= 2, "rule must fire at least twice on the two inner Adds");
    assert_eq!(
        count_adds(&function),
        1,
        "the two inner identity Adds collapse; the outer Sub-Add stays"
    );
}

/// After a count-driven rewrite the whole-graph validator still passes —
/// pins use-list bidirectional integrity through `replace_all_uses`.
#[test]
fn apply_count_preserves_use_list_integrity() {
    let mut t = Tb::empty();
    let a = t.u64(7);
    let z = t.u64(0);
    let sum = t.add(a, z);
    let mut function = t.ret_val(sum);
    let x = Capture::new();
    let rule = rewrite_rule(add(var(x), int_const(0u128)), var(x));
    GraphRewriter::apply_count(&mut function, rule).unwrap();
    strider_ir::validate::validate(&function, function.entry().unwrap())
        .expect("validate must pass after rewrite");
}

// ── RewriteSkip sentinel public contract ─────────────────────────────────────

/// `skip()` produces an error that `is_skip` recognises; an unrelated
/// error does not.  The `rewrite_rule` interpreter consults `is_skip` on
/// every `Err` returned during instantiation to convert a deliberate
/// opt-out into "no change".
#[test]
fn skip_sentinel_round_trips_through_is_skip() {
    let e = skip();
    assert!(is_skip(&e));
    let e_other = anyhow::anyhow!("not a skip");
    assert!(!is_skip(&e_other));
}

// ── asm-fingerprint absorption into the rewritten root ───────────────────────

/// After a rewrite, the freshly-built producer absorbs the rewritten
/// root's asm-fingerprint (superset semantics).
#[test]
fn rewrite_absorbs_source_fingerprint_into_rewritten_root() {
    let mut function = graph_add_x_zero();
    let x = Capture::new();
    // Locate the outer `Add(_, 0)` root (not the inner `Add(7, 1)`).
    let add_node = {
        let m = Matcher::try_new(&function).unwrap();
        let pat = add(var(x), int_const(0u128)).into_pattern();
        let hits = m.find_all(&pat).unwrap();
        assert_eq!(hits.len(), 1);
        hits[0].root()
    };
    const SOURCE_ADDR: u64 = 0xFEED_CAFE_0000_1111;
    function.set_asm_fingerprint(add_node, vec![SOURCE_ADDR]);
    assert!(function.asm_fingerprint(add_node).contains(&SOURCE_ADDR));

    let rule = rewrite_rule(add(var(x), int_const(0u128)), var(x));
    let mut ctx = EditFunction::try_for_built(&mut function).unwrap();
    let changed = rule(&mut ctx, add_node).unwrap().is_some();
    assert!(changed);

    // The Return now reads the redirected producer; its fingerprint must
    // include the source Add's address.
    let kind_producer = {
        let ret = find_node(&function, |k| matches!(k, NodeKind::Return));
        let v = function.node_inputs(ret)[2];
        function.producer(v)
    };
    let fp = function.asm_fingerprint(kind_producer);
    assert!(
        fp.contains(&SOURCE_ADDR),
        "rewritten producer must absorb source fingerprint, got {fp:?}"
    );
}

// ── apply_rules_in_order composition ─────────────────────────────────────────

/// `apply_rules_in_order` runs each rule in turn at a node, OR-ing the
/// results: only the second rule fires on the fixture, yet the composed
/// result is `true`.
#[test]
fn apply_rules_in_order_or_composes_results() {
    let mut function = graph_add_x_zero();
    let x = Capture::new();
    let y = Capture::new();
    // The outer `Add(_, 0)` is the rule's target.
    let add_node = {
        let m = Matcher::try_new(&function).unwrap();
        let pat = add(var(y), int_const(0u128)).into_pattern();
        let hits = m.find_all(&pat).unwrap();
        assert_eq!(hits.len(), 1);
        hits[0].root()
    };
    let rules: Vec<BoxedRule> = vec![
        // First rule looks for Add(_, IntConst(7)) — no match.
        boxed_rule(rewrite_rule(add(var(x), int_const(7u128)), var(x))),
        // Second rule matches the actual fixture (Add(_, 0)).
        boxed_rule(rewrite_rule(add(var(y), int_const(0u128)), var(y))),
    ];
    let mut ctx = EditFunction::try_for_built(&mut function).unwrap();
    let fired = apply_rules_in_order(&rules)(&mut ctx, add_node).unwrap().is_some();
    assert!(fired, "second rule must have fired");
}
