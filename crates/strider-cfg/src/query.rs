use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;

use super::types::{Region, RegionTerminator};
use super::{Cfg, RegionId};
use anyhow::anyhow;

use crate::Result;

/// True when `target` lies outside the half-open function range
/// `[start_addr, start_addr + fn_max_size)`.
///
/// `allow_code_before_start_addr` relaxes the lower bound ONLY while
/// `fn_max_size` is `None`.  With a size set the extent is known exactly, so
/// any `target < start_addr` is a tail call whatever the flag says.
///
/// The window is non-wrapping.  When `start_addr + fn_max_size` overflows it
/// clamps to `[start_addr, u64::MAX]` and the upper bound is dropped
/// entirely.
pub(crate) fn is_addr_tail_call(
    target: u64,
    start_addr: u64,
    fn_max_size: Option<u64>,
    allow_code_before_start_addr: bool,
) -> bool {
    // `lower == 0` disables the lower-bound check; `upper == None` disables
    // the upper one.
    let lower_bound_strict = fn_max_size.is_some() || !allow_code_before_start_addr;
    let lower = if lower_bound_strict { start_addr } else { 0 };
    if target < lower {
        return true;
    }
    // `checked_add`, NOT `saturating_add`: on overflow the `None` drops the
    // upper-bound check, whereas a saturating bound with `target >= upper`
    // would misclassify `target == u64::MAX` as out of range.
    if let Some(sz) = fn_max_size
        && let Some(upper) = start_addr.checked_add(sz)
        && target >= upper
    {
        return true;
    }
    false
}

pub struct IfRegionSuccessors {
    pub if_true_region: Option<NodeIndex>,
    /// The fall-through side.
    pub if_false_region: Option<NodeIndex>,
}

impl Cfg {
    /// The outgoing edge whose target region CONTAINS the terminator's
    /// `true_target` is the taken side, the other the fall-through.  A
    /// degenerate `if (c) goto L else goto L` reports that region for both;
    /// a non-`CondBranch` region reports `None` for both.
    ///
    /// Containment, not a start compare: a `true_target` off an instruction
    /// boundary is seated as an edge to the region that OWNS it, which starts
    /// elsewhere.  It also covers an intra-machine-instruction target at a
    /// non-zero pcode index.
    pub fn region_if(&self, region_id: RegionId) -> Result<IfRegionSuccessors> {
        let region = self
            .region_graph
            .node_weight(region_id)
            .ok_or_else(|| anyhow!("invalid region index {region_id:?}"))?;
        let RegionTerminator::CondBranch { true_target } = &region.terminator else {
            return Ok(IfRegionSuccessors {
                if_true_region: None,
                if_false_region: None,
            });
        };
        let true_target = *true_target;
        let mut if_true_region = None;
        let mut if_false_region = None;
        for edge in self
            .region_graph
            .edges_directed(region_id, petgraph::Outgoing)
        {
            let target = edge.target();
            let contains_taken = self
                .region_graph
                .node_weight(target)
                .ok_or_else(|| {
                    anyhow!("dangling edge target {target:?} from region {region_id:?}")
                })?
                .contains_addr(true_target);
            // Guarding on `if_true_region.is_none()` keeps the degenerate
            // both-arms-same-region case sane: the second edge falls through
            // to `if_false_region` instead of overwriting the taken side.
            if contains_taken && if_true_region.is_none() {
                if_true_region = Some(target);
            } else {
                if_false_region = Some(target);
            }
        }
        Ok(IfRegionSuccessors {
            if_true_region,
            if_false_region,
        })
    }

    /// Unordered.
    pub fn regions(&self) -> impl Iterator<Item = &Region> {
        self.region_graph.node_weights()
    }

    /// Unordered; a predecessor with parallel edges is yielded once per edge,
    /// and dangling sources are skipped.
    pub fn region_predecessors(&self, region_id: RegionId) -> impl Iterator<Item = &Region> {
        self.region_graph
            .edges_directed(region_id, petgraph::Incoming)
            .filter_map(|edge| self.region_graph.node_weight(edge.source()))
    }

    /// Unordered.
    pub fn region_ids(&self) -> impl Iterator<Item = RegionId> {
        self.region_graph.node_indices()
    }

    /// Every arm of `switch_region`, keyed by the address it starts at: the
    /// regions the CFG builder wired for that jump-table's targets.  One pass,
    /// for a caller resolving all of a table's targets at once.
    ///
    /// Keyed by successor edge rather than a global start-address lookup, which
    /// stays correct across a later `split_region` that re-targets the incoming
    /// edge.  Keyed on the full `start_addr`: a target landing inside an
    /// already-decoded region splits it, so a wired arm always begins exactly at
    /// the target, and a target is always a machine-instruction start.  A
    /// successor beginning MID-pcode at the same machine address (a `CondBranch`
    /// into a pcode sequence) is a different region and does not answer for it.
    pub fn switch_arm_regions(
        &self,
        switch_region: RegionId,
    ) -> rustc_hash::FxHashMap<super::types::PcodeInsnAddr, RegionId> {
        self.region_graph
            .neighbors(switch_region)
            .filter_map(|s| {
                self.region_graph
                    .node_weight(s)
                    .map(|region| (region.start_addr, s))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use petgraph::stable_graph::StableDiGraph;
    use strider_target::SleighArch;

    use super::*;
    use crate::test_support::*;
    use crate::types::{PcodeInsnAddr, Region};
    use crate::{Builder, CfgOptions};

    #[test]
    fn is_addr_tail_call_overflowing_window_top_addr_is_in_range() {
        // The window cannot wrap, so it clamps to `[start, u64::MAX]` and
        // every target at or above start, u64::MAX included, is in range.
        let start = u64::MAX - 0x100;
        let sz = 0x1000u64; // overflows when added to start
        assert!(
            !is_addr_tail_call(u64::MAX, start, Some(sz), false),
            "u64::MAX is the top of the non-wrapping window, must be in-range"
        );
        assert!(
            !is_addr_tail_call(start + 0x10, start, Some(sz), false),
            "interior of an overflowing window must be in-range"
        );
        assert!(
            is_addr_tail_call(start - 1, start, Some(sz), false),
            "below start is still a tail call"
        );
    }

    fn real_cfg(case: &str, fn_name: &str) -> Cfg {
        use object::{Object, ObjectSymbol};

        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/out/x64")
            .join(format!("{case}.elf"));
        assert!(
            path.exists(),
            "missing test binary {path:?}; run `make -C fixtures` first"
        );
        let obj = strider_reader::load_elf(&path)
            .unwrap_or_else(|e| panic!("load_elf({path:?}) failed: {e:?}"));
        let obj = obj.file();
        let mem = strider_reader::ElfFileMemReader::from_object(&obj)
            .expect("ElfFileMemReader::from_object");
        let arch = SleighArch::x86_64();
        let mut sleigh =
            rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), mem).expect("create Sleigh");
        let entry_addr = obj
            .symbol_by_name(fn_name)
            .unwrap_or_else(|| panic!("symbol {fn_name:?} not found in {path:?}"))
            .address();
        Builder::for_arch(&arch, &mut sleigh, entry_addr, &CfgOptions::default())
            .build()
            .unwrap_or_else(|e| panic!("Builder::build for {fn_name:?}: {e:?}"))
    }

    #[test]
    fn regions_iterator_count_matches_node_count() {
        let cfg = real_cfg("control", "sum_to_n");
        assert_eq!(cfg.regions().count(), cfg.region_graph.node_count());
    }

    #[test]
    fn region_ids_iterator_count_matches_node_count() {
        let cfg = real_cfg("control", "sum_to_n");
        assert_eq!(cfg.region_ids().count(), cfg.region_graph.node_count());
    }

    #[test]
    fn region_if_both_successors_present_on_abs_val() {
        let cfg = real_cfg("control", "abs_val");
        let has_pair = cfg.region_ids().any(|id| {
            let s = cfg.region_if(id).unwrap();
            s.if_true_region.is_some() && s.if_false_region.is_some()
        });
        assert!(
            has_pair,
            "abs_val: no region with both if-true and if-false successors"
        );
    }

    #[test]
    fn region_if_absent_on_linear_entry() {
        let cfg = real_cfg("arithmetic", "add");
        let s = cfg.region_if(cfg.entry).unwrap();
        assert!(s.if_true_region.is_none());
        assert!(s.if_false_region.is_none());
    }

    fn make_cond_region(start_machine: u64, true_target: PcodeInsnAddr) -> Region {
        let mut r = make_region(&[(start_machine, 0)]);
        r.terminator = RegionTerminator::CondBranch { true_target };
        r
    }

    #[test]
    fn region_if_resolves_polarity_by_true_target() {
        // The taken side is decided by containment, not edge insertion order.
        let mut graph: StableDiGraph<Region, ()> = StableDiGraph::new();
        let src = graph.add_node(make_cond_region(0x1000, addr(0x3000, 0)));
        let fallthrough = graph.add_node(make_region(&[(0x2000, 0)]));
        let taken = graph.add_node(make_region(&[(0x3000, 0)]));
        // Fall-through edge FIRST, to prove order-independence.
        graph.add_edge(src, fallthrough, ());
        graph.add_edge(src, taken, ());

        let cfg = Cfg {
            region_graph: graph,
            entry: src,
            undecodable_seeded: Vec::new(),
            isa_mode_conflicts: Vec::new(),
            interior_branch_targets: Vec::new(),
            link_register_seated: Vec::new(),
            tail_call_seated: Vec::new(),
            function_isa_bit: None,
        };

        let s = cfg.region_if(src).unwrap();
        assert_eq!(
            s.if_true_region,
            Some(taken),
            "taken side = true_target match"
        );
        assert_eq!(s.if_false_region, Some(fallthrough));
    }

    #[test]
    fn region_if_matches_taken_successor_by_containment_not_start() {
        // A hand-built region spanning [0x3000, 0x3010] with the branch
        // targeting the interior 0x3008, to pin that region_if matches the taken
        // arm by containment rather than start-address equality.
        let mut graph: StableDiGraph<Region, ()> = StableDiGraph::new();
        let src = graph.add_node(make_cond_region(0x1000, addr(0x3008, 0)));
        let fallthrough = graph.add_node(make_region(&[(0x2000, 0)]));
        let taken = graph.add_node(make_region(&[(0x3000, 0), (0x3010, 0)]));
        graph.add_edge(src, fallthrough, ());
        graph.add_edge(src, taken, ());

        let cfg = Cfg {
            region_graph: graph,
            entry: src,
            undecodable_seeded: Vec::new(),
            isa_mode_conflicts: Vec::new(),
            interior_branch_targets: Vec::new(),
            link_register_seated: Vec::new(),
            tail_call_seated: Vec::new(),
            function_isa_bit: None,
        };

        let s = cfg.region_if(src).unwrap();
        assert_eq!(
            s.if_true_region,
            Some(taken),
            "interior true_target must match the taken successor by containment"
        );
        assert_eq!(s.if_false_region, Some(fallthrough));
    }

    #[test]
    fn region_if_both_arms_same_region_returns_that_region_for_both() {
        // Degenerate `if (c) goto L else goto L`: both edges point at one
        // region, which must be reported for both sides.
        let mut graph: StableDiGraph<Region, ()> = StableDiGraph::new();
        let src = graph.add_node(make_cond_region(0x1000, addr(0x2000, 0)));
        let both = graph.add_node(make_region(&[(0x2000, 0)]));
        graph.add_edge(src, both, ());
        graph.add_edge(src, both, ());

        let cfg = Cfg {
            region_graph: graph,
            entry: src,
            undecodable_seeded: Vec::new(),
            isa_mode_conflicts: Vec::new(),
            interior_branch_targets: Vec::new(),
            link_register_seated: Vec::new(),
            tail_call_seated: Vec::new(),
            function_isa_bit: None,
        };

        let s = cfg.region_if(src).unwrap();
        assert_eq!(s.if_true_region, Some(both));
        assert_eq!(s.if_false_region, Some(both));
    }

    #[test]
    fn switch_arm_regions_keys_each_arm_by_its_own_start() {
        // Three switch arms; each keys to its own successor, and an address no
        // arm starts at is absent. The map is built from outgoing edges, so
        // only successors of `src` are considered.
        let mut graph: StableDiGraph<Region, ()> = StableDiGraph::new();
        let src = graph.add_node(make_region(&[(0x1000, 0)]));
        let arm_a = graph.add_node(make_region(&[(0x2000, 0)]));
        let arm_b = graph.add_node(make_region(&[(0x3000, 0)]));
        let arm_c = graph.add_node(make_region(&[(0x4000, 0)]));
        // A non-successor region starting at a would-be target, to prove the
        // lookup is edge-scoped rather than global.
        let stranger = graph.add_node(make_region(&[(0x5000, 0)]));
        graph.add_edge(src, arm_a, ());
        graph.add_edge(src, arm_b, ());
        graph.add_edge(src, arm_c, ());

        let cfg = Cfg {
            region_graph: graph,
            entry: src,
            undecodable_seeded: Vec::new(),
            isa_mode_conflicts: Vec::new(),
            interior_branch_targets: Vec::new(),
            link_register_seated: Vec::new(),
            tail_call_seated: Vec::new(),
            function_isa_bit: None,
        };

        let arms = cfg.switch_arm_regions(src);
        assert_eq!(arms.get(&addr(0x3000, 0)), Some(&arm_b));
        assert_eq!(arms.get(&addr(0x2000, 0)), Some(&arm_a));
        assert_eq!(arms.get(&addr(0x4000, 0)), Some(&arm_c));
        assert_eq!(
            arms.get(&addr(0x5000, 0)),
            None,
            "a region not wired as a successor of `src` is not an arm"
        );
        let _ = stranger;
    }

    /// A switch target is always a machine-instruction START
    /// (`PcodeInsnAddr::at_machine_start`), so an arm is the successor whose own
    /// start is that address at pcode index 0. A successor starting MID-pcode at
    /// the same machine address (a `CondBranch` into a pcode sequence) is a
    /// different region and must not answer for it.
    #[test]
    fn switch_arm_regions_does_not_key_a_mid_pcode_successor_at_the_machine_start() {
        let mut graph: StableDiGraph<Region, ()> = StableDiGraph::new();
        let src = graph.add_node(make_region(&[(0x1000, 0)]));
        let mid_pcode = graph.add_node(make_region(&[(0x2000, 3)]));
        graph.add_edge(src, mid_pcode, ());

        let cfg = Cfg {
            region_graph: graph,
            entry: src,
            undecodable_seeded: Vec::new(),
            isa_mode_conflicts: Vec::new(),
            interior_branch_targets: Vec::new(),
            link_register_seated: Vec::new(),
            tail_call_seated: Vec::new(),
            function_isa_bit: None,
        };

        assert_eq!(
            cfg.switch_arm_regions(src).get(&addr(0x2000, 0)),
            None,
            "no successor starts at 0x2000's pcode index 0, so the site has no arm"
        );
        let _ = mid_pcode;
    }
}
