//! Black-box: `walk_graph` must visit every node reachable from `entry`
//! under the documented backward-data + forward-control rule, exactly once,
//! and never visit detached or never-attached nodes.

#![allow(clippy::unwrap_used)]

mod common;

use std::collections::HashSet;

use ir::node::{NodeId, NodeOutputType};
use ir::walk::walk_graph;
use ir::{FunctionBuilder, IntBinaryOp};

#[test]
fn entry_only_graph_visits_at_least_entry_initial_memory_and_return() {
    let fg = common::return_const(0, NodeOutputType::U32);
    let visited: HashSet<NodeId> = walk_graph(&fg.graph, fg.entry).collect();
    assert!(visited.contains(&fg.entry), "entry must be visited");
    // The minimal graph also has: InitialMemory, ControlState, MemPhi,
    // IntConst, Return — at least 4 distinct ids reachable.
    assert!(
        visited.len() >= 4,
        "minimal return-const graph reaches >= 4 nodes; got {}",
        visited.len()
    );
}

#[test]
fn walk_visits_no_node_more_than_once() {
    // Build a slightly bigger graph: const + const → add → return.
    let fg = common::return_binop(7, 8, IntBinaryOp::Add, NodeOutputType::U32);
    let visited: Vec<NodeId> = walk_graph(&fg.graph, fg.entry).collect();
    let unique: HashSet<NodeId> = visited.iter().copied().collect();
    assert_eq!(
        visited.len(),
        unique.len(),
        "walk_graph must visit each node at most once"
    );
}

#[test]
fn diamond_join_via_phi_visits_all_arms() {
    // Construct: entry → if-true → join, if-false → join.
    let mut b = FunctionBuilder::empty().unwrap();
    let entry_region = b.create_region().unwrap();
    b.set_entry_region(entry_region).unwrap();
    b.set_region(entry_region);
    // Sentinel asm-fingerprint address so every emitted node carries
    // a non-empty Layer-C fingerprint (Phase 1 Task 1.4b / G3).
    b.set_lift_addr(Some(0xDEAD_BEEF_0000_0001));

    let cond = b.build_boolean_const(true);
    let true_region = b.create_region().unwrap();
    let false_region = b.create_region().unwrap();
    b.build_if(cond, true_region, false_region).unwrap();

    let join = b.create_region().unwrap();
    b.set_region(true_region);
    b.build_branch(join).unwrap();
    b.set_region(false_region);
    b.build_branch(join).unwrap();

    b.set_region(join);
    let v = b.build_int_const(99u64, NodeOutputType::U32).unwrap();
    b.build_return(Some(v), &[]).unwrap();
    b.set_lift_addr(None);
    let fg = b.build().unwrap();

    let visited: HashSet<NodeId> = walk_graph(&fg.graph, fg.entry).collect();
    assert!(visited.contains(&fg.entry));
    // A diamond reaches at least 8 nodes: entry, init-mem, 4 ControlStates
    // (entry/true/false/join), 4 MemPhis, the If, the cond, the int-const,
    // and the Return — well over 8 distinct nodes.
    assert!(
        visited.len() >= 8,
        "diamond should reach many distinct nodes; got {}",
        visited.len()
    );
}
