mod region_builder;
mod split;

use region_builder::RegionBuilder;

use std::collections::BTreeMap;

use petgraph::graph::NodeIndex;

use crate::Cfg;
use crate::options::CfgOptions;
use crate::types::{MachineInsnAddr, PcodeInsnAddr, Region, RegionGraph};
use anyhow::{anyhow, bail};

use crate::Result;

/// Incrementally constructs a [`Cfg`] from a binary entry point.
///
/// A work queue seeded with the entry address drives decoding: each item
/// either decodes a new region or routes an edge to an existing one.  A
/// branch landing mid-region splits that region in two.
///
/// # Usage
/// ```no_run
/// use strider_cfg::Builder;
/// use strider_cfg::CfgOptions;
/// use strider_target::SleighArch;
/// use rsleigh::mem_readers::BufMemReader;
///
/// let fn_addr: u64 = 0x1000;
/// let reader = BufMemReader::new(Vec::<u8>::new(), fn_addr);
/// let mut sleigh = rsleigh::Sleigh::new(
///     rsleigh::sla_spec::SLA_SPEC_X86_64,
///     rsleigh::pspec::PSPEC_X86_64,
///     reader,
/// ).expect("create Sleigh");
/// let opts = CfgOptions::default();
/// let arch = SleighArch::x86_64();
/// let cfg = Builder::for_arch(&arch, &mut sleigh, fn_addr, &opts).build()?;
/// // `sleigh` is still owned + usable here (the builder only borrowed it).
/// # Ok::<(), anyhow::Error>(())
/// ```
pub struct Builder<'a, R: rsleigh::MemReader> {
    pub(super) sleigh: &'a mut rsleigh::Sleigh<R>,
    pub(super) start_addr: MachineInsnAddr,
    pub(super) options: CfgOptions,
    pub(super) arch: strider_target::SleighArch,
    pub(super) region_graph: RegionGraph,
    pub(super) start_addr_to_region_id: BTreeMap<PcodeInsnAddr, NodeIndex>,
    /// LIFO, so exploration is depth-first.
    pub(super) work_queue: Vec<(Option<NodeIndex>, PcodeInsnAddr)>,
    /// CC overrides for CALL TARGETS, keyed by target machine address.  Only
    /// `no_return` is read here.
    pub(super) per_address_ccs: rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>,
    /// Snapshotted once at construction and indexed by `user_op_id`.  Empty
    /// when the Sleigh reports no user ops or the snapshot fails.
    pub(super) user_op_names: Vec<String>,
}

impl<'a, R: rsleigh::MemReader> Builder<'a, R> {
    /// The canonical constructor: endianness and `ArchPreset` are derived
    /// atomically from `arch`.
    pub fn for_arch(
        arch: &strider_target::SleighArch,
        sleigh: &'a mut rsleigh::Sleigh<R>,
        start_addr: u64,
        options: &CfgOptions,
    ) -> Self {
        // Read `Some(0)` as unbounded, not as a zero-length function.
        // Callers reject zero at their own API boundary; one arriving here
        // is treated as a no-op rather than pinning the lifter at
        // `start_addr`.
        let mut options = options.clone();
        if options.fn_max_size == Some(0) {
            options.fn_max_size = None;
        }
        // A snapshot failure degrades to "no names", leaving CallOthers
        // unclassified rather than aborting CFG construction.
        let user_op_names = sleigh.user_op_names().unwrap_or_default();
        Self {
            sleigh,
            start_addr: start_addr.into(),
            options,
            arch: *arch,
            region_graph: RegionGraph::new(),
            start_addr_to_region_id: BTreeMap::new(),
            work_queue: Vec::new(),
            per_address_ccs: rustc_hash::FxHashMap::default(),
            user_op_names,
        }
    }

    #[must_use]
    pub fn with_per_address_ccs(
        mut self,
        per_address_ccs: rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>,
    ) -> Self {
        self.per_address_ccs = per_address_ccs;
        self
    }

    /// An empty region is rejected unless its terminator is `Unconditional`
    /// or `TailCall`.  Those two empty shapes are deliberate:
    ///
    /// - `Unconditional`: a single-instruction `jmp <in-range>` whose trailing
    ///   branch opcode was popped.
    /// - `TailCall`: the stub [`Self::tail_call_stub`] builds for a CondBranch
    ///   arm leaving the function bound.  Nothing outside the bound is ever
    ///   decoded, so the stub has no body by construction.
    pub(super) fn add_region(&mut self, region: Region) -> Result<NodeIndex> {
        if region.insns.is_empty()
            && !matches!(
                region.terminator,
                super::types::RegionTerminator::Unconditional
                    | super::types::RegionTerminator::TailCall { .. }
            )
        {
            bail!(
                "region at {:?} has no instructions and terminator is {:?} (only Unconditional or TailCall is permitted for empty regions)",
                region.start_addr,
                region.terminator,
            );
        }

        let start_addr = region.start_addr;
        let region_id = self.region_graph.add_node(region);
        self.start_addr_to_region_id.insert(start_addr, region_id);
        Ok(region_id)
    }

    /// Lowers the out-of-function arm of a conditional branch, creating the
    /// stub on first use.  It is wired as a regular CondBranch successor but
    /// never enqueued, so no byte outside `[start, start + fn_max_size)` is
    /// decoded.
    ///
    /// Keyed through `start_addr_to_region_id` like any region, so two
    /// branches to the same OOB address share one stub.
    pub(super) fn tail_call_stub(&mut self, addr: PcodeInsnAddr) -> Result<NodeIndex> {
        if let Some(&existing) = self.start_addr_to_region_id.get(&addr) {
            return Ok(existing);
        }
        self.add_region(Region {
            start_addr: addr,
            insns: Vec::new(),
            terminator: super::types::RegionTerminator::TailCall {
                target: addr.machine_addr.addr,
            },
        })
    }

    /// Range-queries for the last region starting at or below `addr`, then
    /// confirms containment: a `start_addr <= addr` hit alone does not mean
    /// the address is inside that region.
    pub(super) fn find_region_containing_addr(
        &self,
        addr: PcodeInsnAddr,
    ) -> Option<(NodeIndex, &Region)> {
        let (_, &region_id) = self.start_addr_to_region_id.range(..=addr).next_back()?;

        let region = self.region_graph.node_weight(region_id)?;
        region.contains_addr(addr).then_some((region_id, region))
    }

    fn start_pcode_addr(&self) -> PcodeInsnAddr {
        PcodeInsnAddr {
            machine_addr: self.start_addr,
            insn_index: 0,
        }
    }

    /// Routes `addr` to an existing region (splitting it when `addr` lands in
    /// its interior) or decodes a new one.
    fn explore(&mut self, parent_region: Option<NodeIndex>, addr: PcodeInsnAddr) -> Result<()> {
        let existing_region = self.find_region_containing_addr(addr);
        if let Some((region_id, region)) = existing_region {
            let parent_region_id = parent_region
                .ok_or_else(|| anyhow!("non-entry work-queue item has no parent edge"))?;
            if region.start_addr == addr {
                self.region_graph.add_edge(parent_region_id, region_id, ());
            } else {
                let second_region = self.split_region(region_id, addr)?;
                self.region_graph
                    .add_edge(parent_region_id, second_region, ());
            }
        } else {
            RegionBuilder::new(self, addr, parent_region).build()?;
        }
        Ok(())
    }

    /// The returned `Cfg` is pure data and does not own the Sleigh.
    pub fn build(mut self) -> Result<Cfg> {
        self.work_queue.push((None, self.start_pcode_addr()));
        while let Some((parent_region, address)) = self.work_queue.pop() {
            self.explore(parent_region, address)?;
        }
        let start_addr = self.start_pcode_addr();
        let (starting_region, _) = self.find_region_containing_addr(start_addr).ok_or_else(
            || {
                anyhow!(
                    "cfg build completed but no region contains the entry address {start_addr:?}; \
                     check that the entry is decodable"
                )
            },
        )?;

        Ok(Cfg {
            region_graph: self.region_graph,
            entry: starting_region,
            start_addr_to_region_id: self.start_addr_to_region_id,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use petgraph::visit::{EdgeRef, IntoEdgeReferences};

    use crate::test_support::*;
    use crate::types::{Region, RegionTerminator};

    #[test]
    fn add_region_inserts_into_graph_and_map() {
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let r = make_region(&[(0x1000, 0), (0x1004, 0)]);
        let id = b.add_region(r).unwrap();

        assert!(b.region_graph.node_weight(id).is_some());
        assert_eq!(b.start_addr_to_region_id.get(&addr(0x1000, 0)), Some(&id));
    }

    #[test]
    fn add_region_empty_region_with_disallowed_terminator_returns_error() {
        // Any other terminator on an empty region is a construction bug.
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let empty = Region {
            start_addr: addr(0x1000, 0),
            insns: Vec::new(),
            terminator: RegionTerminator::Return,
        };
        let err = b.add_region(empty).unwrap_err();
        assert!(
            err.to_string().contains("has no instructions"),
            "got: {err}"
        );
    }

    #[test]
    fn add_region_empty_unconditional_is_allowed() {
        // `finish_branch_or_tail_call` pops the trailing branch opcode, so a
        // single-instruction `jmp <in-range>` arrives here empty.
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let empty = Region {
            start_addr: addr(0x1000, 0),
            insns: Vec::new(),
            terminator: RegionTerminator::Unconditional,
        };
        b.add_region(empty)
            .expect("empty Unconditional region is allowed");
    }

    #[test]
    fn add_region_empty_tail_call_is_allowed() {
        // The stub for a CondBranch arm leaving the function bound.
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let empty = Region {
            start_addr: addr(0x9000, 0),
            insns: Vec::new(),
            terminator: RegionTerminator::TailCall { target: 0x9000 },
        };
        b.add_region(empty)
            .expect("empty TailCall stub region is allowed");
    }

    #[test]
    fn add_region_two_regions_both_present_with_distinct_indices() {
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let r1 = make_region(&[(0x1000, 0)]);
        let r2 = make_region(&[(0x1010, 0)]);
        let id1 = b.add_region(r1).unwrap();
        let id2 = b.add_region(r2).unwrap();

        assert_ne!(id1, id2);
        assert_eq!(b.region_graph.node_count(), 2);
        assert_eq!(b.start_addr_to_region_id[&addr(0x1000, 0)], id1);
        assert_eq!(b.start_addr_to_region_id[&addr(0x1010, 0)], id2);
    }

    #[test]
    fn find_region_empty_graph_returns_none() {
        let mut sleigh = make_sleigh();
        let b = make_builder(0x1000, &mut sleigh);
        assert!(b.find_region_containing_addr(addr(0x1000, 0)).is_none());
    }

    #[test]
    fn find_region_at_start_addr() {
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let id = b
            .add_region(make_region(&[(0x1000, 0), (0x100f, 0)]))
            .unwrap();
        assert_eq!(
            b.find_region_containing_addr(addr(0x1000, 0))
                .map(|(i, _)| i),
            Some(id)
        );
    }

    #[test]
    fn find_region_at_interior_addr() {
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let id = b
            .add_region(make_region(&[(0x1000, 0), (0x100f, 0)]))
            .unwrap();
        assert_eq!(
            b.find_region_containing_addr(addr(0x1008, 0))
                .map(|(i, _)| i),
            Some(id)
        );
    }

    #[test]
    fn find_region_at_last_insn() {
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let id = b
            .add_region(make_region(&[(0x1000, 0), (0x100f, 0)]))
            .unwrap();
        assert_eq!(
            b.find_region_containing_addr(addr(0x100f, 0))
                .map(|(i, _)| i),
            Some(id)
        );
    }

    #[test]
    fn find_region_beyond_end_returns_none() {
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        b.add_region(make_region(&[(0x1000, 0), (0x100f, 0)]))
            .unwrap();
        assert!(b.find_region_containing_addr(addr(0x1020, 0)).is_none());
    }

    #[test]
    fn find_region_two_adjacent_regions_route_correctly() {
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let id1 = b
            .add_region(make_region(&[(0x1000, 0), (0x100f, 0)]))
            .unwrap();
        let id2 = b
            .add_region(make_region(&[(0x1010, 0), (0x1020, 0)]))
            .unwrap();

        assert_eq!(
            b.find_region_containing_addr(addr(0x1004, 0))
                .map(|(i, _)| i),
            Some(id1)
        );
        assert_eq!(
            b.find_region_containing_addr(addr(0x1010, 0))
                .map(|(i, _)| i),
            Some(id2)
        );
        assert_eq!(
            b.find_region_containing_addr(addr(0x1018, 0))
                .map(|(i, _)| i),
            Some(id2)
        );
    }

    #[test]
    fn split_at_start_is_noop() {
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let id = b
            .add_region(make_region(&[(0x1000, 0), (0x1004, 0), (0x1008, 0)]))
            .unwrap();
        let edges_before = b.region_graph.edge_references().count();
        let map_len_before = b.start_addr_to_region_id.len();

        let result = b.split_region(id, addr(0x1000, 0)).unwrap();
        assert_eq!(result, id);
        assert_eq!(b.region_graph.node_count(), 1);
        assert_eq!(b.region_graph.edge_references().count(), edges_before);
        assert_eq!(b.start_addr_to_region_id.len(), map_len_before);
    }

    #[test]
    fn split_creates_two_regions_second_keeps_original_id() {
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let original = b
            .add_region(make_region(&[
                (0x1000, 0),
                (0x1004, 0),
                (0x1008, 0),
                (0x100c, 0),
            ]))
            .unwrap();
        let second = b.split_region(original, addr(0x1008, 0)).unwrap();
        assert_eq!(second, original, "second half retains original NodeIndex");
        assert_eq!(b.region_graph.node_count(), 2);
    }

    #[test]
    fn split_second_half_is_always_non_empty() {
        // Pins what `split_region`'s `debug_assert!` defends: a real split
        // must leave the second half (which keeps `region_id` and the
        // original terminator) non-empty, so it never silently bypasses
        // `add_region`'s empty-region guard.
        for split_at in [0x1004u64, 0x1008, 0x100c] {
            let mut sleigh = make_sleigh();
            let mut b = make_builder(0x1000, &mut sleigh);
            let original = b
                .add_region(make_region(&[
                    (0x1000, 0),
                    (0x1004, 0),
                    (0x1008, 0),
                    (0x100c, 0),
                ]))
                .unwrap();
            let second = b.split_region(original, addr(split_at, 0)).unwrap();
            assert_eq!(second, original);
            assert!(
                !b.region_graph[second].insns.is_empty(),
                "second half empty after split at {split_at:#x}"
            );
        }
    }

    #[test]
    fn split_produces_correct_addr_ranges() {
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let original = b
            .add_region(make_region(&[
                (0x1000, 0),
                (0x1004, 0),
                (0x1008, 0),
                (0x100c, 0),
            ]))
            .unwrap();
        b.split_region(original, addr(0x1008, 0)).unwrap();

        assert_eq!(b.region_graph[original].start_addr, addr(0x1008, 0));
        assert_eq!(b.region_graph[original].insns.len(), 2);

        let first_id = b.start_addr_to_region_id[&addr(0x1000, 0)];
        assert_eq!(b.region_graph[first_id].start_addr, addr(0x1000, 0));
        assert_eq!(b.region_graph[first_id].insns.len(), 2);
    }

    #[test]
    fn split_adds_fallthrough_edge() {
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let original = b
            .add_region(make_region(&[(0x1000, 0), (0x1004, 0), (0x1008, 0)]))
            .unwrap();
        b.split_region(original, addr(0x1008, 0)).unwrap();

        let edges: Vec<_> = b.region_graph.edge_references().collect();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target(), original);
    }

    #[test]
    fn split_rewires_incoming_edges_to_first_half() {
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let a = b.add_region(make_region(&[(0x0ff0, 0)])).unwrap();
        let b_id = b
            .add_region(make_region(&[(0x1000, 0), (0x1004, 0), (0x1008, 0)]))
            .unwrap();
        b.region_graph.add_edge(a, b_id, ());

        b.split_region(b_id, addr(0x1004, 0)).unwrap();

        let first = b.start_addr_to_region_id[&addr(0x1000, 0)];
        let incoming: Vec<_> = b
            .region_graph
            .edges_directed(first, petgraph::Incoming)
            .collect();
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].source(), a);

        // The original `a -> b_id` edge was rewired to `a -> first`, leaving
        // the second half only the split's own fall-through from `first`.
        let second_incoming: Vec<_> = b
            .region_graph
            .edges_directed(b_id, petgraph::Incoming)
            .collect();
        assert_eq!(second_incoming.len(), 1);
        assert_eq!(second_incoming[0].source(), first);
    }

    #[test]
    fn split_addr_in_zero_pcode_hole_rounds_down_to_largest_le() {
        // 0x1008 falls in the hole between 0x1004 and 0x100c, mirroring the
        // AArch64 PAC zero-pcode-op case.
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let original = b
            .add_region(make_region(&[(0x1000, 0), (0x1004, 0), (0x100c, 0)]))
            .unwrap();
        let second = b.split_region(original, addr(0x1008, 0)).unwrap();
        assert_eq!(second, original);

        assert_eq!(b.region_graph[original].start_addr, addr(0x1008, 0));
        assert_eq!(b.region_graph[original].insns.len(), 1);
        assert_eq!(b.region_graph[original].insns[0].addr, addr(0x100c, 0));

        let first_id = b.start_addr_to_region_id[&addr(0x1000, 0)];
        assert_eq!(b.region_graph[first_id].insns.len(), 2);
        assert_eq!(
            b.region_graph[first_id].insns.last().unwrap().addr,
            addr(0x1004, 0)
        );

        assert_eq!(b.start_addr_to_region_id[&addr(0x1008, 0)], original);
    }

    #[test]
    fn split_addr_in_phantom_start_span_is_noop() {
        // After a hole round-down the second region's start_addr (0x1008)
        // sits below its first surviving insn (0x100c).  A branch into the
        // phantom span [0x1008, 0x100c) must be a no-op split, NOT a hard
        // CFG-build error.
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let original = b
            .add_region(make_region(&[(0x1000, 0), (0x1004, 0), (0x100c, 0)]))
            .unwrap();
        let second = b.split_region(original, addr(0x1008, 0)).unwrap();
        assert_eq!(second, original);
        assert_eq!(b.region_graph[second].start_addr, addr(0x1008, 0));

        // 0x100a is below the only insn (0x100c) but >= start_addr.
        let again = b.split_region(second, addr(0x100a, 0)).unwrap();
        assert_eq!(again, second, "phantom-span split must be a no-op");
        assert_eq!(b.region_graph[second].insns.len(), 1);
        assert_eq!(b.region_graph[second].insns[0].addr, addr(0x100c, 0));
    }

    #[test]
    fn split_addr_below_every_insn_returns_error() {
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let id = b
            .add_region(make_region(&[(0x1000, 0), (0x1010, 0)]))
            .unwrap();
        let err = b.split_region(id, addr(0x0ff0, 0)).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not found"), "got: {msg}");
    }
}
