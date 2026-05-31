mod indirect_resolver;
mod region_builder;
mod split;

pub use indirect_resolver::{IndirectResolverFn, ResolvedTargets};

use region_builder::RegionBuilder;

use std::collections::BTreeMap;

use rustc_hash::FxHashMap;

use petgraph::graph::NodeIndex;

use strider_ir::ReadOnlyMemory;

use crate::cfg::options::Options;
use crate::cfg::types::{MachineInsnAddr, PcodeInsnAddr, Region, RegionGraph};
use crate::cfg::Cfg;
use anyhow::{anyhow, bail};

use crate::cfg::Result;

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
/// use strider_lift::cfg::{Builder, OptionsBuilder};
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
/// let opts = OptionsBuilder::new().build();
/// let arch = SleighArch::x86_64();
/// let cfg = Builder::for_arch(&arch, &mut sleigh, fn_addr, opts).build()?;
/// // `sleigh` is still owned + usable here (the builder only borrowed it).
/// # Ok::<(), anyhow::Error>(())
/// ```
///
/// See `crates/strider-lift/tests/cfg_build_end_to_end.rs` for runnable
/// end-to-end examples.
pub struct Builder<'rom, 'a, R: rsleigh::MemReader> {
    pub(super) sleigh: &'a mut rsleigh::Sleigh<R>,
    /// Virtual address at which the function entry point begins.
    pub(super) start_addr: MachineInsnAddr,
    pub(super) options: Options,
    /// Target architecture.  Carries both endianness (threaded into the
    /// installed [`IndirectResolverFn`] — canonical implementation:
    /// `strider_analyze::indirect_resolver::resolve_indirect_target` —
    /// which builds a mini IR via `crate::pcode_lift::ValueLifter::new`)
    /// and the [`strider_target::ArchPreset`] discriminator consulted by
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
    /// Optional callback that resolves the target of a `BranchIndirect`
    /// when no pre-classified entry in `options.known_targets` matches.
    /// When `None`, the builder treats every unresolved `BranchIndirect`
    /// as deferred via
    /// [`crate::cfg::RegionTerminator::UnresolvedIndirectBranch`].
    /// Install one with [`Self::with_indirect_resolver`] — the canonical
    /// implementation is the
    /// `strider_analyze::indirect_resolver::resolve_indirect_target`
    /// free function, wrapped in an [`IndirectResolverFn`] closure.
    pub(super) indirect_resolver: Option<IndirectResolverFn<R>>,
    /// Borrowed read-only memory image consulted by the indirect-branch
    /// resolver when folding constant-address loads (e.g. rodata-resident
    /// jump tables).  `None` disables that step.  Install with
    /// [`Self::with_read_only_memory`].
    ///
    /// Borrowed (not `Arc`) because strider runs single-threaded and the
    /// rom outlives any single CFG build by construction — the
    /// orchestrator owns it for the whole run and threads it down per
    /// iteration.
    pub(super) read_only_memory: Option<&'rom dyn ReadOnlyMemory>,
}

impl<'rom, 'a, R: rsleigh::MemReader> Builder<'rom, 'a, R> {
    /// Creates a new `Builder` whose endianness AND `ArchPreset` are
    /// derived atomically from `arch` (which is stored in full).  The
    /// canonical constructor for CFG building — carrying the whole
    /// `SleighArch` (which is `Copy + Eq`) prevents the silent
    /// misclassification that a split endianness/preset ctor would
    /// invite (e.g. a big-endian binary decoded as LE, or AArch64 `brk`
    /// classified as the x86 stub).  The Sleigh is borrowed mutably
    /// (`lift_one(&mut self)` is stateful), not owned — the caller
    /// retains ownership and can reuse it after `build()` returns.
    #[must_use]
    pub fn for_arch(
        arch: &strider_target::SleighArch,
        sleigh: &'a mut rsleigh::Sleigh<R>,
        start_addr: u64,
        options: Options,
    ) -> Self {
        Self {
            sleigh,
            start_addr: start_addr.into(),
            options,
            arch: *arch,
            region_graph: RegionGraph::new(),
            start_addr_to_region_id: BTreeMap::new(),
            work_queue: Vec::new(),
            indirect_resolver: None,
            read_only_memory: None,
        }
    }

    /// Installs the borrowed read-only memory image consulted by the
    /// indirect-branch resolver when folding constant-address loads
    /// (e.g. rodata-resident jump tables).  Use the same
    /// `ReadOnlyMemory` that the optimizer's `LoadReadOnly` pass
    /// would see (typically the binary's mapped `.rodata` / `.text`).
    ///
    /// Borrowed (not `Arc`) because the orchestrator owns the rom for
    /// the whole run and threads it down per CFG rebuild; the cfg
    /// builder is short-lived and never outlives the rom.
    #[must_use]
    pub fn with_read_only_memory(mut self, rom: &'rom dyn ReadOnlyMemory) -> Self {
        self.read_only_memory = Some(rom);
        self
    }

    /// Installs the [`IndirectResolverFn`] callback used when the
    /// builder encounters a `BranchIndirect` that's not pre-classified
    /// in `options.known_targets`.  Without a resolver, every
    /// unresolved `BranchIndirect` is deferred via
    /// [`crate::cfg::RegionTerminator::UnresolvedIndirectBranch`].
    /// Callers that want indirect-branch resolution (the strider
    /// orchestrator, the example binary) must call this with the
    /// canonical implementation:
    ///
    /// ```text
    /// use strider_lift::cfg::Builder;
    /// use strider_analyze::indirect_resolver::resolve_indirect_target;
    ///
    /// let resolver: strider_lift::cfg::IndirectResolverFn<_> =
    ///     Box::new(|insns, target_vn, sleigh, lr_vn, rom, endianness| {
    ///         resolve_indirect_target(insns, target_vn, sleigh, lr_vn, rom, endianness)
    ///     });
    /// let cfg = Builder::for_arch(&arch, &mut sleigh, addr, opts)
    ///     .with_indirect_resolver(resolver)
    ///     .build()?;
    /// ```
    ///
    /// (Not a runnable doctest: this crate cannot depend on
    /// `strider-analyze` — that would create a back-edge.  The
    /// snippet is the canonical pattern downstream consumers
    /// wire up.)
    ///
    /// Keeps the dep direction forward: the resolver implementation
    /// lives **above** strider-lift in the crate-dependency order, so
    /// strider-lift doesn't need a `strider-analyze` back-edge for the
    /// cfg-time mini-IR resolver.
    #[must_use]
    pub fn with_indirect_resolver(
        mut self,
        resolver: IndirectResolverFn<R>,
    ) -> Self {
        self.indirect_resolver = Some(resolver);
        self
    }

    /// Inserts `region` into the graph and records its start address in the
    /// lookup map.  Returns the assigned [`NodeIndex`].
    ///
    /// # Errors
    /// Returns an error when `region.insns` is empty AND `region.terminator`
    /// is not [`super::types::RegionTerminator::Unconditional`].  Empty
    /// regions terminating with `Unconditional` are explicitly allowed:
    /// they arise from the single-instruction CondBranch-with-OOB-successor
    /// case, where popping the trailing CondBranch leaves no body but the
    /// in-range edge must still be preserved.  The IR-layer per-region
    /// driver iterates `region.insns` (a no-op for empty insns) and handles
    /// the terminator separately.
    pub(super) fn add_region(&mut self, region: Region) -> Result<NodeIndex> {
        if region.insns.is_empty()
            && !matches!(region.terminator, super::types::RegionTerminator::Unconditional)
        {
            bail!(
                "region at {:?} has no instructions and terminator is {:?} (only Unconditional is permitted for empty regions)",
                region.start_addr, region.terminator,
            );
        }

        let start_addr = region.start_addr;
        let region_id = self.region_graph.add_node(region);
        self.start_addr_to_region_id.insert(start_addr, region_id);
        Ok(region_id)
    }

    /// Finds the region that contains `addr`, if any.
    ///
    /// Uses a [`BTreeMap`] range query to find the last region whose
    /// `start_addr <= addr`, then confirms that `addr` also falls within the
    /// region's instruction range via [`Region::contains_addr`].
    pub(super) fn find_region_containing_addr(&self, addr: PcodeInsnAddr) -> Option<(NodeIndex, &Region)> {
        // Find the last region whose start_addr <= addr
        let (_, &region_id) = self.start_addr_to_region_id.range(..=addr).next_back()?;

        let region = self.region_graph.node_weight(region_id)?;
        if region.contains_addr(addr) {
            Some((region_id, region))
        } else {
            None
        }
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
    fn explore(
        &mut self,
        parent_region: Option<NodeIndex>,
        addr: PcodeInsnAddr,
    ) -> Result<()> {
        let existing_region = self.find_region_containing_addr(addr);
        if let Some((region_id, region)) = existing_region {
            // This is the case that someone just referenced our region - add an edge between them.
            let parent_region_id =
                parent_region.ok_or_else(|| anyhow!("non-entry work-queue item has no parent edge"))?;
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

    /// Threads IR-level indirect-branch resolver results back into the CFG build.
    ///
    /// When the builder encounters a `BranchIndirect` whose pcode
    /// address is in `known_targets`, it uses the cached classification
    /// directly instead of invoking the cfg-time mini-graph resolver.
    /// This is the strider fixed-point orchestrator's feedback path:
    /// after the IR-level indirect-branch resolver resolves an indirect
    /// branch, the next iteration's
    /// CFG build reads the resolution from `known_targets` and emits
    /// the appropriate `RegionTerminator` (`Branch` / `TailCall` /
    /// `Switch` / `Return`) directly — no re-resolution overhead.
    ///
    /// Replaces any previous `known_targets` set on this builder.
    /// Pass an empty map to clear.
    #[must_use]
    pub fn with_known_targets(
        mut self,
        known_targets: FxHashMap<PcodeInsnAddr, ResolvedTargets>,
    ) -> Self {
        self.options.known_targets = known_targets;
        self
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
        let (starting_region, _) = self
            .find_region_containing_addr(start_addr)
            .ok_or_else(|| {
                anyhow!(
                    "cfg build completed but no region contains the entry address {start_addr:?}; \
                     check that the entry is decodable"
                )
            })?;

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
    use crate::cfg::types::{
        MachineInsnAddr, PcodeInsnAddr, Region, RegionInstruction, RegionTerminator,
    };
    use crate::cfg::OptionsBuilder;

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
        rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader)
            .expect("create empty Sleigh")
    }

    fn make_builder<'a>(
        start_addr: u64,
        sleigh: &'a mut rsleigh::Sleigh<TestReader>,
    ) -> Builder<'static, 'a, TestReader> {
        let arch = SleighArch::x86_64();
        Builder::for_arch(&arch, sleigh, start_addr, OptionsBuilder::new().build())
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
    fn add_region_empty_region_with_non_unconditional_returns_error() {
        // Empty regions are allowed ONLY with `Unconditional` (the
        // single-instruction CondBranch-with-OOB-successor fold).  Any
        // other terminator on an empty region is a construction bug.
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let empty = Region {
            start_addr: addr(0x1000, 0),
            insns: Vec::new(),
            terminator: RegionTerminator::Return,
        };
        let err = b.add_region(empty).unwrap_err();
        assert!(err.to_string().contains("has no instructions"), "got: {err}");
    }

    #[test]
    fn add_region_empty_unconditional_is_allowed() {
        // The OOB-CondBranch fold produces an empty region with
        // `Unconditional`; `add_region` must accept it.
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let empty = Region {
            start_addr: addr(0x1000, 0),
            insns: Vec::new(),
            terminator: RegionTerminator::Unconditional,
        };
        b.add_region(empty).expect("empty Unconditional region is allowed");
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
            b.find_region_containing_addr(addr(0x1000, 0)).map(|(i, _)| i),
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
            b.find_region_containing_addr(addr(0x1008, 0)).map(|(i, _)| i),
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
            b.find_region_containing_addr(addr(0x100f, 0)).map(|(i, _)| i),
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
            b.find_region_containing_addr(addr(0x1004, 0)).map(|(i, _)| i),
            Some(id1)
        );
        assert_eq!(
            b.find_region_containing_addr(addr(0x1010, 0)).map(|(i, _)| i),
            Some(id2)
        );
        assert_eq!(
            b.find_region_containing_addr(addr(0x1018, 0)).map(|(i, _)| i),
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
        let incoming: Vec<_> = b.region_graph.edges_directed(first, petgraph::Incoming).collect();
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
        assert_eq!(b.region_graph[first_id].insns.last().unwrap().addr, addr(0x1004, 0));

        assert_eq!(b.start_addr_to_region_id[&addr(0x1008, 0)], original);
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
