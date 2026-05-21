mod indirect_resolver;
mod region_builder;
mod split;

pub use indirect_resolver::{IndirectResolverFn, ResolvedTargets};

use region_builder::RegionBuilder;

use std::collections::{BTreeMap, HashMap};

use petgraph::graph::NodeIndex;

use crate::cfg::options::Options;
use crate::cfg::types::{MachineInsnAddr, PcodeInsnAddr, Region, RegionEdgeKind, RegionGraph};
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
/// let sleigh = rsleigh::Sleigh::new(
///     rsleigh::sla_spec::SLA_SPEC_X86_64,
///     rsleigh::pspec::PSPEC_X86_64,
///     reader,
/// ).expect("create Sleigh");
/// let opts = OptionsBuilder::new().build();
/// let arch = SleighArch::x86_64();
/// let cfg = Builder::for_arch(&arch, sleigh, fn_addr, opts).build()?;
/// # Ok::<(), anyhow::Error>(())
/// ```
///
/// See `crates/cfg/tests/build_end_to_end.rs` for runnable end-to-end examples.
pub struct Builder<R: rsleigh::MemReader> {
    pub(super) sleigh: rsleigh::Sleigh<R>,
    /// Virtual address at which the function entry point begins.
    pub(super) start_addr: MachineInsnAddr,
    pub(super) options: Options,
    /// Byte order of the target architecture.  Threaded into
    /// [`super::indirect_resolve::resolve_indirect_target`] which
    /// builds a mini IR via `crate::pcode_lift::ValueLifter::new`.  Set
    /// atomically with `preset` via [`Self::for_arch`].
    pub(super) endianness: strider_target::Endianness,
    /// Coarse architecture family.  Consulted by
    /// [`super::region_builder::RegionBuilder`]'s `Opcode::CallOther`
    /// arm to pass the right `arch` to
    /// [`strider_target::call_other_abi::classify`].  Set atomically with
    /// `endianness` via [`Self::for_arch`].
    pub(super) preset: strider_target::ArchPreset,
    /// The graph being constructed.
    pub(super) graph: RegionGraph,
    /// Maps each region's `start_addr` to its [`NodeIndex`].
    /// Used by `find_region_containing_addr` and `split_region`.
    pub(super) start_addr_to_region_id: BTreeMap<PcodeInsnAddr, NodeIndex>,
    /// Pending addresses to explore, together with the parent edge they
    /// should connect from. Treated as a LIFO stack (depth-first traversal).
    pub(super) work_queue: Vec<(Option<(NodeIndex, RegionEdgeKind)>, PcodeInsnAddr)>,
    /// Optional cache of `(machine_addr) → Arc<LiftRes>`.  When
    /// present, [`super::region_builder::RegionBuilder::lift_one_cached`]
    /// consults it before invoking Sleigh's decoder.  The cache must be
    /// scoped to a single Sleigh context (see [`crate::cfg::DecodeCache`]).
    pub(super) decode_cache: Option<crate::cfg::DecodeCache>,
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
}

impl<R: rsleigh::MemReader> Builder<R> {
    /// Creates a new `Builder` whose endianness AND `ArchPreset` are
    /// derived atomically from `arch`.  The canonical constructor for
    /// CFG building — setting both fields from one `SleighArch` source
    /// prevents the silent misclassification that a split
    /// endianness/preset ctor would invite (e.g. a big-endian binary
    /// decoded as LE, or AArch64 `brk` classified as the x86 stub).
    #[must_use]
    pub fn for_arch(
        arch: &strider_target::SleighArch,
        sleigh: rsleigh::Sleigh<R>,
        start_addr: u64,
        options: Options,
    ) -> Self {
        Self {
            sleigh,
            start_addr: start_addr.into(),
            options,
            endianness: arch.endianness(),
            preset: arch.preset(),
            graph: RegionGraph::new(),
            start_addr_to_region_id: BTreeMap::new(),
            work_queue: Vec::new(),
            decode_cache: None,
            indirect_resolver: None,
        }
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
    /// ```ignore
    /// use std::sync::Arc;
    /// use strider_lift::cfg::Builder;
    /// use strider_analyze::indirect_resolver::resolve_indirect_target;
    ///
    /// let resolver: strider_lift::cfg::IndirectResolverFn<_> =
    ///     Arc::new(|insns, target_vn, sleigh, lr_vn, rom, endianness| {
    ///         resolve_indirect_target(insns, target_vn, sleigh, lr_vn, rom, endianness)
    ///     });
    /// let cfg = Builder::for_arch(&arch, sleigh, addr, opts)
    ///     .with_indirect_resolver(resolver)
    ///     .build()?;
    /// ```
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

    /// Attaches a Sleigh-decode cache to this builder.  When set,
    /// every machine-instruction lift consults the cache before
    /// invoking `Sleigh::lift_one`, and inserts on miss.  See
    /// [`crate::cfg::DecodeCache`] for the cache's invariants (in
    /// particular: it must be scoped to one Sleigh context).
    #[must_use]
    pub fn with_decode_cache(mut self, cache: crate::cfg::DecodeCache) -> Self {
        self.decode_cache = Some(cache);
        self
    }

    /// Inserts `region` into the graph and records its start address in the
    /// lookup map.  Returns the assigned [`NodeIndex`].
    ///
    /// # Errors
    /// Returns an error when `region.insns` is empty AND `region.terminator`
    /// is not [`super::types::RegionTerminator::Branch`].  Empty regions
    /// terminating with `Branch` are explicitly allowed: they arise from
    /// the single-instruction CondBranch-with-OOB-successor case, where
    /// popping the trailing CondBranch leaves no body but the in-range
    /// edge must still be preserved.  The IR-layer per-region driver
    /// iterates `region.insns` (a no-op for empty insns) and handles the
    /// terminator separately.
    pub(super) fn add_region(&mut self, region: Region) -> Result<NodeIndex> {
        if region.insns.is_empty()
            && !matches!(region.terminator, super::types::RegionTerminator::Branch)
        {
            bail!(
                "region at {:?} has no instructions and terminator is {:?} (only Branch is permitted for empty regions)",
                region.start_addr, region.terminator,
            );
        }

        let start_addr = region.start_addr;
        let region_id = self.graph.add_node(region);
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

        let region = self.graph.node_weight(region_id)?;
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
        parent_region: Option<(NodeIndex, RegionEdgeKind)>,
        addr: PcodeInsnAddr,
    ) -> Result<()> {
        let existing_region = self.find_region_containing_addr(addr);
        if let Some((region_id, region)) = existing_region {
            // This is the case that someone just referenced our region - add an edge between them.
            let (parent_region_id, edge_kind) =
                parent_region.ok_or_else(|| anyhow!("non-entry work-queue item has no parent edge"))?;
            if region.start_addr == addr {
                // The address lands on the start of an existing region — wire an edge.
                self.graph.add_edge(parent_region_id, region_id, edge_kind);
            } else {
                // The address lands inside an existing region — split it and
                // wire the edge to the new "second half".
                let second_region = self.split_region(region_id, addr)?;
                self.graph
                    .add_edge(parent_region_id, second_region, edge_kind);
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
        known_targets: HashMap<PcodeInsnAddr, ResolvedTargets>,
    ) -> Self {
        self.options.known_targets = known_targets;
        self
    }

    /// Builds and returns the completed [`Cfg`].
    ///
    /// Seeds the work queue with the entry address, processes items until the
    /// queue is empty, then locates the entry region.
    ///
    /// # Errors
    /// Returns an `anyhow::Error` if disassembly fails, if the start region
    /// cannot be located after processing, or if any region split or edge
    /// routing fails.
    pub fn build(mut self) -> Result<Cfg<R>> {
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
            graph: self.graph,
            sleigh: self.sleigh,
            entry: starting_region,
            start_addr_to_region_id: self.start_addr_to_region_id,
        })
    }
}

