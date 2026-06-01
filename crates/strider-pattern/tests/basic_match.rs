//! Single-node match tests.  Recursive (multi-node) tests land in
//! `recursive_match.rs` in the next commit.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use strider_pattern::pat_graph::{PatGraph, KindSpec, NodeData, Concrete};
use strider_pattern::Matcher;
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
