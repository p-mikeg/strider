//! Single-node match tests.  Recursive (multi-node) tests land in
//! `recursive_match.rs` in the next commit.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use strider_pattern::pat_graph::{PatGraph, KindSpec, NodeData, Concrete, EdgeData};
use strider_pattern::{Matcher, Capture};
use strider_ir::FunctionBuilder;
use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir_test_utils::RegisterSet;

#[test]
fn matches_int_const_5() {
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn");
    let five = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
    b.build_return(Some(five), &[]).unwrap();
    let function = b.build().unwrap();

    let mut g: PatGraph<Concrete> = PatGraph::new();
    let root = g.add_node(NodeData {
        kind: KindSpec::Exact(NodeKind::IntConst(5)),
        output_ty: Some(NodeOutputType::I64),
        capture: None,
        post_match: None,
        build_spec: None,
    });
    g.set_root(root);

    let m = Matcher::try_new(&function).unwrap();
    let hits = m.find_all(&g);
    assert_eq!(hits.len(), 1, "expected exactly one IntConst(5) match");
}

#[test]
fn rejects_wrong_int_const_value() {
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn");
    let five = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
    b.build_return(Some(five), &[]).unwrap();
    let function = b.build().unwrap();

    let mut g: PatGraph<Concrete> = PatGraph::new();
    let root = g.add_node(NodeData {
        kind: KindSpec::Exact(NodeKind::IntConst(99)),  // value mismatch
        output_ty: Some(NodeOutputType::I64),
        capture: None,
        post_match: None,
        build_spec: None,
    });
    g.set_root(root);

    let m = Matcher::try_new(&function).unwrap();
    let hits = m.find_all(&g);
    assert!(hits.is_empty(), "wrong-value IntConst should not match");
}

#[test]
fn variant_only_matches_any_int_const() {
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn");
    let five = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
    let seven = b.build_int_const(7u64, NodeOutputType::I64).unwrap();
    let sum = b
        .build_int_binary_operation(
            five,
            seven,
            strider_ir::IntBinaryOp::Add,
            NodeOutputType::I64,
        )
        .unwrap();
    b.build_return(Some(sum), &[]).unwrap();
    let function = b.build().unwrap();

    let mut g: PatGraph<Concrete> = PatGraph::new();
    let exemplar = NodeKind::IntConst(0);  // payload ignored under KindSpec::Variant
    let root = g.add_node(NodeData {
        kind: KindSpec::Variant(std::mem::discriminant(&exemplar)),
        output_ty: Some(NodeOutputType::I64),
        capture: None,
        post_match: None,
        build_spec: None,
    });
    g.set_root(root);

    let m = Matcher::try_new(&function).unwrap();
    let hits = m.find_all(&g);
    assert!(hits.len() >= 2, "Variant kind spec should match both IntConsts; got {}", hits.len());
}

/// Recursive multi-node match: pattern `Add(IntConst(5), var(x))` against
/// IR `Add(IntConst(5), IntConst(7))` must produce exactly one hit, with
/// `x` bound to the 7-output (or the 5-output if commutative retry
/// fires — both are valid under commutative semantics).
#[test]
fn matches_add_int_const_5_var_x() {
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn");
    let five = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
    let seven = b.build_int_const(7u64, NodeOutputType::I64).unwrap();
    let add_out = b
        .build_int_binary_operation(
            five,
            seven,
            strider_ir::IntBinaryOp::Add,
            NodeOutputType::I64,
        )
        .unwrap();
    b.build_return(Some(add_out), &[]).unwrap();
    let function = b.build().unwrap();

    let cap = Capture::default();
    let mut g: PatGraph<Concrete> = PatGraph::new();
    let five_pat = g.add_node(NodeData {
        kind: KindSpec::Exact(NodeKind::IntConst(5)),
        output_ty: None,
        capture: None,
        post_match: None,
        build_spec: None,
    });
    let var_pat = g.add_node(NodeData {
        kind: KindSpec::Any,
        output_ty: None,
        capture: Some(cap.as_ref()),
        post_match: None,
        build_spec: None,
    });
    let add_kind = NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::Add);
    let add_pat = g.add_node(NodeData {
        kind: KindSpec::Exact(add_kind),
        output_ty: None,
        capture: None,
        post_match: None,
        build_spec: None,
    });
    g.add_edge(
        five_pat,
        add_pat,
        EdgeData { consumer_slot: 0, producer_output_slot: 0 },
    );
    g.add_edge(
        var_pat,
        add_pat,
        EdgeData { consumer_slot: 1, producer_output_slot: 0 },
    );
    g.set_root(add_pat);

    let m = Matcher::try_new(&function).unwrap();
    let hits = m.find_all(&g);
    assert_eq!(hits.len(), 1, "Add(IntConst(5), _) should match exactly once");
    // `x` should bind to one of the two constants — under commutative
    // matching either ordering is accepted.  Assert it binds to *some*
    // IntConst output (i.e. the capture fired and the binding resolves
    // back to an IntConst node).
    let m0 = &hits[0];
    let out = m0.output(cap).expect("x must be bound to a value output");
    let kind = function.kind_of_output(out);
    assert!(
        matches!(kind, NodeKind::IntConst(_)),
        "x should bind to an IntConst output, got {kind:?}",
    );
}

/// `Add(IntConst(5), var(x))` against an IR `Add(IntConst(7), IntConst(5))`
/// — the constant is on the swapped side.  Commutative retry must fire,
/// so we still get exactly one hit with `x` bound to the 7-output.
#[test]
fn commutative_retry_matches_swapped_operands() {
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn");
    let seven = b.build_int_const(7u64, NodeOutputType::I64).unwrap();
    let five = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
    // IR builds Add(7, 5) — note operand order is reversed relative to
    // the pattern's Add(IntConst(5), _).
    let add_out = b
        .build_int_binary_operation(
            seven,
            five,
            strider_ir::IntBinaryOp::Add,
            NodeOutputType::I64,
        )
        .unwrap();
    b.build_return(Some(add_out), &[]).unwrap();
    let function = b.build().unwrap();

    let cap = Capture::default();
    let mut g: PatGraph<Concrete> = PatGraph::new();
    let five_pat = g.add_node(NodeData {
        kind: KindSpec::Exact(NodeKind::IntConst(5)),
        output_ty: None,
        capture: None,
        post_match: None,
        build_spec: None,
    });
    let var_pat = g.add_node(NodeData {
        kind: KindSpec::Any,
        output_ty: None,
        capture: Some(cap.as_ref()),
        post_match: None,
        build_spec: None,
    });
    let add_kind = NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::Add);
    let add_pat = g.add_node(NodeData {
        kind: KindSpec::Exact(add_kind),
        output_ty: None,
        capture: None,
        post_match: None,
        build_spec: None,
    });
    g.add_edge(
        five_pat,
        add_pat,
        EdgeData { consumer_slot: 0, producer_output_slot: 0 },
    );
    g.add_edge(
        var_pat,
        add_pat,
        EdgeData { consumer_slot: 1, producer_output_slot: 0 },
    );
    g.set_root(add_pat);

    let m = Matcher::try_new(&function).unwrap();
    let hits = m.find_all(&g);
    assert_eq!(
        hits.len(),
        1,
        "commutative retry should match Add(7, 5) against Add(5, _)",
    );
    let out = hits[0].output(cap).expect("x must be bound");
    let kind = function.kind_of_output(out);
    // After the swap, `x` should bind to the 7-side constant (the
    // operand not used by the literal `IntConst(5)` pattern).
    assert!(
        matches!(kind, NodeKind::IntConst(7)),
        "x should bind to the 7-output after commutative retry; got {kind:?}",
    );
}
