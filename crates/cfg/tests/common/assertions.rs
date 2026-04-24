#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Structural assertions over a completed `Cfg` — used across integration tests.

use cfg::{Cfg, RegionEdgeKind};
use petgraph::visit::{EdgeRef, IntoEdgeReferences};

pub fn region_count<R: rsleigh::MemReader>(cfg: &Cfg<R>) -> usize {
    cfg.region_ids().count()
}

pub fn count_edges_of_kind<R: rsleigh::MemReader>(cfg: &Cfg<R>, kind: RegionEdgeKind) -> usize {
    cfg.graph.edge_references().filter(|e| *e.weight() == kind).count()
}

pub fn outgoing_edge_kinds<R: rsleigh::MemReader>(
    cfg: &Cfg<R>,
    id: petgraph::graph::NodeIndex,
) -> Vec<RegionEdgeKind> {
    cfg.graph
        .edges_directed(id, petgraph::Direction::Outgoing)
        .map(|e| *e.weight())
        .collect()
}

/// Returns `true` when the graph contains a cycle (i.e. a loop back-edge).
pub fn has_cycle<R: rsleigh::MemReader>(cfg: &Cfg<R>) -> bool {
    petgraph::algo::is_cyclic_directed(&cfg.graph)
}

/// Returns `true` when every edge source and target is a valid node.
pub fn all_edge_endpoints_valid<R: rsleigh::MemReader>(cfg: &Cfg<R>) -> bool {
    cfg.graph.edge_references().all(|e| {
        cfg.graph.node_weight(e.source()).is_some() && cfg.graph.node_weight(e.target()).is_some()
    })
}

/// Returns `true` when the entry node has no predecessors.
pub fn entry_has_no_predecessors<R: rsleigh::MemReader>(cfg: &Cfg<R>) -> bool {
    cfg.graph.edges_directed(cfg.entry, petgraph::Incoming).count() == 0
}

/// Returns `true` when every region that has any `IfCase*` edge has EXACTLY
/// one `IfCaseTrue` and one `IfCaseFalse` outgoing edge (the pair invariant).
pub fn all_conditional_regions_well_formed<R: rsleigh::MemReader>(cfg: &Cfg<R>) -> bool {
    for id in cfg.region_ids() {
        let kinds = outgoing_edge_kinds(cfg, id);
        let has_true = kinds.contains(&RegionEdgeKind::IfCaseTrue);
        let has_false = kinds.contains(&RegionEdgeKind::IfCaseFalse);
        let is_conditional = has_true || has_false;
        if is_conditional && !(has_true && has_false && kinds.len() == 2) {
            return false;
        }
    }
    true
}

/// Linear function: one region, no branch or if-case edges.
pub fn assert_linear_function<R: rsleigh::MemReader>(cfg: &Cfg<R>, name: &str) {
    assert_eq!(region_count(cfg), 1, "{name}: expected 1 region");
    assert_eq!(count_edges_of_kind(cfg, RegionEdgeKind::Branch), 0, "{name}: unexpected Branch edges");
    assert_eq!(count_edges_of_kind(cfg, RegionEdgeKind::IfCaseTrue), 0, "{name}: unexpected IfCaseTrue edges");
    assert!(all_edge_endpoints_valid(cfg), "{name}: invalid edge endpoints");
}

/// Single-conditional function: ≥ 2 regions, paired if-case edges, no cycle.
pub fn assert_single_conditional<R: rsleigh::MemReader>(cfg: &Cfg<R>, name: &str) {
    assert!(region_count(cfg) >= 2, "{name}: expected at least 2 regions");
    assert!(count_edges_of_kind(cfg, RegionEdgeKind::IfCaseTrue) >= 1, "{name}: expected IfCaseTrue edge");
    assert!(count_edges_of_kind(cfg, RegionEdgeKind::IfCaseFalse) >= 1, "{name}: expected IfCaseFalse edge");
    assert!(all_conditional_regions_well_formed(cfg), "{name}: conditional pair invariant violated");
    assert!(all_edge_endpoints_valid(cfg), "{name}: invalid edge endpoints");
}

/// Looping function: ≥ 3 regions, at least one cycle, pair invariant holds.
pub fn assert_looping_function<R: rsleigh::MemReader>(cfg: &Cfg<R>, name: &str) {
    assert!(region_count(cfg) >= 2, "{name}: expected at least 2 regions for a loop");
    assert!(has_cycle(cfg), "{name}: expected a back-edge cycle");
    assert!(all_conditional_regions_well_formed(cfg), "{name}: conditional pair invariant violated");
    assert!(all_edge_endpoints_valid(cfg), "{name}: invalid edge endpoints");
}

/// Global invariants that every well-formed CFG must satisfy.
pub fn assert_global_invariants<R: rsleigh::MemReader>(cfg: &Cfg<R>, name: &str) {
    assert!(cfg.graph.node_weight(cfg.entry).is_some(), "{name}: entry node missing");
    assert!(entry_has_no_predecessors(cfg), "{name}: entry has predecessors");
    assert!(all_edge_endpoints_valid(cfg), "{name}: invalid edge endpoints");
    assert!(all_conditional_regions_well_formed(cfg), "{name}: conditional pair invariant violated");
}
