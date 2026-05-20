//! Verifies the content-keyed BFG cache as a standalone module.

use std::sync::Arc;

use strider_analyze::orchestrator::cache::BfgContentCache;
use strider_ir::BuiltFunctionGraph;
use strider_ir::node::{NodeKind, NodeOutputKind};

/// Build a minimal `BuiltFunctionGraph` whose only purpose is to be a
/// distinct, Arc-shareable value in the cache.  Mirrors the
/// `compact_remaps_entry_and_drops_zombies` scaffold in
/// `strider_ir::function`'s test module.
fn make_minimal_bfg() -> BuiltFunctionGraph {
    let mut graph = strider_ir::graph::Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    BuiltFunctionGraph::from_graph_and_entry_for_rewrite(graph, entry)
}

#[test]
fn empty_cache_misses() {
    let c = BfgContentCache::new();
    assert!(c.get(0xdead_beef).is_none());
}

#[test]
fn cache_hits_after_insert() {
    let c = BfgContentCache::new();
    c.insert(0xdead_beef, Arc::new(make_minimal_bfg()));
    assert!(c.get(0xdead_beef).is_some());
}

#[test]
fn cache_clear_drops_entries() {
    let c = BfgContentCache::new();
    c.insert(0x1, Arc::new(make_minimal_bfg()));
    assert!(c.get(0x1).is_some());
    c.clear();
    assert!(c.get(0x1).is_none());
}
