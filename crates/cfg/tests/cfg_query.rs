#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Tests for `Cfg` query methods: `region_if`, `region_branch`,
//! `regions`, `region_ids`, and the `DuplicateEdgeKind` error.

mod common;
use common::{binary, build_cfg, make_region, make_sleigh};

use cfg::{Cfg, RegionEdgeKind};
use petgraph::stable_graph::StableDiGraph;

fn real_cfg(fn_name: &str) -> Cfg<reader::ElfFileMemReader> {
    let p = binary("x64", fn_name);
    build_cfg(
        p.to_str().unwrap(),
        fn_name,
        rsleigh::sla_spec::SLA_SPEC_X86_64,
        rsleigh::pspec::PSPEC_X86_64,
    )
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
        start_addr_to_region_id: std::collections::BTreeMap::new(),
    };

    let err = cfg.region_branch(src).unwrap_err();
    assert!(
        err.to_string().contains("more than one outgoing edge"),
        "got: {err}"
    );
}

// ── W3: region_id_at_start public-API contract ────────────────────────────

#[test]
fn region_id_at_start_returns_some_for_real_function_entry() {
    // W3 — `region_id_at_start` is `pub` (cross-crate-callable),
    // not `pub(crate)` / `test_api`-only.  This test pins the public
    // contract from the cfg crate's own test suite so an accidental
    // visibility narrowing (which would break F5's
    // `IndirectBranchResolve` in the opt crate) fails at the cfg
    // boundary.
    let cfg = real_cfg("add");
    // The CFG entry's region must start at the function's entry
    // machine address; `region_id_at_start` therefore finds it.
    let entry_region = cfg
        .graph
        .node_weight(cfg.entry)
        .expect("entry region exists");
    let entry_addr = entry_region.start_addr.machine_addr;
    let rid = cfg.region_id_at_start(entry_addr);
    assert_eq!(
        rid,
        Some(cfg.entry),
        "region_id_at_start must locate the entry region by its start addr",
    );
}

#[test]
fn region_id_at_start_returns_none_for_unknown_machine_addr() {
    // An address that does not start any region in the cfg returns
    // None; the helper only matches `start_addr.machine_addr`, never
    // mid-region addresses.
    let cfg = real_cfg("add");
    let rid = cfg.region_id_at_start(cfg::MachineInsnAddr { addr: 0xdead_beef });
    assert!(rid.is_none(), "unknown addr must return None, got {rid:?}");
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
        start_addr_to_region_id: std::collections::BTreeMap::new(),
    };

    let err = cfg
        .region_if(src)
        .map(|_| ())
        .expect_err("region_if must return DuplicateEdgeKind on malformed graph");
    assert!(
        err.to_string().contains("more than one outgoing edge"),
        "got: {err}"
    );
}
