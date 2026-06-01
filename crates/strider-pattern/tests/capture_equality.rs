//! Capture-equality semantics: when a single `Capture` appears in two
//! pattern slots, the matcher must bind both to the **same** IR output
//! — and reject the match when the two slots would bind to different
//! outputs.  The contract is enforced by `Bindings::bind_capture`'s
//! conflict path; this file pins the end-to-end matcher behaviour.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use strider_pattern::pat_graph::{Concrete, EdgeData, KindSpec, NodeData, PatGraph};
use strider_pattern::{Capture, Matcher};
use strider_ir::FunctionBuilder;
use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir_test_utils::RegisterSet;

/// Pattern `Add(var(x), var(x))` against IR `Add(N, N)` where both
/// operands resolve to the same producer (one `IntConst(5)` reused on
/// both sides — the IR dedups constants by value, so feeding it twice
/// yields the same `NodeOutputId`).  The match must succeed exactly
/// once and `x` must bind to that producer's output.
#[test]
fn add_x_x_matches_when_both_inputs_are_same_node() {
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn");
    let five = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
    // IR dedup makes both Add operands the same NodeOutputId.
    let add_out = b
        .build_int_binary_operation(
            five,
            five,
            strider_ir::IntBinaryOp::Add,
            NodeOutputType::I64,
        )
        .unwrap();
    b.build_return(Some(add_out), &[]).unwrap();
    let function = b.build().unwrap();

    let x = Capture::default();
    let mut g: PatGraph<Concrete> = PatGraph::new();
    let var_a = g.add_node(NodeData {
        kind: KindSpec::Any,
        output_ty: None,
        capture: Some(x),
        post_match: None,
        template_spec: None,
            force_ordered: false,
    });
    let var_b = g.add_node(NodeData {
        kind: KindSpec::Any,
        output_ty: None,
        capture: Some(x),
        post_match: None,
        template_spec: None,
            force_ordered: false,
    });
    let add_pat = g.add_node(NodeData {
        kind: KindSpec::Exact(NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::Add)),
        output_ty: None,
        capture: None,
        post_match: None,
        template_spec: None,
            force_ordered: false,
    });
    g.add_edge(
        var_a,
        add_pat,
        EdgeData { consumer_slot: 0, producer_output_slot: 0 },
    );
    g.add_edge(
        var_b,
        add_pat,
        EdgeData { consumer_slot: 1, producer_output_slot: 0 },
    );
    g.set_root(add_pat);

    let m = Matcher::try_new(&function).unwrap();
    let hits = m.find_all(&g);
    assert_eq!(hits.len(), 1, "Add(x, x) against Add(N, N) should match");
    let bound = hits[0].output(x).expect("x must be bound");
    assert_eq!(bound, five, "x should bind to the shared IntConst(5) output");
}

/// Pattern `Add(var(x), var(x))` against IR `Add(N1, N2)` where the two
/// producers differ.  The shared capture forces both pattern slots to
/// bind to the same IR output; `Bindings::bind_capture` rejects the
/// second bind with a conflicting `Binding`, so no match should be
/// reported — even after the commutative-retry path (swapping the
/// operands keeps the operands distinct).
#[test]
fn add_x_x_rejects_when_inputs_differ() {
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn");
    let three = b.build_int_const(3u64, NodeOutputType::I64).unwrap();
    let seven = b.build_int_const(7u64, NodeOutputType::I64).unwrap();
    let add_out = b
        .build_int_binary_operation(
            three,
            seven,
            strider_ir::IntBinaryOp::Add,
            NodeOutputType::I64,
        )
        .unwrap();
    b.build_return(Some(add_out), &[]).unwrap();
    let function = b.build().unwrap();

    let x = Capture::default();
    let mut g: PatGraph<Concrete> = PatGraph::new();
    let var_a = g.add_node(NodeData {
        kind: KindSpec::Any,
        output_ty: None,
        capture: Some(x),
        post_match: None,
        template_spec: None,
            force_ordered: false,
    });
    let var_b = g.add_node(NodeData {
        kind: KindSpec::Any,
        output_ty: None,
        capture: Some(x),
        post_match: None,
        template_spec: None,
            force_ordered: false,
    });
    let add_pat = g.add_node(NodeData {
        kind: KindSpec::Exact(NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::Add)),
        output_ty: None,
        capture: None,
        post_match: None,
        template_spec: None,
            force_ordered: false,
    });
    g.add_edge(
        var_a,
        add_pat,
        EdgeData { consumer_slot: 0, producer_output_slot: 0 },
    );
    g.add_edge(
        var_b,
        add_pat,
        EdgeData { consumer_slot: 1, producer_output_slot: 0 },
    );
    g.set_root(add_pat);

    let m = Matcher::try_new(&function).unwrap();
    let hits = m.find_all(&g);
    assert!(
        hits.is_empty(),
        "Add(x, x) against Add(N1, N2) where N1 != N2 should not match; got {} hits",
        hits.len(),
    );
}
