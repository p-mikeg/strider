use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;

use super::types::{Region, RegionEdgeKind};
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
#[must_use]
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
pub struct IfRegionState {
    /// Region reached when the branch condition is *true*, if present.
    pub if_true_region: Option<NodeIndex>,
    /// Region reached when the branch condition is *false* (fall-through), if present.
    pub if_false_region: Option<NodeIndex>,
}

impl<R: rsleigh::MemReader> Cfg<R> {
    /// Returns the sole successor of `region_id` whose edge weight is `kind`,
    /// or `None` if no such edge exists.
    ///
    /// # Errors
    /// Returns an error when more than one outgoing edge of `kind` is
    /// attached to `region_id`.
    fn unique_outgoing(&self, region_id: RegionId, kind: RegionEdgeKind) -> Result<Option<NodeIndex>> {
        let mut found: Option<NodeIndex> = None;
        for edge in self.graph.edges_directed(region_id, petgraph::Outgoing) {
            if *edge.weight() != kind {
                continue;
            }
            if found.is_some() {
                return Err(anyhow!("region {region_id:?} has more than one outgoing edge of kind {kind:?}"));
            }
            found = Some(edge.target());
        }
        Ok(found)
    }

    /// Returns the unconditional-branch successor of `region_id`, if any.
    ///
    /// # Errors
    /// Returns an error when more than one `Branch` edge leaves
    /// `region_id`.
    pub fn region_branch(&self, region_id: RegionId) -> Result<Option<NodeIndex>> {
        self.unique_outgoing(region_id, RegionEdgeKind::Branch)
    }

    /// Returns the fallthrough successor of `region_id`, if any.
    ///
    /// A region's fallthrough edge is its successor on the
    /// `Fallthrough` edge kind — emitted either by sequential decode
    /// reaching a known region OR by the builder reclassifying a
    /// `Branch` whose target was the next machine instruction.
    ///
    /// # Errors
    /// Returns an error when more than one `Fallthrough` edge leaves
    /// `region_id`.
    pub fn region_fallthrough(&self, region_id: RegionId) -> Result<Option<NodeIndex>> {
        self.unique_outgoing(region_id, RegionEdgeKind::Fallthrough)
    }

    /// Returns both conditional-branch successors of `region_id`.
    ///
    /// # Errors
    /// Returns an error when more than one `IfCaseTrue` or `IfCaseFalse`
    /// edge leaves `region_id`.
    pub fn region_if(&self, region_id: RegionId) -> Result<IfRegionState> {
        Ok(IfRegionState {
            if_true_region: self.unique_outgoing(region_id, RegionEdgeKind::IfCaseTrue)?,
            if_false_region: self.unique_outgoing(region_id, RegionEdgeKind::IfCaseFalse)?,
        })
    }

    /// Iterates over all [`Region`]s in the CFG (unordered).
    pub fn regions(&self) -> impl Iterator<Item = &Region> {
        self.graph.node_weights()
    }

    /// Iterates over the [`RegionId`] of every region in the CFG (unordered).
    pub fn region_ids(&self) -> impl Iterator<Item = RegionId> {
        self.graph.node_indices()
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
    #[must_use]
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
    //! Tests for `Cfg`'s query API: `region_if`, `region_branch`,
    //! `regions`, `region_ids`, `region_id_at_start`, and the
    //! DuplicateEdgeKind error path through `region_branch` / `region_if`.
    //!
    //! Ported from pre-rewrite `crates/cfg/tests/cfg_query.rs`.  The
    //! malformed-CFG tests live inline so they can populate the
    //! `pub(crate) start_addr_to_region_id` field directly.

    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::collections::BTreeMap;

    use petgraph::stable_graph::StableDiGraph;
    use rsleigh::mem_readers::BufMemReader;
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
            terminator: crate::cfg::RegionTerminator::Fallthrough,
        }
    }

    fn empty_sleigh() -> rsleigh::Sleigh<BufMemReader<Vec<u8>>> {
        let arch = SleighArch::x86_64();
        let reader = BufMemReader::new(Vec::<u8>::new(), 0x0);
        rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader)
            .expect("create empty Sleigh")
    }

    // ── real-binary helpers ──────────────────────────────────────────────

    fn real_cfg(case: &str, fn_name: &str) -> Cfg<strider_reader::ElfFileMemReader> {
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
        let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), mem)
            .expect("create Sleigh");
        let entry_addr = obj
            .symbol_by_name(fn_name)
            .unwrap_or_else(|| panic!("symbol {fn_name:?} not found in {path:?}"))
            .address();
        Builder::for_arch(&arch, sleigh, entry_addr, OptionsBuilder::new().build())
            .build()
            .unwrap_or_else(|e| panic!("Builder::build for {fn_name:?}: {e:?}"))
    }

    // ── regions / region_ids iteration ───────────────────────────────────

    #[test]
    fn regions_iterator_count_matches_node_count() {
        let cfg = real_cfg("control", "sum_to_n");
        assert_eq!(cfg.regions().count(), cfg.graph.node_count());
    }

    #[test]
    fn region_ids_iterator_count_matches_node_count() {
        let cfg = real_cfg("control", "sum_to_n");
        assert_eq!(cfg.region_ids().count(), cfg.graph.node_count());
    }

    // ── region_branch ────────────────────────────────────────────────────

    #[test]
    fn region_branch_returns_none_for_linear_entry() {
        let cfg = real_cfg("arithmetic", "add");
        assert!(cfg.region_branch(cfg.entry).unwrap().is_none());
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

    // ── DuplicateEdgeKind error ──────────────────────────────────────────

    #[test]
    fn duplicate_edge_kind_is_detected_by_region_branch() {
        // Construct a malformed Cfg with two Branch edges from one node
        // to distinct destinations.  region_branch (via unique_outgoing)
        // must error with "more than one outgoing edge".
        let mut graph: StableDiGraph<Region, RegionEdgeKind> = StableDiGraph::new();
        let src = graph.add_node(make_region(&[(0x1000, 0)]));
        let dst1 = graph.add_node(make_region(&[(0x2000, 0)]));
        let dst2 = graph.add_node(make_region(&[(0x3000, 0)]));
        graph.add_edge(src, dst1, RegionEdgeKind::Branch);
        graph.add_edge(src, dst2, RegionEdgeKind::Branch);

        let cfg = Cfg {
            sleigh: empty_sleigh(),
            graph,
            entry: src,
            start_addr_to_region_id: BTreeMap::new(),
        };

        let err = cfg.region_branch(src).unwrap_err();
        assert!(
            err.to_string().contains("more than one outgoing edge"),
            "got: {err}"
        );
    }

    #[test]
    fn duplicate_if_case_true_is_detected_by_region_if() {
        let mut graph: StableDiGraph<Region, RegionEdgeKind> = StableDiGraph::new();
        let src = graph.add_node(make_region(&[(0x1000, 0)]));
        let dst1 = graph.add_node(make_region(&[(0x2000, 0)]));
        let dst2 = graph.add_node(make_region(&[(0x3000, 0)]));
        graph.add_edge(src, dst1, RegionEdgeKind::IfCaseTrue);
        graph.add_edge(src, dst2, RegionEdgeKind::IfCaseTrue);

        let cfg = Cfg {
            sleigh: empty_sleigh(),
            graph,
            entry: src,
            start_addr_to_region_id: BTreeMap::new(),
        };

        let err = cfg
            .region_if(src)
            .map(|_| ())
            .expect_err("region_if must return DuplicateEdgeKind");
        assert!(
            err.to_string().contains("more than one outgoing edge"),
            "got: {err}"
        );
    }

    // ── region_id_at_start ───────────────────────────────────────────────

    #[test]
    fn region_id_at_start_returns_some_for_real_function_entry() {
        let cfg = real_cfg("arithmetic", "add");
        let entry_region = cfg
            .graph
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
