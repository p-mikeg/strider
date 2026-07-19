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
    /// Containment, not start-address equality, is the correct test: a
    /// region's `start_addr` can sit BELOW its first instruction once a target
    /// in a zero-pcode-op hole makes `split_region` round down, so
    /// `true_target` may be the first instruction instead.
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

    /// Content-keyed and therefore stable across CFG rebuilds, unlike a
    /// `NodeIndex`.
    ///
    /// A region entry is EITHER the exact `start_addr.machine_addr` OR the
    /// first materialised instruction's address.  Those differ when the entry
    /// machine insn lifts to zero pcode ops (alignment `nop` / `pause` /
    /// `endbr64` / `paciasp`): the builder keys the region at the zero-op
    /// address, but a branch or switch TARGET lands on the first real
    /// instruction, which is equally a valid entry.
    ///
    /// Genuine interior addresses still return `None`; they signal a missing
    /// `split_region`.
    pub fn region_id_at_start(&self, addr: super::types::MachineInsnAddr) -> Option<RegionId> {
        // O(log R) range query rather than an O(R) graph scan: the greatest
        // start_addr at or below (addr, u64::MAX) is the only region that
        // could own `addr` as its entry, so confirm against just that one.
        let upper = super::types::PcodeInsnAddr {
            machine_addr: addr,
            insn_index: u64::MAX,
        };
        let (_, &rid) = self.start_addr_to_region_id.range(..=upper).next_back()?;
        let region = self.region_graph.node_weight(rid)?;
        let is_entry = region.start_addr.machine_addr == addr
            || region
                .insns
                .first()
                .is_some_and(|i| i.addr.machine_addr == addr);
        is_entry.then_some(rid)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::collections::BTreeMap;

    use petgraph::stable_graph::StableDiGraph;
    use strider_target::SleighArch;

    use super::*;
    use crate::test_support::*;
    use crate::types::{MachineInsnAddr, PcodeInsnAddr, Region, RegionInstruction};
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
            start_addr_to_region_id: BTreeMap::new(),
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
        // A region's `start_addr` can sit below its first instruction after a
        // zero-pcode-op hole rounds `split_region` down, so `true_target` may
        // be an INTERIOR address of the taken successor.  Here that region
        // spans [0x3000, 0x3010] and the branch targets 0x3008.
        let mut graph: StableDiGraph<Region, ()> = StableDiGraph::new();
        let src = graph.add_node(make_cond_region(0x1000, addr(0x3008, 0)));
        let fallthrough = graph.add_node(make_region(&[(0x2000, 0)]));
        let taken = graph.add_node(make_region(&[(0x3000, 0), (0x3010, 0)]));
        graph.add_edge(src, fallthrough, ());
        graph.add_edge(src, taken, ());

        let cfg = Cfg {
            region_graph: graph,
            entry: src,
            start_addr_to_region_id: BTreeMap::new(),
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
            start_addr_to_region_id: BTreeMap::new(),
        };

        let s = cfg.region_if(src).unwrap();
        assert_eq!(s.if_true_region, Some(both));
        assert_eq!(s.if_false_region, Some(both));
    }

    #[test]
    fn region_id_at_start_returns_some_for_real_function_entry() {
        let cfg = real_cfg("arithmetic", "add");
        let entry_region = cfg
            .region_graph
            .node_weight(cfg.entry)
            .expect("entry region exists");
        let entry_addr = entry_region.start_addr.machine_addr;
        let rid = cfg.region_id_at_start(entry_addr);
        assert_eq!(
            rid,
            Some(cfg.entry),
            "region_id_at_start must locate the entry region by its start addr"
        );
    }

    #[test]
    fn region_id_at_start_returns_none_for_unknown_machine_addr() {
        let cfg = real_cfg("arithmetic", "add");
        let rid = cfg.region_id_at_start(MachineInsnAddr { addr: 0xdead_beef });
        assert!(rid.is_none(), "unknown addr must return None, got {rid:?}");
    }

    /// From an AcpiDsLoad2EndOp switch target: when the entry machine insn
    /// lifts to zero pcode ops, a region's `start_addr` sits below its first
    /// materialised instruction, and BOTH must resolve as entries since a
    /// jump-table case label lands on the latter.  An address strictly between
    /// them is not an entry and would signal a missing split.
    #[test]
    fn region_id_at_start_accepts_first_insn_of_phantom_span_region() {
        let mut graph: StableDiGraph<Region, ()> = StableDiGraph::new();
        // 0x1000 is the zero-pcode entry insn; 0x1004 the first real one.
        let region = Region {
            start_addr: addr(0x1000, 0),
            insns: vec![RegionInstruction {
                addr: addr(0x1004, 0),
                insn: fake_insn(),
            }],
            terminator: crate::RegionTerminator::Unconditional,
        };
        let rid = graph.add_node(region);
        let mut start_map = BTreeMap::new();
        start_map.insert(addr(0x1000, 0), rid);
        let cfg = Cfg {
            region_graph: graph,
            entry: rid,
            start_addr_to_region_id: start_map,
        };

        assert_eq!(
            cfg.region_id_at_start(MachineInsnAddr { addr: 0x1000 }),
            Some(rid),
            "exact start_addr resolves"
        );
        assert_eq!(
            cfg.region_id_at_start(MachineInsnAddr { addr: 0x1004 }),
            Some(rid),
            "first materialised instruction (phantom-span head) resolves"
        );
        assert!(
            cfg.region_id_at_start(MachineInsnAddr { addr: 0x1002 })
                .is_none(),
            "an address between start and first insn is not a region entry"
        );
        assert!(
            cfg.region_id_at_start(MachineInsnAddr { addr: 0x1008 })
                .is_none(),
            "an address past the region is unknown"
        );
    }
}
