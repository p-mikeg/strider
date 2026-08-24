//! Rewrite rules over a built `Function`: LHS matching at a root, consumer
//! redirection, and `apply_rules_count` driving rules across the graph.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{IRBuilderExt, IRViewer, IRWalker, IntBinaryOp, IntUnaryOp};
use strider_ir_test_utils::RegisterSet;

use strider_opt::{BoxedRule, EditFunction, apply_rules_count, rewrite_rule, rewrite_rule_runtime};
use strider_pattern::{
    Capture, Match, MatchPat, Matcher, Pattern, TemplatePat, anything, call, int_add, int_const,
    int_sub, is_skip, skip, var,
};

/// Wraps a `FunctionBuilder` with a single entry region pre-created,
/// finalised via `ret_val`.
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

    /// `IntBinaryOp::Sub` is not a primitive; pcode-lift lowers `l - r` to
    /// `Add(l, Neg(r))`.
    fn sub(&mut self, l: ValueId, r: ValueId) -> ValueId {
        let neg = self
            .fb
            .build_int_unary_operation(r, IntUnaryOp::Neg, ValueType::I64)
            .expect("int_unary_operation");
        self.add(l, neg)
    }

    fn ret_val(mut self, v: ValueId) -> strider_ir::Function {
        self.fb.build_return(Some(v), &[]).expect("build_return");
        self.fb.build().expect("FunctionBuilder::build (validator)")
    }
}

#[track_caller]
fn find_node<F: Fn(&NodeKind) -> bool>(function: &strider_ir::Function, pred: F) -> NodeId {
    function
        .walk()
        .find(|&n| pred(function.node_kind(n)))
        .expect("expected node kind not found in graph")
}

#[track_caller]
fn match_count(function: &strider_ir::Function, pat: Pattern, expected: usize) -> Vec<Match> {
    let hits = Matcher::new(function).find_all(&pat).unwrap();
    assert_eq!(
        hits.len(),
        expected,
        "expected {expected} match(es), got {}",
        hits.len()
    );
    hits
}

/// `return(add(x, 0))` where `x` is `add(7, 1)`, so the outer Add has a
/// non-const LHS.
fn graph_add_x_zero() -> strider_ir::Function {
    let mut t = Tb::empty();
    let c7 = t.u64(7);
    let c1 = t.u64(1);
    let x = t.add(c7, c1);
    let zero = t.u64(0);
    let sum = t.add(x, zero);
    t.ret_val(sum)
}

/// `return(sub(x, x))`.
fn graph_sub_x_x() -> strider_ir::Function {
    let mut t = Tb::empty();
    let c7 = t.u64(7);
    let c1 = t.u64(1);
    let x = t.add(c7, c1);
    let diff = t.sub(x, x);
    t.ret_val(diff)
}

/// `return(add(IntConst(a), IntConst(b)))`.
fn graph_add_const_const(a: u64, b: u64) -> strider_ir::Function {
    let mut t = Tb::empty();
    let ca = t.u64(a);
    let cb = t.u64(b);
    let s = t.add(ca, cb);
    t.ret_val(s)
}

#[track_caller]
fn find_add(function: &strider_ir::Function) -> NodeId {
    find_node(function, |k| {
        matches!(k, NodeKind::IntBinaryOp(IntBinaryOp::Add))
    })
}

/// Finds the lowered `Sub`, `Add(_, Neg(_))`, distinguishing it from a plain
/// `Add(a, b)` by its `Neg`-producing operand.
#[track_caller]
fn find_sub(function: &strider_ir::Function) -> NodeId {
    function
        .walk()
        .find(|&n| {
            matches!(
                function.node_kind(n),
                NodeKind::IntBinaryOp(IntBinaryOp::Add)
            ) && function
                .node_inputs(n)
                .into_iter()
                .map(|inp| function.producer(inp))
                .any(|src| {
                    matches!(
                        function.node_kind(src),
                        NodeKind::IntUnaryOp(IntUnaryOp::Neg)
                    )
                })
        })
        .expect("fixture must contain a lowered Sub: Add(_, Neg(_))")
}

fn return_data_input_kind(function: &strider_ir::Function) -> NodeKind {
    let ret = find_node(function, |k| matches!(k, NodeKind::Return));
    let inputs: Vec<ValueId> = function.node_inputs(ret).into_iter().collect();
    // Return inputs: [ctrl, mem, retval0, ...].
    let data_value = inputs[2];
    *function.kind_of_value(data_value)
}

fn fire_anywhere<F>(function: &mut strider_ir::Function, rule: F) -> bool
where
    F: for<'g> Fn(&mut EditFunction<'g>, NodeId) -> strider_pattern::Result<Option<ValueId>>,
{
    let mut ctx = EditFunction::new(function);
    apply_rules_count(&mut ctx, std::slice::from_ref(&rule)).expect("apply must not error") > 0
}

#[test]
fn identity_rule_redirects_consumers_and_returns_true() {
    let mut function = graph_add_x_zero();
    let x = Capture::new();
    let rule = rewrite_rule(int_add(var(x), int_const(0u128)), var(x));

    let fired = fire_anywhere(&mut function, rule);
    assert!(fired, "rule should have fired on the outer Add");

    // The Return now consumes the inner `add(7, 1)` directly.
    let kind = return_data_input_kind(&function);
    assert!(matches!(kind, NodeKind::IntBinaryOp(IntBinaryOp::Add)));
}

#[test]
fn rule_returns_false_when_lhs_does_not_match() {
    let mut function = graph_add_const_const(5, 3);
    let x = Capture::new();
    let rule = rewrite_rule(int_sub(var(x), var(x)), int_const(0u128));
    let add_node = find_add(&function);
    let fired = {
        let mut ctx = EditFunction::new(&mut function);
        rule(&mut ctx, add_node).unwrap().is_some()
    };
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
    let rule = rewrite_rule(int_sub(var(x), var(x)), int_const(0u128));

    let sub_node = find_sub(&function);
    let fired = {
        let mut ctx = EditFunction::new(&mut function);
        rule(&mut ctx, sub_node).unwrap().is_some()
    };
    assert!(fired);

    {
        use strider_ir::IRViewer;
        let ret = find_node(&function, |k| matches!(k, NodeKind::Return));
        let data_value = function.node_inputs(ret)[2];
        assert!(
            matches!(function.kind_of_value(data_value), NodeKind::IntConst(_))
                && function.int_const_u128(data_value) == Some(0),
            "sub x x should fold to IntConst(0)"
        );
    }
}

/// Rewriting on a multi-output node (a `Call`, whose outputs are
/// `[Control, Memory, ret-val0...]`) must error rather than silently
/// rewire the wrong slot.
#[test]
fn rewrite_rule_on_call_root_returns_err() {
    use strider_ir_test_utils::SENTINEL_LIFT_ADDR;
    // `build_call` reads the stack pointer through the variable table and
    // errors if it is absent, so track one.
    let sp = strider_ir_test_utils::reg_vn(0x7000, 8);
    let mut fb = RegisterSet::new()
        .tracked(sp)
        .stack_vn(sp)
        .build_fn_single_region()
        .unwrap();
    fb.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let tgt = fb.build_int_const(0x1234u64, ValueType::I64).unwrap();
    fb.build_call_cc(tgt, None).unwrap();
    fb.build_return(None, &[]).unwrap();
    fb.set_lift_addr(None);
    let mut function = fb.build().unwrap();

    let rule = rewrite_rule_runtime(call().build(), int_const(0u128).into_template()).unwrap();
    let call_node = function
        .walk()
        .find(|n| matches!(function.node_kind(*n), NodeKind::Call))
        .expect("Call node");
    let err = {
        let mut ctx = EditFunction::new(&mut function);
        match rule(&mut ctx, call_node) {
            Ok(_) => panic!("multi-output root must error"),
            Err(e) => e,
        }
    };
    let dbg = format!("{err:?}");
    assert!(
        dbg.contains("output") || dbg.contains("exactly"),
        "expected node_outputs_exact failure, got {err:?}"
    );
}

/// Rules with structurally different shapes collect into one `Vec`.
#[test]
fn rewrite_rule_results_collect_into_heterogeneous_vec() {
    let x = Capture::new();
    let y = Capture::new();
    let rules: Vec<BoxedRule> = vec![
        rewrite_rule(int_add(var(x), int_const(0u128)), var(x)),
        rewrite_rule(int_sub(var(y), var(y)), int_const(0u128)),
    ];
    assert_eq!(rules.len(), 2);
}

#[test]
fn rewrite_returns_false_when_no_matching_node() {
    let mut t = Tb::empty();
    let _dead_a = t.u64(5);
    let _dead_b = t.u64(3);
    let other = t.u64(7);
    let mut function = t.ret_val(other);

    let x = Capture::new();
    let rule = rewrite_rule(int_add(var(x), int_const(0u128)), var(x));
    let fired = fire_anywhere(&mut function, rule);
    assert!(!fired);
}

#[test]
fn pattern_match_before_and_after_rewrite() {
    let mut function = graph_add_x_zero();
    match_count(
        &function,
        int_add(anything(), int_const(0u128)).into_pattern(),
        1,
    );

    let x = Capture::new();
    let rule = rewrite_rule(int_add(var(x), int_const(0u128)), var(x));
    fire_anywhere(&mut function, rule);

    let ret_kind = return_data_input_kind(&function);
    assert!(matches!(ret_kind, NodeKind::IntBinaryOp(IntBinaryOp::Add)));
}

fn count_adds(function: &strider_ir::Function) -> usize {
    function
        .walk()
        .filter(|nid| {
            matches!(
                function.node_kind(*nid),
                NodeKind::IntBinaryOp(IntBinaryOp::Add)
            )
        })
        .count()
}

#[test]
fn apply_count_with_no_match_returns_zero() {
    let mut t = Tb::empty();
    let v = t.u64(7);
    let mut function = t.ret_val(v);
    let x = Capture::new();
    let rule = rewrite_rule(int_add(var(x), int_const(0u128)), var(x));
    let mut ctx = EditFunction::new(&mut function);
    let n = apply_rules_count(&mut ctx, std::slice::from_ref(&rule)).unwrap();
    assert_eq!(n, 0, "rule must not fire on a graph without any Add node");
}

/// One application on `Add(7, 0)`, after which the rewritten Add is
/// unreachable.
#[test]
fn apply_count_with_one_match_returns_one() {
    let mut t = Tb::empty();
    let a = t.u64(7);
    let z = t.u64(0);
    let sum = t.add(a, z);
    let mut function = t.ret_val(sum);
    assert_eq!(
        count_adds(&function),
        1,
        "fixture must have exactly one Add"
    );

    let x = Capture::new();
    let rule = rewrite_rule(int_add(var(x), int_const(0u128)), var(x));
    let n = {
        let mut ctx = EditFunction::new(&mut function);
        apply_rules_count(&mut ctx, std::slice::from_ref(&rule)).unwrap()
    };
    assert_eq!(n, 1, "exactly one application expected");
    assert_eq!(
        count_adds(&function),
        0,
        "post-rewrite reachable graph must have zero Add nodes"
    );
}

/// `apply_rules_count` walks every reachable node once per call. Driven to a
/// fixed point, the two inner identity-Adds collapse and the outer
/// lowered-Sub Add stays.
#[test]
fn apply_rules_count_round_robin_reaches_fixed_point() {
    let mut t = Tb::empty();
    let ac = t.u64(11);
    let bc = t.u64(13);
    let z = t.u64(0);
    let lhs = t.add(ac, z);
    let rhs = t.add(bc, z);
    let diff = t.sub(lhs, rhs); // lowers to Add(lhs, Neg(rhs))
    let mut function = t.ret_val(diff);
    // Two inner identity-Adds plus the outer Sub-lowering Add.
    assert_eq!(count_adds(&function), 3);

    let y = Capture::new();
    let z_cap = Capture::new();
    let rules: Vec<BoxedRule> = vec![
        rewrite_rule(int_add(var(y), int_const(0u128)), var(y)),
        rewrite_rule(int_sub(var(z_cap), var(z_cap)), int_const(0u128)),
    ];

    let mut total: usize = 0;
    for _ in 0..16 {
        let n = {
            let mut ctx = EditFunction::new(&mut function);
            apply_rules_count(&mut ctx, &rules).unwrap()
        };
        total += n;
        if n == 0 {
            break;
        }
    }
    assert!(
        total >= 2,
        "rule must fire at least twice on the two inner Adds"
    );
    assert_eq!(
        count_adds(&function),
        1,
        "the two inner identity Adds collapse; the outer Sub-Add stays"
    );
}

/// The whole-graph validator still passes after a count-driven rewrite.
#[test]
fn apply_count_preserves_use_list_integrity() {
    let mut t = Tb::empty();
    let a = t.u64(7);
    let z = t.u64(0);
    let sum = t.add(a, z);
    let mut function = t.ret_val(sum);
    let x = Capture::new();
    let rule = rewrite_rule(int_add(var(x), int_const(0u128)), var(x));
    {
        let mut ctx = EditFunction::new(&mut function);
        apply_rules_count(&mut ctx, std::slice::from_ref(&rule)).unwrap();
    }
    strider_ir::validate::validate(&function).expect("validate must pass after rewrite");
}

/// An unrelated instantiation error must not be mistaken for a `skip()`
/// opt-out.
#[test]
fn skip_sentinel_round_trips_through_is_skip() {
    let e = skip();
    assert!(is_skip(&e));
    let e_other = anyhow::anyhow!("not a skip");
    assert!(!is_skip(&e_other));
}

/// The freshly-built producer absorbs the rewritten root's asm-fingerprint.
#[test]
fn rewrite_absorbs_source_fingerprint_into_rewritten_root() {
    let mut function = graph_add_x_zero();
    let x = Capture::new();
    // Locate the outer `Add(_, 0)` root (not the inner `Add(7, 1)`).
    let add_node = {
        let m = Matcher::new(&function);
        let pat = int_add(var(x), int_const(0u128)).into_pattern();
        let hits = m.find_all(&pat).unwrap();
        assert_eq!(hits.len(), 1);
        hits[0].root()
    };
    const SOURCE_ADDR: u64 = 0xFEED_CAFE_0000_1111;
    function
        .side_tables_mut()
        .extend_asm_fingerprint(add_node, &[SOURCE_ADDR]);
    assert!(
        function
            .side_tables()
            .asm_fingerprint(add_node)
            .contains(&SOURCE_ADDR)
    );

    let rule = rewrite_rule(int_add(var(x), int_const(0u128)), var(x));
    let mut ctx = EditFunction::new(&mut function);
    let changed = rule(&mut ctx, add_node).unwrap().is_some();
    assert!(changed);

    // The Return now reads the redirected producer; its fingerprint must
    // include the source Add's address.
    let kind_producer = {
        let ret = find_node(&function, |k| matches!(k, NodeKind::Return));
        let v = function.node_inputs(ret)[2];
        function.producer(v)
    };
    let fp = function.side_tables().asm_fingerprint(kind_producer);
    assert!(
        fp.contains(&SOURCE_ADDR),
        "rewritten producer must absorb source fingerprint, got {fp:?}"
    );
}
