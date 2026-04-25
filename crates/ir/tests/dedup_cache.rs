//! Black-box: cacheable kinds dedup; non-cacheable kinds never do; mutating
//! a cacheable node's inputs evicts its stale cache entry.
//!
//! These tests reach the `Graph` arena through the `BuiltFunctionGraph::graph`
//! field, since the `Graph` type itself isn't named at the crate root.

mod common;

use ir::node::{NodeKind, NodeOutputKind, NodeOutputType};
use ir::{FunctionBuilder, IntBinaryOp};

fn empty_built() -> ir::BuiltFunctionGraph {
    FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)
        .unwrap()
        .build()
        .unwrap()
}

#[test]
fn cacheable_int_const_dedupes_on_repeat_create() {
    let mut fg = empty_built();
    let a = fg.graph.create_node(
        NodeKind::IntConst(42),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    let b = fg.graph.create_node(
        NodeKind::IntConst(42),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    assert_eq!(a, b, "same (kind, inputs, output_kinds) must dedup");
}

#[test]
fn cacheable_int_const_with_different_type_does_not_dedup() {
    let mut fg = empty_built();
    let a = fg.graph.create_node(
        NodeKind::IntConst(0),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    let b = fg.graph.create_node(
        NodeKind::IntConst(0),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    assert_ne!(a, b);
}

#[test]
fn non_cacheable_return_never_dedupes() {
    let mut fg = empty_built();
    let a = fg.graph.create_node(NodeKind::Return, [], []);
    let b = fg.graph.create_node(NodeKind::Return, [], []);
    assert_ne!(a, b);
}

#[test]
fn non_cacheable_stack_store_phi_never_dedupes() {
    let mut fg = empty_built();
    let space = rsleigh::VnSpace::RAM;
    let a = fg.graph.create_node(
        NodeKind::StackStorePhi { space },
        [],
        [NodeOutputKind::Memory],
    );
    let b = fg.graph.create_node(
        NodeKind::StackStorePhi { space },
        [],
        [NodeOutputKind::Memory],
    );
    assert_ne!(
        a, b,
        "StackStorePhi has side-table state that breaks the cache key"
    );
}

#[test]
fn detach_node_inputs_evicts_cacheable_node_from_cache() {
    let mut fg = empty_built();
    let lhs = fg.graph.create_node(
        NodeKind::IntConst(7),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    let rhs = fg.graph.create_node(
        NodeKind::IntConst(9),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    let [lhs_out] = fg.graph.node_outputs_exact::<1>(lhs).unwrap();
    let [rhs_out] = fg.graph.node_outputs_exact::<1>(rhs).unwrap();
    let ty = NodeOutputKind::OutputType(NodeOutputType::U32);

    let add_a = fg.graph.create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [lhs_out, rhs_out],
        [ty],
    );
    fg.graph.detach_node_inputs(add_a);

    // Re-creating with the original key must produce a fresh NodeId, not
    // resurrect the now-zero-input zombie.
    let add_b = fg.graph.create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [lhs_out, rhs_out],
        [ty],
    );
    assert_ne!(add_a, add_b);
    assert_eq!(fg.graph.node_inputs(add_b).into_iter().count(), 2);
}

#[test]
fn update_input_evicts_cacheable_node_from_cache() {
    let mut fg = empty_built();
    let one = fg.graph.create_node(
        NodeKind::IntConst(1),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    let two = fg.graph.create_node(
        NodeKind::IntConst(2),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    let three = fg.graph.create_node(
        NodeKind::IntConst(3),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    let [a] = fg.graph.node_outputs_exact::<1>(one).unwrap();
    let [b] = fg.graph.node_outputs_exact::<1>(two).unwrap();
    let [c] = fg.graph.node_outputs_exact::<1>(three).unwrap();
    let ty = NodeOutputKind::OutputType(NodeOutputType::U32);

    let add = fg.graph.create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [a, b],
        [ty],
    );

    let in0 = fg.graph.node_input_id_at(add, 0);
    fg.graph.update_input(in0, c);

    // Original key (a, b) must no longer hit `add`.
    let fresh = fg.graph.create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [a, b],
        [ty],
    );
    assert_ne!(add, fresh);
}
