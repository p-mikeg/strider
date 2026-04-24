#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Tests for `Cfg` query methods: `region_insn`, `region_if`, `region_branch`,
//! `regions`, `region_ids`, and the `DuplicateEdgeKind` error.

mod common;
use common::{binary, build_cfg, make_region, make_sleigh};

use cfg::{Cfg, ErrorKind, RegionEdgeKind};
use petgraph::graph::NodeIndex;
use petgraph::stable_graph::StableDiGraph;

fn real_cfg(fn_name: &str) -> Cfg<reader::ElfFileMemReader> {
    let p = binary("x64");
    build_cfg(
        p.to_str().unwrap(),
        fn_name,
        rsleigh::sla_spec::SLA_SPEC_X86_64,
        rsleigh::pspec::PSPEC_X86_64,
    )
}

// ── region_insn ──────────────────────────────────────────────────────────────

#[test]
fn region_insn_returns_clone_of_region_insns() {
    let cfg = real_cfg("add");
    let insns = cfg.region_insn(cfg.entry).unwrap();
    assert!(!insns.is_empty());
    // Cloning — the underlying region still has its instructions.
    assert_eq!(cfg.graph[cfg.entry].insns.len(), insns.len());
}

#[test]
fn region_insn_invalid_node_index_returns_error() {
    let cfg = real_cfg("add");
    let bogus = NodeIndex::new(10_000);
    let err = cfg.region_insn(bogus).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::InvalidRegion(_)));
}

// ── regions / region_ids iteration ───────────────────────────────────────────

#[test]
fn regions_iterator_count_matches_node_count() {
    let cfg = real_cfg("sum_to_n");
    assert_eq!(cfg.regions().count(), cfg.graph.node_count());
}

#[test]
fn region_ids_iterator_count_matches_node_count() {
    let cfg = real_cfg("sum_to_n");
    assert_eq!(cfg.region_ids().count(), cfg.graph.node_count());
}

// ── region_branch ────────────────────────────────────────────────────────────

#[test]
fn region_branch_returns_none_for_linear_entry() {
    let cfg = real_cfg("add");
    assert!(cfg.region_branch(cfg.entry).unwrap().is_none());
}

// ── region_if ────────────────────────────────────────────────────────────────

#[test]
fn region_if_both_successors_present_on_abs_val() {
    let cfg = real_cfg("abs_val");
    let has_pair = cfg.region_ids().any(|id| {
        let s = cfg.region_if(id).unwrap();
        s.if_true_region.is_some() && s.if_false_region.is_some()
    });
    assert!(has_pair, "abs_val: no region with both if-true and if-false successors");
}

#[test]
fn region_if_absent_on_linear_entry() {
    let cfg = real_cfg("add");
    let s = cfg.region_if(cfg.entry).unwrap();
    assert!(s.if_true_region.is_none());
    assert!(s.if_false_region.is_none());
}

// ── DuplicateEdgeKind error ──────────────────────────────────────────────────

#[test]
fn duplicate_edge_kind_is_detected_by_region_branch() {
    // Manually construct a malformed Cfg with two Branch edges from one node
    // to distinct destinations. `region_branch` (via `following_regions`)
    // must return DuplicateEdgeKind.
    let mut graph: StableDiGraph<cfg::test_api::Region, RegionEdgeKind> = StableDiGraph::new();
    let src = graph.add_node(make_region(&[(0x1000, 0)]));
    let dst1 = graph.add_node(make_region(&[(0x2000, 0)]));
    let dst2 = graph.add_node(make_region(&[(0x3000, 0)]));
    graph.add_edge(src, dst1, RegionEdgeKind::Branch);
    graph.add_edge(src, dst2, RegionEdgeKind::Branch);

    let cfg = cfg::Cfg {
        sleigh: make_sleigh(),
        graph,
        entry: src,
    };

    let err = cfg.region_branch(src).unwrap_err();
    assert!(matches!(
        err.kind(),
        ErrorKind::DuplicateEdgeKind(_, RegionEdgeKind::Branch)
    ));
}

#[test]
fn duplicate_if_case_true_is_detected_by_region_if() {
    // Two IfCaseTrue edges — should fail through `region_if`'s call to
    // `following_regions`.
    let mut graph: StableDiGraph<cfg::test_api::Region, RegionEdgeKind> = StableDiGraph::new();
    let src = graph.add_node(make_region(&[(0x1000, 0)]));
    let dst1 = graph.add_node(make_region(&[(0x2000, 0)]));
    let dst2 = graph.add_node(make_region(&[(0x3000, 0)]));
    graph.add_edge(src, dst1, RegionEdgeKind::IfCaseTrue);
    graph.add_edge(src, dst2, RegionEdgeKind::IfCaseTrue);

    let cfg = cfg::Cfg {
        sleigh: make_sleigh(),
        graph,
        entry: src,
    };

    let result = cfg.region_if(src);
    let err = match result {
        Err(e) => e,
        Ok(_) => panic!("expected DuplicateEdgeKind error"),
    };
    assert!(matches!(
        err.kind(),
        ErrorKind::DuplicateEdgeKind(_, RegionEdgeKind::IfCaseTrue)
    ));
}
