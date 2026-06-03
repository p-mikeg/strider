use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;

use super::types::{Region, RegionTerminator};
use super::{Cfg, RegionId};
use anyhow::anyhow;

use crate::cfg::Result;

/// Decides whether `target` is a tail call — i.e. lies outside the
/// half-open function range `[start_addr, start_addr + fn_max_size)`.
///
/// Shared by `crate::cfg::Builder::is_branch_tail_call_nocheck` (cfg-time
/// classification) and `strider`'s orchestrator (post-cfg `Single(K)`
/// resolution).  Both layers must agree on the predicate.
///
/// `allow_code_before_start_addr = true` disables the lower-bound check
/// **only when `fn_max_size` is `None`** (relevant for binaries whose
/// function bodies legitimately reach back into the prelude / unwind
/// area, in the unbounded case).  When `fn_max_size` is set, the
/// function's extent is known exactly as `[start_addr, start_addr +
/// fn_max_size)`, so any `target < start_addr` lands in a *different*
/// function and is classified as a tail call regardless of the flag.
pub fn is_addr_tail_call(
    target: u64,
    start_addr: u64,
    fn_max_size: Option<u64>,
    allow_code_before_start_addr: bool,
) -> bool {
    // Compute lower / upper bounds once, then test membership in the
    // half-open `[lower, upper)` window.  `lower == 0` disables the
    // lower-bound check (caller permits code before start_addr in the
    // unbounded case); `upper = None` disables the upper-bound check
    // (caller didn't supply a function size).
    let lower_bound_strict = fn_max_size.is_some() || !allow_code_before_start_addr;
    let lower = if lower_bound_strict { start_addr } else { 0 };
    if target < lower {
        return true;
    }
    if let Some(sz) = fn_max_size {
        let upper = start_addr.saturating_add(sz);
        if target >= upper {
            return true;
        }
    }
    false
}

/// The two successors of a conditional-branch region.
///
/// Returned by [`Cfg::region_if`].
pub struct IfRegionSuccessors {
    /// Region reached when the branch condition is *true*, if present.
    pub if_true_region: Option<NodeIndex>,
    /// Region reached when the branch condition is *false* (fall-through), if present.
    pub if_false_region: Option<NodeIndex>,
}

impl Cfg {
    /// Returns the sole outgoing successor of `region_id`, or `None` when the
    /// region has no successor.
    ///
    /// Meaningful for `Unconditional` regions, which have exactly one
    /// outgoing edge.  A `CondBranch` (two successors) or `Switch` (many) is a
    /// caller error here — use [`Cfg::region_if`] or the `Switch` terminator's
    /// `targets` instead.
    ///
    /// # Errors
    /// Returns an error when more than one outgoing edge leaves `region_id`.
    pub fn region_successor(&self, region_id: RegionId) -> Result<Option<NodeIndex>> {
        let mut found: Option<NodeIndex> = None;
        for edge in self.region_graph.edges_directed(region_id, petgraph::Outgoing) {
            if found.is_some() {
                return Err(anyhow!("region {region_id:?} has more than one outgoing edge"));
            }
            found = Some(edge.target());
        }
        Ok(found)
    }

    /// Returns both conditional-branch successors of `region_id`.
    ///
    /// The region's [`RegionTerminator::CondBranch`] records the taken
    /// successor's address in `true_target`.  This walks the (unweighted)
    /// outgoing edges and reports the one whose target region **contains**
    /// `true_target` as `if_true_region`, the other as `if_false_region`.
    /// When both arms target the same region (a degenerate `if (c) goto L`
    /// else `goto L`), both fields hold that region.  For a non-`CondBranch`
    /// region, both fields are `None`.
    ///
    /// Containment (not start-address equality) is the right test: a region's
    /// `start_addr` can sit *below* its first instruction when a branch target
    /// lands in a zero-pcode-op hole and `split_region` rounds the start down,
    /// so the branch's `true_target` may be that region's first instruction
    /// rather than its `start_addr`.  [`Region::contains_addr`] handles both.
    ///
    /// # Errors
    /// Returns an error when `region_id` or one of its edge targets is missing
    /// from the graph (a construction bug).
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
        for edge in self.region_graph.edges_directed(region_id, petgraph::Outgoing) {
            let target = edge.target();
            let contains_taken = self
                .region_graph
                .node_weight(target)
                .ok_or_else(|| {
                    anyhow!("dangling edge target {target:?} from region {region_id:?}")
                })?
                .contains_addr(true_target);
            // The first edge whose target contains `true_target` is the taken
            // side; the remaining edge is the fall-through.  Guarding on
            // `if_true_region.is_none()` keeps the degenerate both-arms-same-
            // region case sane (the second edge falls to `if_false_region`).
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

    /// Iterates over all [`Region`]s in the CFG (unordered).
    pub fn regions(&self) -> impl Iterator<Item = &Region> {
        self.region_graph.node_weights()
    }

    /// Iterates over the [`RegionId`] of every region in the CFG (unordered).
    pub fn region_ids(&self) -> impl Iterator<Item = RegionId> {
        self.region_graph.node_indices()
    }

    /// Returns the `RegionId` of the region whose **start machine
    /// address** equals `addr`, or `None` if no such region exists.
    ///
    /// Content-keyed lookup that is stable across CFG rebuilds (same
    /// machine address always produces the same key).  Used by the
    /// indirect-branch resolver and by `strider`'s switch handler to
    /// correlate a machine address with the region that owns it.
    ///
    /// CORRECTNESS: only matches regions whose `start_addr.machine_addr`
    /// equals `addr` exactly.  Mid-region matches return `None` — the
    /// caller is interested in the canonical region whose lift would
    /// populate the cache entry, which is the region that *starts* at
    /// `addr`.  After a `split_region` event, the second-half region's
    /// start is a different machine address (the split point), so this
    /// lookup transparently distinguishes pre- and post-split halves.
    pub fn region_id_at_start(&self, addr: super::types::MachineInsnAddr) -> Option<RegionId> {
        // O(log R) range query instead of an O(R) graph scan: locate the
        // greatest start_addr ≤ (addr, pcode=u64::MAX), then verify it
        // matches the requested machine address exactly.  The BTreeMap
        // was promoted from the Builder at construction time.
        use std::collections::Bound;
        let lower = super::types::PcodeInsnAddr {
            machine_addr: addr,
            insn_index: 0,
        };
        let upper = super::types::PcodeInsnAddr {
            machine_addr: addr,
            insn_index: u64::MAX,
        };
        let mut range = self
            .start_addr_to_region_id
            .range((Bound::Included(lower), Bound::Included(upper)));
        let (_, &rid) = range.next()?;
        Some(rid)
    }
}

#[cfg(test)]
mod tests {
    //! Tests for `Cfg`'s query API: `region_if`, `region_successor`,
    //! `regions`, `region_ids`, `region_id_at_start`, and the
    //! DuplicateEdgeKind error path through `region_successor` / `region_if`.
    //!
    //! Ported from pre-rewrite `crates/cfg/tests/cfg_query.rs`.  The
    //! malformed-CFG tests live inline so they can populate the
    //! `pub(crate) start_addr_to_region_id` field directly.

    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::collections::BTreeMap;

    use petgraph::stable_graph::StableDiGraph;
    use strider_target::SleighArch;

    use super::*;
    use crate::cfg::types::{MachineInsnAddr, PcodeInsnAddr, Region, RegionInstruction};
    use crate::cfg::{Builder, OptionsBuilder};

    // ── synthetic helpers ────────────────────────────────────────────────

    fn addr(machine: u64, insn: u64) -> PcodeInsnAddr {
        PcodeInsnAddr {
            machine_addr: MachineInsnAddr { addr: machine },
            insn_index: insn,
        }
    }

    fn fake_insn() -> rsleigh::Insn {
        rsleigh::Insn {
            opcode: rsleigh::Opcode::Copy,
            output: None,
            inputs: vec![].into(),
        }
    }

    fn make_region(addrs: &[(u64, u64)]) -> Region {
        let start = addr(addrs[0].0, addrs[0].1);
        let insns = addrs
            .iter()
            .map(|&(m, i)| RegionInstruction {
                addr: addr(m, i),
                insn: fake_insn(),
            })
            .collect();
        Region {
            start_addr: start,
            insns,
            terminator: crate::cfg::RegionTerminator::Unconditional,
        }
    }

    // ── real-binary helpers ──────────────────────────────────────────────

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
        let mem = strider_reader::ElfFileMemReader::from_object(&obj)
            .expect("ElfFileMemReader::from_object");
        let arch = SleighArch::x86_64();
        let mut sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), mem)
            .expect("create Sleigh");
        let entry_addr = obj
            .symbol_by_name(fn_name)
            .unwrap_or_else(|| panic!("symbol {fn_name:?} not found in {path:?}"))
            .address();
        Builder::for_arch(&arch, &mut sleigh, entry_addr, OptionsBuilder::new().build())
            .build()
            .unwrap_or_else(|e| panic!("Builder::build for {fn_name:?}: {e:?}"))
    }

    // ── regions / region_ids iteration ───────────────────────────────────

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

    // ── region_successor ────────────────────────────────────────────────────

    #[test]
    fn region_successor_returns_none_for_linear_entry() {
        let cfg = real_cfg("arithmetic", "add");
        assert!(cfg.region_successor(cfg.entry).unwrap().is_none());
    }

    // ── region_if ────────────────────────────────────────────────────────

    #[test]
    fn region_if_both_successors_present_on_abs_val() {
        let cfg = real_cfg("control", "abs_val");
        let has_pair = cfg.region_ids().any(|id| {
            let s = cfg.region_if(id).unwrap();
            s.if_true_region.is_some() && s.if_false_region.is_some()
        });
        assert!(has_pair, "abs_val: no region with both if-true and if-false successors");
    }

    #[test]
    fn region_if_absent_on_linear_entry() {
        let cfg = real_cfg("arithmetic", "add");
        let s = cfg.region_if(cfg.entry).unwrap();
        assert!(s.if_true_region.is_none());
        assert!(s.if_false_region.is_none());
    }

    // ── region_successor: multiple-successor error ───────────────────────

    #[test]
    fn multiple_successors_are_detected_by_region_successor() {
        // A region with two outgoing edges is not a single-successor region;
        // region_successor must error rather than silently pick one.
        let mut graph: StableDiGraph<Region, ()> = StableDiGraph::new();
        let src = graph.add_node(make_region(&[(0x1000, 0)]));
        let dst1 = graph.add_node(make_region(&[(0x2000, 0)]));
        let dst2 = graph.add_node(make_region(&[(0x3000, 0)]));
        graph.add_edge(src, dst1, ());
        graph.add_edge(src, dst2, ());

        let cfg = Cfg {
            region_graph: graph,
            entry: src,
            start_addr_to_region_id: BTreeMap::new(),
        };

        let err = cfg.region_successor(src).unwrap_err();
        assert!(
            err.to_string().contains("more than one outgoing edge"),
            "got: {err}"
        );
    }

    // ── region_if: polarity resolved by true_target ──────────────────────

    /// Helper: a `CondBranch` region whose taken successor is `true_target`.
    fn make_cond_region(start_machine: u64, true_target: PcodeInsnAddr) -> Region {
        let mut r = make_region(&[(start_machine, 0)]);
        r.terminator = RegionTerminator::CondBranch { true_target };
        r
    }

    #[test]
    fn region_if_resolves_polarity_by_true_target() {
        // Two distinct successors; the one whose region contains the
        // terminator's true_target is the taken side regardless of edge
        // insertion order.
        let mut graph: StableDiGraph<Region, ()> = StableDiGraph::new();
        let src = graph.add_node(make_cond_region(0x1000, addr(0x3000, 0)));
        let fallthrough = graph.add_node(make_region(&[(0x2000, 0)]));
        let taken = graph.add_node(make_region(&[(0x3000, 0)]));
        // Insert the fall-through edge FIRST to prove order-independence.
        graph.add_edge(src, fallthrough, ());
        graph.add_edge(src, taken, ());

        let cfg = Cfg {
            region_graph: graph,
            entry: src,
            start_addr_to_region_id: BTreeMap::new(),
        };

        let s = cfg.region_if(src).unwrap();
        assert_eq!(s.if_true_region, Some(taken), "taken side = true_target match");
        assert_eq!(s.if_false_region, Some(fallthrough));
    }

    #[test]
    fn region_if_matches_taken_successor_by_containment_not_start() {
        // Regression: a region's `start_addr` can sit below its first
        // instruction (zero-pcode-op hole + rounded `split_region`), so the
        // branch's `true_target` may be an INTERIOR address of the taken
        // successor rather than its `start_addr`.  region_if must match by
        // containment.  Here the taken region spans [0x3000, 0x3010] and the
        // branch targets the interior address 0x3008.
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
        // Degenerate `if (c) goto L else goto L`: both unweighted edges point
        // at one region.  region_if must report it for both sides, not drop
        // the false side.
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

    // ── region_id_at_start ───────────────────────────────────────────────

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
}
