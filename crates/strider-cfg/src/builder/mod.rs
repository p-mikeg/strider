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
/// The builder uses a work-queue that is seeded with the entry address.
/// Items are popped one at a time; each item triggers decoding of a new
/// region (via `RegionBuilder`) or routing of an edge to an existing
/// region.  When a branch target lands in the middle of an already-decoded
/// region, that region is split in two.
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
///
/// See `crates/strider-lift/tests/cfg_build_end_to_end.rs` for runnable
/// end-to-end examples.
pub struct Builder<'a, R: rsleigh::MemReader> {
    pub(super) sleigh: &'a mut rsleigh::Sleigh<R>,
    /// Virtual address at which the function entry point begins.
    pub(super) start_addr: MachineInsnAddr,
    pub(super) options: CfgOptions,
    /// Target architecture.  Carries both endianness and the
    /// [`strider_target::ArchPreset`] discriminator consulted by
    /// [`super::region_builder::RegionBuilder`]'s `Opcode::CallOther`
    /// arm to pass the right preset to
    /// [`strider_target::call_other_abi::classify`].  `SleighArch` is
    /// `Copy + Eq`, so carrying the whole arch avoids the silent
    /// misclassification a split endianness/preset ctor would invite
    /// (e.g. a big-endian binary decoded as LE, or AArch64 `brk`
    /// classified as the x86 stub) without losing ergonomics.
    pub(super) arch: strider_target::SleighArch,
    /// The region graph being constructed.
    pub(super) region_graph: RegionGraph,
    /// Maps each region's `start_addr` to its [`NodeIndex`].
    /// Used by `find_region_containing_addr` and `split_region`.
    pub(super) start_addr_to_region_id: BTreeMap<PcodeInsnAddr, NodeIndex>,
    /// Pending addresses to explore, together with the parent edge they
    /// should connect from. Treated as a LIFO stack (depth-first traversal).
    pub(super) work_queue: Vec<(Option<NodeIndex>, PcodeInsnAddr)>,
    /// Per-address calling-convention overrides for CALL TARGETS, keyed by
    /// target machine address.  The CFG builder consults only one attribute:
    /// [`strider_target::BuiltCallingConvention::no_return`].  A direct call to
    /// a target flagged `no_return` terminates the calling region `NoReturn`
    /// (→ `Call + Unreachable`) regardless of where the return address lands,
    /// so a mid-function no-return call correctly kills its fall-through.
    /// Empty by default (the function-end structural fallback still applies);
    /// the orchestrator seeds it from the lifter options via
    /// [`Self::with_per_address_ccs`].
    pub(super) per_address_ccs:
        rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>,
}

impl<'a, R: rsleigh::MemReader> Builder<'a, R> {
    /// Creates a new `Builder` whose endianness AND `ArchPreset` are
    /// derived atomically from `arch` (which is stored in full).  The
    /// canonical constructor for CFG building — carrying the whole
    /// `SleighArch` (which is `Copy + Eq`) prevents the silent
    /// misclassification that a split endianness/preset ctor would
    /// invite (e.g. a big-endian binary decoded as LE, or AArch64 `brk`
    /// classified as the x86 stub).  The Sleigh is borrowed mutably
    /// (`lift_one(&mut self)` is stateful), not owned — the caller
    /// retains ownership and can reuse it after `build()` returns.
    pub fn for_arch(
        arch: &strider_target::SleighArch,
        sleigh: &'a mut rsleigh::Sleigh<R>,
        start_addr: u64,
        options: &CfgOptions,
    ) -> Self {
        // `Some(0)` means "unbounded" (no effect) rather than a
        // zero-length function — downstream callers reject zero at their
        // own API boundary, but a zero reaching this far is a defensive
        // no-op so the lifter doesn't decode past `start_addr`.
        let mut options = options.clone();
        if options.fn_max_size == Some(0) {
            options.fn_max_size = None;
        }
        Self {
            sleigh,
            start_addr: start_addr.into(),
            options,
            arch: *arch,
            region_graph: RegionGraph::new(),
            start_addr_to_region_id: BTreeMap::new(),
            work_queue: Vec::new(),
            per_address_ccs: rustc_hash::FxHashMap::default(),
        }
    }

    /// Seeds the per-address call-target CC overrides (see
    /// `Self::per_address_ccs`).  Called by the orchestrator with the same
    /// map it hands the lifter, so a `no_return`-flagged target terminates its
    /// calling region during CFG construction.  Builder-style; returns `self`.
    #[must_use]
    pub fn with_per_address_ccs(
        mut self,
        per_address_ccs: rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>,
    ) -> Self {
        self.per_address_ccs = per_address_ccs;
        self
    }

    /// Inserts `region` into the graph and records its start address in the
    /// lookup map.  Returns the assigned [`NodeIndex`].
    ///
    /// # Errors
    /// Returns an error when `region.insns` is empty AND `region.terminator`
    /// is neither [`super::types::RegionTerminator::Unconditional`] nor
    /// [`super::types::RegionTerminator::TailCall`].  The two allowed empty
    /// shapes are deliberate:
    /// - `Unconditional`: a single-instruction `jmp <in-range>` region whose
    ///   trailing branch opcode was popped by `finish_branch_or_tail_call` —
    ///   no body remains but the successor edge must still be preserved.
    /// - `TailCall`: the synthetic conditional-tail-call stub built by
    ///   [`Self::tail_call_stub`] for a CondBranch arm whose target lies
    ///   outside the function bound — nothing outside the bound is ever
    ///   decoded, so the stub has no body by construction.
    ///
    /// The IR-layer per-region driver iterates `region.insns` (a no-op for
    /// empty insns) and handles the terminator separately.
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

    /// Returns the synthetic tail-call stub region for `addr`, creating it
    /// on first use.
    ///
    /// Stubs lower the out-of-function arm of a conditional branch: an
    /// **empty** region at the OOB target address terminated
    /// `TailCall { target: addr }`, wired as a regular CondBranch successor
    /// edge but never pushed onto the work queue — no byte outside
    /// `[start, start + fn_max_size)` is ever decoded.  The IR layer lifts
    /// the stub as `Call(IntConst(target)) + Return`, so the conditional
    /// survives with the leaving arm represented as a conditional tail
    /// call.
    ///
    /// Keyed through `start_addr_to_region_id` like every region, so two
    /// branches to the same OOB address share one stub instead of
    /// colliding on the one-region-per-start-address invariant.
    ///
    /// # Errors
    /// Propagates [`Self::add_region`] failures (none reachable for the
    /// empty-`TailCall` shape, which `add_region` explicitly allows).
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

    /// Finds the region that contains `addr`, if any.
    ///
    /// Uses a [`BTreeMap`] range query to find the last region whose
    /// `start_addr <= addr`, then confirms that `addr` also falls within the
    /// region's instruction range via [`Region::contains_addr`].
    pub(super) fn find_region_containing_addr(
        &self,
        addr: PcodeInsnAddr,
    ) -> Option<(NodeIndex, &Region)> {
        // Find the last region whose start_addr <= addr
        let (_, &region_id) = self.start_addr_to_region_id.range(..=addr).next_back()?;

        let region = self.region_graph.node_weight(region_id)?;
        region.contains_addr(addr).then_some((region_id, region))
    }

    /// Returns the pcode address corresponding to the function entry point.
    fn start_pcode_addr(&self) -> PcodeInsnAddr {
        PcodeInsnAddr {
            machine_addr: self.start_addr,
            insn_index: 0,
        }
    }

    /// Routes `addr` to either an existing region or a new one.
    ///
    /// - If a region already contains `addr` at its *start*, just adds the
    ///   incoming edge.
    /// - If a region contains `addr` in its *interior*, calls [`split_region`](Self::split_region)
    ///   to split it and then adds the edge to the second half.
    /// - If no region contains `addr`, builds a new region via [`RegionBuilder`].
    fn explore(&mut self, parent_region: Option<NodeIndex>, addr: PcodeInsnAddr) -> Result<()> {
        let existing_region = self.find_region_containing_addr(addr);
        if let Some((region_id, region)) = existing_region {
            // This is the case that someone just referenced our region - add an edge between them.
            let parent_region_id = parent_region
                .ok_or_else(|| anyhow!("non-entry work-queue item has no parent edge"))?;
            if region.start_addr == addr {
                // The address lands on the start of an existing region — wire an edge.
                self.region_graph.add_edge(parent_region_id, region_id, ());
            } else {
                // The address lands inside an existing region — split it and
                // wire the edge to the new "second half".
                let second_region = self.split_region(region_id, addr)?;
                self.region_graph
                    .add_edge(parent_region_id, second_region, ());
            }
        } else {
            RegionBuilder::new(self, addr, parent_region).build()?;
        }
        Ok(())
    }

    /// Builds the completed [`Cfg`].
    ///
    /// The `Cfg` is a pure data structure (regions + edges) and does
    /// not own the Sleigh; the borrowed Sleigh stays in the caller's
    /// scope, usable immediately after `build()` returns for the IR
    /// lifter / dot renderer / next CFG rebuild.
    ///
    /// Seeds the work queue with the entry address, processes items until the
    /// queue is empty, then locates the entry region.
    ///
    /// # Errors
    /// Returns an `anyhow::Error` if disassembly fails, if the start region
    /// cannot be located after processing, or if any region split or edge
    /// routing fails.
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
    //! Tests for the `Builder`-private helpers `add_region`,
    //! `find_region_containing_addr`, and `split_region`.  Ported from
    //! pre-rewrite `crates/cfg/tests/builder_{add_region,find_region,
    //! split_region}.rs`.  Live inline so the `pub(super)` helpers are
    //! reachable without re-exporting them via a `test_api`.

    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use petgraph::visit::{EdgeRef, IntoEdgeReferences};
    use rsleigh::mem_readers::BufMemReader;
    use strider_target::SleighArch;

    use super::*;
    use crate::CfgOptions;
    use crate::types::{
        MachineInsnAddr, PcodeInsnAddr, Region, RegionInstruction, RegionTerminator,
    };

    type TestReader = BufMemReader<Vec<u8>>;

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
            terminator: RegionTerminator::Unconditional,
        }
    }

    fn make_sleigh() -> rsleigh::Sleigh<TestReader> {
        let arch = SleighArch::x86_64();
        let reader = BufMemReader::new(Vec::<u8>::new(), 0x0);
        rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("create empty Sleigh")
    }

    fn make_builder<'a>(
        start_addr: u64,
        sleigh: &'a mut rsleigh::Sleigh<TestReader>,
    ) -> Builder<'a, TestReader> {
        let arch = SleighArch::x86_64();
        Builder::for_arch(&arch, sleigh, start_addr, &CfgOptions::default())
    }

    // ── add_region ───────────────────────────────────────────────────────

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
        // Empty regions are allowed ONLY with `Unconditional` (popped
        // trailing branch in `finish_branch_or_tail_call`) or `TailCall`
        // (the synthetic conditional-tail-call stub).  Any other
        // terminator on an empty region is a construction bug.
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
        // `finish_branch_or_tail_call` pops the trailing branch opcode
        // before sealing a region as `Unconditional`; a single-instruction
        // `jmp <in-range>` region is therefore empty by the time it
        // reaches `add_region`, which must accept it.
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
        // The synthetic conditional-tail-call stub (built for a CondBranch
        // arm whose target lies outside the function bound) is an empty
        // region terminated `TailCall`; `add_region` must accept it.
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

    // ── find_region_containing_addr ──────────────────────────────────────

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

    // ── split_region ─────────────────────────────────────────────────────

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
        // Pins the invariant the in-place `debug_assert!` in `split_region`
        // defends: a real split (`0 < split_index < len`) must leave the
        // second half — which retains `region_id` and its original
        // terminator — non-empty, so it never silently bypasses
        // `add_region`'s empty-region guard.  Exercise every interior split
        // point of a 4-insn region.
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

        // The original `a → b_id` edge was rewired to `a → first`, so the
        // second half's only remaining incoming edge is the split's own
        // fall-through from `first` — not from `a`.
        let second_incoming: Vec<_> = b
            .region_graph
            .edges_directed(b_id, petgraph::Incoming)
            .collect();
        assert_eq!(second_incoming.len(), 1);
        assert_eq!(second_incoming[0].source(), first);
    }

    #[test]
    fn split_addr_in_zero_pcode_hole_rounds_down_to_largest_le() {
        // Region [(0x1000), (0x1004), (0x100c)] — hole between 0x1004
        // and 0x100c that 0x1008 falls into.  Mirrors the AArch64 PAC
        // zero-pcode-op case.
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
        // After a hole round-down, the second region's start_addr (0x1008)
        // sits below its first surviving insn (0x100c).  A branch targeting an
        // address in the phantom span [0x1008, 0x100c) — e.g. 0x100a — must be
        // a no-op split (returns the same region), NOT a hard CFG-build error.
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let original = b
            .add_region(make_region(&[(0x1000, 0), (0x1004, 0), (0x100c, 0)]))
            .unwrap();
        let second = b.split_region(original, addr(0x1008, 0)).unwrap();
        assert_eq!(second, original);
        assert_eq!(b.region_graph[second].start_addr, addr(0x1008, 0));

        // 0x100a is below the only insn (0x100c) but >= start_addr (0x1008).
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
