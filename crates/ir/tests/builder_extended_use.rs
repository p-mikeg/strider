//! `FunctionBuilder` exposes its inner `Graph` (via `graph_mut()`)
//! and `entry()` so callers can run analysis passes (and arbitrary
//! in-place mutations) WITHOUT consuming the builder via `build()`.
//! After such mutations, calling `build()` must still produce a
//! valid `BuiltFunctionGraph`.
//!
//! These tests pin the contract that opt-pass plumbing depends on.
//! They don't import the `opt` crate (would create a dep cycle in
//! tests) — instead they mutate the graph directly via the public
//! `Graph` API the same way an opt pass would.

#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

mod common;

use ir::node::{NodeKind, NodeOutputKind, NodeOutputType};
use ir::FunctionBuilder;

/// Drive the builder through several rounds of in-place mutation
/// (mimicking an iterative analysis loop) without ever consuming it
/// via `build()`.  At each step the builder's `entry()` must continue
/// to point at the same node, and `graph_mut()` must keep producing
/// fresh node ids.
#[test]
fn analysis_loop_without_build_round_trips() {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    let v = b.build_int_const(0u64, NodeOutputType::U64);
    b.build_return(Some(v), &[]).unwrap();

    // Capture the entry once - it must remain stable across iterations.
    let entry = b.entry();

    // Round 1: synthesize a new IntConst via graph_mut().
    let r1 = b.graph_mut().create_node(
        NodeKind::IntConst(1u128),
        std::iter::empty(),
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    assert_eq!(b.entry(), entry, "entry() must be stable after round 1");

    // Round 2: another mutation - round 1's node must persist.
    let r2 = b.graph_mut().create_node(
        NodeKind::IntConst(2u128),
        std::iter::empty(),
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    assert_eq!(b.entry(), entry, "entry() must be stable after round 2");
    assert_ne!(r1, r2, "consecutive create_node calls must produce distinct ids");

    // Both synthesized nodes are live in the arena.
    assert!(matches!(b.body().graph.node_kind(r1), NodeKind::IntConst(1)));
    assert!(matches!(b.body().graph.node_kind(r2), NodeKind::IntConst(2)));
}

/// After driving the builder through several rounds of in-place
/// mutation, calling `build()` must still produce a valid
/// `BuiltFunctionGraph` (i.e. one that passes `validate`).  Pins
/// the "build still works after extended use" contract.
#[test]
fn final_build_after_extended_use_yields_valid_built() {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    let v = b.build_int_const(7u64, NodeOutputType::U64);
    b.build_return(Some(v), &[]).unwrap();

    // N rounds of in-place mutation via graph_mut() - the same
    // shape an opt pass would use: synthesize a fresh node, leave it
    // unattached.  The validator skips unreachable nodes, so leaving
    // them detached is still valid.
    for k in 1u128..=5 {
        b.graph_mut().create_node(
            NodeKind::IntConst(k),
            std::iter::empty(),
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
    }

    // build() must succeed - its validate step is the contract.
    let built = b.build().expect("build() after extended use must succeed");
    assert!(built.preorder().count() >= 1, "preorder must visit Entry at minimum");
}
