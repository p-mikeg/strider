//! Tests for `Graph::retain_reachable` — compaction of unreachable nodes.

#![allow(clippy::unwrap_used)]

use ir::Graph;
use ir::node::{NodeKind, NodeOutputKind, NodeOutputType};

#[test]
fn retain_reachable_drops_detached_zombie_node() {
    let mut graph = Graph::new();

    // Live: an entry-typed node we'll treat as the root.
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);

    // Doomed zombie: a free-standing IntConst with no consumers.
    let zombie = graph.create_node(
        NodeKind::IntConst(0xdead_beef),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let pre = graph.all_node_ids().count();
    assert!(pre >= 2, "graph must hold both nodes pre-compaction");
    assert!(graph.all_node_ids().any(|n| n == zombie));

    let remap = graph.retain_reachable(entry).unwrap();

    // Zombie no longer in the graph.
    let post = graph.all_node_ids().count();
    assert!(post < pre, "compaction must shrink the graph");
    assert!(remap.node_old_to_new(zombie).is_none(), "zombie has no remap entry");
    assert!(remap.node_old_to_new(entry).is_some(), "entry survives");

    // Live entry still has its single Control output.
    let new_entry = remap.node_old_to_new(entry).unwrap();
    let outs: Vec<_> = graph.node_outputs(new_entry).into_iter().collect();
    assert_eq!(outs.len(), 1);
    assert!(graph.output_kind(outs[0]).is_control());
}

#[test]
fn retain_reachable_preserves_asm_fingerprint_on_surviving_node() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    graph.set_asm_fingerprint(entry, vec![0x1000, 0x1004, 0x1008]);

    let remap = graph.retain_reachable(entry).unwrap();
    let new_entry = remap.node_old_to_new(entry).unwrap();
    assert_eq!(graph.asm_fingerprint(new_entry), &[0x1000, 0x1004, 0x1008]);
}

#[test]
fn retain_reachable_rebuilds_dedup_cache() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _remap = graph.retain_reachable(entry).unwrap();

    // After compaction, creating a cacheable node with identical
    // (kind, inputs, output_kinds) should dedup correctly.
    let one_a = graph.create_node(
        NodeKind::IntConst(7),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let one_b = graph.create_node(
        NodeKind::IntConst(7),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    assert_eq!(one_a, one_b, "dedup cache must be rebuilt");
}

#[test]
fn retain_reachable_drops_side_table_entry_for_dropped_node() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let zombie = graph.create_node(
        NodeKind::IntConst(0),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    graph.set_asm_fingerprint(zombie, vec![0xdead]);
    let remap = graph.retain_reachable(entry).unwrap();
    assert!(remap.node_old_to_new(zombie).is_none());
    // Surviving entry has no fingerprint entry leaking from the dropped zombie.
    let new_entry = remap.node_old_to_new(entry).unwrap();
    assert!(graph.asm_fingerprint(new_entry).is_empty());
}
