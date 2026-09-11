mod flow;
mod region_builder;
mod split;

use flow::NO_FLOW_VARS;
pub use flow::{FlowContext, FlowVars};
use region_builder::RegionBuilder;

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;

use petgraph::graph::NodeIndex;
use petgraph::stable_graph::EdgeIndex;
use petgraph::visit::EdgeRef;

use crate::Cfg;
use crate::options::CfgOptions;
use crate::types::{MachineInsnAddr, PcodeInsnAddr, Region, RegionGraph};
use anyhow::{anyhow, bail};

use crate::Result;

#[cfg(test)]
thread_local! {
    /// Regions [`Builder::find_region_containing_addr`] tested for containment
    /// since a test last cleared it.
    pub(crate) static REGION_PROBES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A pending region to explore off a successor edge.
pub(super) struct WorkItem {
    /// The region edging into this one, or `None` for the entry.
    pub(super) parent: Option<NodeIndex>,
    pub(super) addr: PcodeInsnAddr,
    /// The context this target decodes in: what Sleigh flowed to it for a
    /// direct edge, else the function mode carrying the resolved branch's
    /// ISA-mode bit. Restored before decode, undoing a forward-hold clobber.
    pub(super) carried: FlowContext,
    /// Seeded from `known_targets` rather than reached by a decoded branch. A
    /// direct edge that will not decode is a real error; a seeded one may be a
    /// misclassified jump-table entry, so it is dropped and reported instead.
    pub(super) seeded: bool,
}

/// Incrementally constructs a [`Cfg`] from a binary entry point.
///
/// A work queue seeded with the entry address drives decoding: each item
/// either decodes a new region or routes an edge to an existing one. A branch
/// landing mid-region splits that region in two.
///
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
/// # Ok::<(), anyhow::Error>(())
/// ```
pub struct Builder<'a, R: rsleigh::MemReader> {
    pub(super) sleigh: &'a mut rsleigh::Sleigh<R>,
    pub(super) start_addr: MachineInsnAddr,
    pub(super) options: CfgOptions,
    pub(super) arch: strider_target::SleighArch,
    pub(super) region_graph: RegionGraph,
    /// One region per start address.
    pub(super) start_addr_to_region_id: BTreeMap<PcodeInsnAddr, NodeIndex>,
    /// Longest span of a region another region starts INSIDE of, which bounds
    /// [`Self::find_region_containing_addr`]'s reverse walk.  Monotone: a split
    /// only shortens both halves.
    ///
    /// Non-zero without overlapping instruction streams too: splitting at a
    /// pcode index INSIDE one machine instruction leaves the first half's last
    /// entry carrying the whole machine-instruction length, so its span reaches
    /// past the second half's start.  That shadow is bounded by one instruction
    /// length.
    pub(super) max_shadowed_span: u64,
    /// Starts of the regions another region begins inside of: the only ones a
    /// lookup miss has to re-probe.
    pub(super) shadowed_starts: BTreeSet<PcodeInsnAddr>,
    /// LIFO for depth-first exploration.
    pub(super) work_queue: Vec<WorkItem>,
    /// CC overrides for CALL TARGETS, keyed by target machine address.  Only
    /// `no_return` is read here.
    pub(super) per_address_ccs: rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>,
    /// Snapshotted once at construction and indexed by `user_op_id`.  Empty
    /// when the Sleigh reports no user ops or the snapshot fails.
    pub(super) user_op_names: Vec<String>,
    /// The flowing context vars, borrowed from the lift engine (discovered once
    /// per sla); empty by default.
    pub(super) flow_vars: &'a FlowVars,
    /// This function's constant ISA mode, the base context a strider-resolved
    /// target decodes in ([`Self::enqueue_resolved`]).  Empty unless
    /// [`Self::with_function_mode`] supplies it.
    pub(super) function_mode: FlowContext,
    /// Seeded targets whose region would not decode, each with the region that
    /// seeded it; see [`Cfg::undecodable_seeded_targets`].  Keyed on the SITE,
    /// not the address alone: two explorations of one address decode in
    /// different contexts by construction, so a failure at one site says
    /// nothing about the same address reached from another.
    pub(super) undecodable_seeded: Vec<(Option<NodeIndex>, PcodeInsnAddr)>,
    /// The ISA mode each region was decoded in, for the conflict check in
    /// [`Self::explore`].
    pub(super) region_isa_mode: BTreeMap<NodeIndex, u32>,
    /// Addresses reached in two different ISA modes; see
    /// [`Cfg::isa_mode_conflicts`].
    pub(super) isa_mode_conflicts: Vec<PcodeInsnAddr>,
    /// Branch targets interior to a region but off every instruction boundary;
    /// see [`Cfg::interior_branch_targets`].
    pub(super) interior_branch_targets: Vec<PcodeInsnAddr>,
    /// The address each [`Self::seat_non_boundary_target`] edge was seated for.
    /// Every other edge targets a region starting exactly at its own address,
    /// so a split can move it by start alone; these point INTO the region, and
    /// only the half that keeps those bytes may keep the edge.
    pub(super) non_boundary_seats: rustc_hash::FxHashMap<EdgeIndex, PcodeInsnAddr>,
    /// Sites seated as a `Return` from a `LinkRegister` answer; see
    /// [`Cfg::link_register_seated`].
    pub(super) link_register_seated: Vec<PcodeInsnAddr>,
    /// Sites seated as a `TailCall` from a single resolved target.
    pub(super) tail_call_seated: Vec<PcodeInsnAddr>,
}

impl<'a, R: rsleigh::MemReader> Builder<'a, R> {
    pub fn for_arch(
        arch: &strider_target::SleighArch,
        sleigh: &'a mut rsleigh::Sleigh<R>,
        start_addr: u64,
        options: &CfgOptions,
    ) -> Self {
        // `Some(0)` is unbounded: a zero-length bound would pin every decode
        // at `start_addr`.
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
            max_shadowed_span: 0,
            shadowed_starts: BTreeSet::new(),
            work_queue: Vec::new(),
            undecodable_seeded: Vec::new(),
            region_isa_mode: BTreeMap::new(),
            isa_mode_conflicts: Vec::new(),
            interior_branch_targets: Vec::new(),
            non_boundary_seats: rustc_hash::FxHashMap::default(),
            link_register_seated: Vec::new(),
            tail_call_seated: Vec::new(),
            per_address_ccs: rustc_hash::FxHashMap::default(),
            user_op_names,
            // A single-shot build on a fresh engine has no cross-function
            // context to leak; a reused engine supplies the vars via
            // `with_flow_vars`.
            flow_vars: &NO_FLOW_VARS,
            function_mode: FlowContext::default(),
        }
    }

    /// Supplies the flowing context vars (from [`FlowVars::discover`]), so a
    /// reused engine's decode mode is re-imposed at each region rather than left
    /// to the shared, leak-prone context DB.
    #[must_use]
    pub fn with_flow_vars(mut self, flow_vars: &'a FlowVars) -> Self {
        // The ISA-mode var this arch re-imposes at each region (`restore_at`)
        // must be one the sla actually flows, or the re-impose silently no-ops
        // and interworking correctness is lost.
        debug_assert!(
            self.arch
                .isa_mode_var()
                .is_none_or(|v| flow_vars.contains(v)),
            "arch ISA-mode var {:?} is not among the sla's flowing context vars",
            self.arch.isa_mode_var(),
        );
        self.flow_vars = flow_vars;
        self
    }

    /// Supplies this function's constant ISA mode, captured at its entry.
    #[must_use]
    pub fn with_function_mode(mut self, function_mode: FlowContext) -> Self {
        self.function_mode = function_mode;
        self
    }

    #[must_use]
    pub fn with_per_address_ccs(
        mut self,
        per_address_ccs: rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>,
    ) -> Self {
        self.per_address_ccs = per_address_ccs;
        self
    }

    /// Enqueue `addr` as a *direct* successor of the region at `source_addr`
    /// (branch or fall-through), capturing the context Sleigh flowed to it.
    ///
    /// Never seeded: a direct edge is written in the instruction stream, so an
    /// address it names is code and a decode failure there is a real error.
    /// The taint belongs only to [`Self::enqueue_resolved`]'s targets, which
    /// the classifier may have over-approximated. Propagating it down direct
    /// edges instead swallows genuine failures and leaves the region that lost
    /// its successor with no terminator.
    pub(super) fn enqueue(
        &mut self,
        parent: Option<NodeIndex>,
        addr: PcodeInsnAddr,
        source_addr: u64,
    ) {
        let mut carried = self.flow_vars.snapshot(self.sleigh, addr.machine_addr.addr);
        // A direct edge switches no ISA mode, so the child decodes in the
        // parent's. The child's own snapshot reads the pspec DEFAULT for a
        // target the mode has not straight-line flowed to yet (a forward
        // branch), so take the ISA-mode var from the parent region's context at
        // `source_addr`; transients keep the snapshot, a region start having
        // fresh IT-block state.
        if let Some(var) = self.arch.isa_mode_var() {
            self.flow_vars
                .take_var_at(&mut carried, self.sleigh, source_addr, var);
        }
        self.work_queue.push(WorkItem {
            parent,
            addr,
            carried,
            seeded: false,
        });
    }

    /// Enqueue `addr` as a strider-*resolved indirect* target of `parent`,
    /// resolved from the branch at `branch_addr`.  Sleigh never flowed the mode
    /// here, so decode in the function mode (default transients: a target is a
    /// region start, where IT-block/register-list state is fresh) with the
    /// ISA-mode var set to what the branch commits: an interworking branch's own
    /// bit (`isa_bit`), else the mode flowing INTO the branch, so a `mov pc` in a
    /// Thumb region of an otherwise-ARM function keeps Thumb.
    pub(super) fn enqueue_resolved(
        &mut self,
        parent: Option<NodeIndex>,
        addr: PcodeInsnAddr,
        isa_bit: Option<bool>,
        branch_addr: u64,
    ) {
        let carried = match self.arch.isa_mode_var() {
            Some(var) => {
                let bit = isa_bit.unwrap_or_else(|| {
                    self.sleigh.get_context_at(branch_addr, var).map_or_else(
                        |_| {
                            // Degrade to the function's own mode; `false` would
                            // decode every resolved same-mode target of a Thumb
                            // function as ARM.
                            self.flow_vars
                                .value_of(&self.function_mode, var)
                                .is_some_and(|v| v != 0)
                        },
                        |mode| mode != 0,
                    )
                });
                self.flow_vars.with_mode_bit(&self.function_mode, var, bit)
            }
            None => self.function_mode.clone(),
        };
        self.work_queue.push(WorkItem {
            parent,
            addr,
            carried,
            seeded: true,
        });
    }

    /// An empty region is rejected unless its terminator is `Unconditional` (a
    /// region sealed at a zero-pcode-op instruction, which `build` segments at
    /// every one of) or `TailCall` (the [`Self::tail_call_stub`] for a
    /// CondBranch arm leaving the function bound, whose bytes are never
    /// decoded).
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

        debug_assert!(
            region.insns_are_ascending(),
            "region at {:?} has out-of-order insns; `insn_index_at` bisects over them",
            region.start_addr,
        );

        // No non-empty region has a "phantom span" (start_addr below its first
        // instruction): `build` segments at every zero-pcode-op instruction.
        debug_assert!(
            region
                .insns
                .first()
                .is_none_or(|i| i.addr.machine_addr == region.start_addr.machine_addr),
            "phantom span: region start {:?} below first insn {:?}",
            region.start_addr,
            region.insns.first().map(|i| i.addr),
        );

        let start_addr = region.start_addr;
        let span = region.span_len();
        // Record both directions of an overlap: a region this one starts inside
        // of, and one already starting inside this one.
        let shadowed = self
            .find_region_containing_addr(start_addr)
            .map(|(_, shadowed)| (shadowed.start_addr, shadowed.span_len()));
        if let Some((shadowed_start, shadowed_span)) = shadowed {
            self.max_shadowed_span = self.max_shadowed_span.max(shadowed_span);
            self.shadowed_starts.insert(shadowed_start);
        }
        let end =
            PcodeInsnAddr::at_machine_start(start_addr.machine_addr.addr.saturating_add(span));
        if self
            .start_addr_to_region_id
            .range((Bound::Excluded(start_addr), Bound::Excluded(end)))
            .next()
            .is_some()
        {
            self.max_shadowed_span = self.max_shadowed_span.max(span);
            self.shadowed_starts.insert(start_addr);
        }
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
            empty_span_len: 0,
            terminator: super::types::RegionTerminator::TailCall {
                target: addr.machine_addr.addr.into(),
            },
        })
    }

    /// The region containing `addr`.
    ///
    /// The greatest start at or below `addr` need not be its owner: regions are
    /// disjoint only while every target lands on an instruction boundary, and
    /// one starting inside another region's last instruction shadows that
    /// region over the rest of those bytes.  Only such a shadowed region can own
    /// `addr` once the greatest start does not, and it reaches at most
    /// `max_shadowed_span` bytes below `addr`, which bounds how far back to
    /// look. The walk visits only `shadowed_starts`: bounding by a single
    /// global span alone would make one overlapping address anywhere in the
    /// function re-probe every region within that span, on every miss.
    pub(super) fn find_region_containing_addr(
        &self,
        addr: PcodeInsnAddr,
    ) -> Option<(NodeIndex, &Region)> {
        let (&greatest, &greatest_id) = self.start_addr_to_region_id.range(..=addr).next_back()?;
        if let Some(owner) = self.region_owning(greatest_id, addr) {
            return Some(owner);
        }
        let floor = PcodeInsnAddr::at_machine_start(
            addr.machine_addr
                .addr
                .saturating_sub(self.max_shadowed_span),
        );
        if floor >= greatest {
            return None;
        }
        self.shadowed_starts
            .range(floor..greatest)
            .rev()
            .filter_map(|start| self.start_addr_to_region_id.get(start))
            .find_map(|&region_id| self.region_owning(region_id, addr))
    }

    fn region_owning(
        &self,
        region_id: NodeIndex,
        addr: PcodeInsnAddr,
    ) -> Option<(NodeIndex, &Region)> {
        #[cfg(test)]
        REGION_PROBES.with(|n| n.set(n.get() + 1));
        let region = self.region_graph.node_weight(region_id)?;
        region.contains_addr(addr).then_some((region_id, region))
    }

    /// A target landing INSIDE an already-decoded region but not on one of its
    /// instruction starts.
    ///
    /// A region is a hole-free run of instructions, but that does not make every
    /// interior ADDRESS a boundary: `0x1003` is interior to a region whose
    /// instructions start at `0x1000` (7 bytes) and `0x1007`. The classifier
    /// over-approximates its index bound deliberately, so a table read one entry
    /// past its end can fold to such an address; `split_region` cannot express
    /// it, so the site defers instead of failing the function.
    pub(super) fn addr_is_interior_non_boundary(&self, addr: PcodeInsnAddr) -> bool {
        self.find_region_containing_addr(addr)
            .is_some_and(|(_, region)| region.start_addr != addr && !region.contains_insn_at(addr))
    }

    /// Seats an edge for a target interior to `owner` but off every instruction
    /// boundary of it, which no split can express.
    ///
    /// A `Switch` resolves its arms by exact start address, so the target is
    /// dropped from the table (it can only be an over-approximated entry); a
    /// table left with no arm at all re-defers the whole site, and by then no
    /// arm edge was ever wired.
    ///
    /// Every other terminator edges to the region that OWNS the address, which
    /// `Cfg::region_if` still matches by containment, so a conditional keeps
    /// both arms. That owner's instruction stream starts EARLIER than the
    /// target, though: for a direct branch into overlapping code the arm is not
    /// the stream the branch jumps to. The address is recorded so a caller is
    /// not left believing the edge is exact -- see
    /// [`Cfg::interior_branch_targets`].
    fn seat_non_boundary_target(
        &mut self,
        parent: NodeIndex,
        owner: NodeIndex,
        addr: PcodeInsnAddr,
    ) -> Result<()> {
        let region = self
            .region_graph
            .node_weight_mut(parent)
            .ok_or_else(|| anyhow!("invalid region index {parent:?}"))?;
        let super::types::RegionTerminator::Switch {
            target_vn,
            targets,
            addr: dispatch,
        } = &mut region.terminator
        else {
            let edge = self.region_graph.add_edge(parent, owner, ());
            self.non_boundary_seats.insert(edge, addr);
            self.interior_branch_targets.push(addr);
            return Ok(());
        };
        let before = targets.len();
        targets.retain(|t| t.addr != addr.machine_addr.addr);
        debug_assert!(
            targets.len() != before,
            "seated interior target {addr:?} names no arm of the Switch that routed it",
        );
        if targets.is_empty() {
            region.terminator = super::types::RegionTerminator::UnresolvedIndirectBranch {
                target_vn: *target_vn,
                addr: *dispatch,
            };
        }
        // Reported whether or not an arm went with it: this path wires no edge,
        // so anything unsaid here leaves the site an arm short in silence.
        self.interior_branch_targets.push(addr);
        Ok(())
    }

    fn start_pcode_addr(&self) -> PcodeInsnAddr {
        PcodeInsnAddr {
            machine_addr: self.start_addr,
            insn_index: 0,
        }
    }

    /// Routes `addr` to the region that owns it, or decodes a new one; a branch
    /// into an existing region's interior splits it. Before decoding a fresh
    /// region `restore_at` pins the `carried` context this edge captured, so a
    /// strider-resolved or backward target still decodes in the mode that
    /// reaches it.
    fn explore(
        &mut self,
        parent_region: Option<NodeIndex>,
        addr: PcodeInsnAddr,
        carried: FlowContext,
    ) -> Result<()> {
        if let Some((region_id, region)) = self.find_region_containing_addr(addr) {
            // An address an existing region owns is reused and this edge's
            // `carried` mode dropped, so for bytes reachable in two ISA modes
            // the first arrival to decode them wins: under the LIFO work queue,
            // the last-enqueued edge. Which one that is depends on queue order,
            // so the loser's path would silently decode in the wrong ISA;
            // recording the clash is what keeps the answer honest.
            let mode_clash = match (
                self.arch.isa_mode_var(),
                self.region_isa_mode.get(&region_id),
            ) {
                (Some(var), Some(&decoded)) => self
                    .flow_vars
                    .value_of(&carried, var)
                    .is_some_and(|want| want != decoded),
                _ => false,
            };
            let is_start = region.start_addr == addr;
            let is_boundary = region.contains_insn_at(addr);
            if mode_clash {
                self.isa_mode_conflicts.push(addr);
            }
            let parent_region_id = parent_region
                .ok_or_else(|| anyhow!("non-entry work-queue item has no parent edge"))?;
            let target = if is_start {
                region_id
            } else if is_boundary {
                self.split_region(region_id, addr)?
            } else {
                // Overlapping code, or a jump-table entry read past the table
                // end: a target no split can express.
                return self.seat_non_boundary_target(parent_region_id, region_id, addr);
            };
            self.region_graph.add_edge(parent_region_id, target, ());
            return Ok(());
        }
        if !self.flow_vars.is_empty() {
            // Undoes a sibling region's forward-hold clobber of this address.
            self.flow_vars.restore_at(
                self.sleigh,
                addr.machine_addr.addr,
                &carried,
                self.arch.isa_mode_var(),
            )?;
        }
        let isa_mode = self
            .arch
            .isa_mode_var()
            .and_then(|var| self.flow_vars.value_of(&carried, var));
        RegionBuilder::new(self, addr, parent_region, isa_mode).build()?;
        Ok(())
    }

    /// Removes the `Switch` arm each undecodable target was seeded from, and
    /// the arm's successor edge with it.
    ///
    /// Every arm must resolve to a successor region starting exactly at it, and
    /// the lifter errors on one that does not. A dropped target leaves such an
    /// arm behind, so it is stripped here and the site is reported through
    /// [`Cfg::undecodable_seeded_targets`] instead.
    ///
    /// Only the site that failed loses the arm. The same address reached from
    /// another site decodes in that site's own context -- `enqueue_resolved`
    /// pins the branch's committed ISA mode, `enqueue` carries the parent
    /// region's -- so on ARM/Thumb or MIPS16 one can fail while the other is
    /// live, and stripping both would take an arm whose edge is wired.
    ///
    /// Stripping the LAST arm re-defers the site to an
    /// `UnresolvedIndirectBranch`: a zero-arm `Switch` fails the whole
    /// function's lift, which is the outcome dropping the target exists to
    /// avoid.
    fn drop_undecodable_switch_arms(&mut self) {
        for (site, target) in self.undecodable_seeded.clone() {
            let Some(site) = site else {
                continue;
            };
            let Some(region) = self.region_graph.node_weight_mut(site) else {
                continue;
            };
            let crate::RegionTerminator::Switch {
                targets,
                target_vn,
                addr,
            } = &mut region.terminator
            else {
                continue;
            };
            let before = targets.len();
            targets.retain(|t| t.addr != target.machine_addr.addr);
            if targets.len() == before {
                continue;
            }
            if targets.is_empty() {
                region.terminator = crate::RegionTerminator::UnresolvedIndirectBranch {
                    target_vn: *target_vn,
                    addr: *addr,
                };
            }
            self.remove_arm_edge(site, target);
        }
    }

    /// Drops the edge `site` wired for the arm at `target`.
    ///
    /// An arm resolves to the region starting exactly at its address. Leaving
    /// the edge behind outlives the arm: a demoted `UnresolvedIndirectBranch`
    /// would carry an outgoing edge its contract forbids, and a `Switch` that
    /// kept its other arms would keep a successor it no longer names.
    fn remove_arm_edge(&mut self, site: NodeIndex, target: PcodeInsnAddr) {
        let Some(&arm) = self.start_addr_to_region_id.get(&target) else {
            return;
        };
        let edges: Vec<EdgeIndex> = self
            .region_graph
            .edges_connecting(site, arm)
            .map(|e| e.id())
            .collect();
        for edge in edges {
            self.region_graph.remove_edge(edge);
            self.non_boundary_seats.remove(&edge);
        }
    }

    pub fn build(mut self) -> Result<Cfg> {
        // The two are independent setters but not independent features: with
        // flow vars and no function mode, `with_mode_bit` hands back an empty
        // context, `explore`'s `isa_mode` stays `None`, `region_isa_mode` is
        // never written and the clash check falls to its catch-all arm, so
        // ISA-mode handling is off with nothing said.
        debug_assert!(
            self.arch.isa_mode_var().is_none_or(|var| {
                self.flow_vars.is_empty()
                    || self.flow_vars.value_of(&self.function_mode, var).is_some()
            }),
            "with_flow_vars without with_function_mode silently disables ISA-mode handling",
        );
        let entry = self.start_pcode_addr();
        self.enqueue(None, entry, entry.machine_addr.addr);
        while let Some(WorkItem {
            parent: parent_region,
            addr: address,
            carried,
            seeded,
        }) = self.work_queue.pop()
        {
            match self.explore(parent_region, address, carried) {
                Ok(()) => {}
                // A seeded target that will not decode is a misclassification,
                // not a broken function: drop the edge and report the address so
                // the caller learns the site is unresolved.
                Err(_) if seeded => self.undecodable_seeded.push((parent_region, address)),
                Err(e) => return Err(e),
            }
        }
        self.drop_undecodable_switch_arms();
        let start_addr = self.start_pcode_addr();
        let (starting_region, _) = self.find_region_containing_addr(start_addr).ok_or_else(
            || {
                anyhow!(
                    "cfg build completed but no region contains the entry address {start_addr:?}; \
                     check that the entry is decodable"
                )
            },
        )?;

        let function_isa_bit = self
            .arch
            .isa_mode_var()
            .and_then(|var| self.flow_vars.value_of(&self.function_mode, var))
            .map(|mode| mode != 0);
        Ok(Cfg {
            region_graph: self.region_graph,
            entry: starting_region,
            undecodable_seeded: self
                .undecodable_seeded
                .iter()
                .map(|&(_, target)| target)
                .collect(),
            isa_mode_conflicts: self.isa_mode_conflicts,
            interior_branch_targets: self.interior_branch_targets,
            link_register_seated: self.link_register_seated,
            tail_call_seated: self.tail_call_seated,
            function_isa_bit,
        })
    }
}

#[cfg(test)]
mod tests {
    use petgraph::visit::{EdgeRef, IntoEdgeReferences};

    use crate::test_support::*;
    use crate::types::{PcodeInsnAddr, Region, RegionTerminator};

    /// x86-64 `movabs rax, 0`: ten bytes, so `base + 5` is interior to it and
    /// not an instruction boundary.
    const MOVABS_RAX_0: [u8; 10] = [0x48, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0];

    /// The pcode index of the `BranchIndirect` in the machine instruction at
    /// `at`.
    fn branch_indirect_index(bytes: &[u8], base: u64, at: u64) -> u64 {
        let mut sleigh = make_sleigh_over(bytes.to_vec(), base);
        let lift = sleigh.lift_one(at).expect("lift_one");
        lift.insns
            .iter()
            .position(|i| i.opcode == rsleigh::Opcode::BranchIndirect)
            .expect("no BranchIndirect at that address") as u64
    }

    fn build_cfg(
        bytes: Vec<u8>,
        base: u64,
        options: &crate::CfgOptions,
    ) -> crate::Result<crate::Cfg> {
        let arch = strider_target::SleighArch::x86_64();
        let mut sleigh = make_sleigh_over(bytes, base);
        super::Builder::for_arch(&arch, &mut sleigh, base, options).build()
    }

    /// Every `Switch` arm must resolve to a successor region starting exactly at
    /// it; the lifter errors out on a target with no arm.
    fn assert_every_switch_target_has_an_arm(cfg: &crate::Cfg) {
        for rid in cfg.region_ids() {
            let RegionTerminator::Switch { targets, .. } = cfg
                .region_graph()
                .node_weight(rid)
                .unwrap()
                .terminator
                .clone()
            else {
                continue;
            };
            let arms = cfg.switch_arm_regions(rid);
            for t in targets {
                assert!(
                    arms.contains_key(&PcodeInsnAddr::at_machine_start(t.addr)),
                    "switch target {:#x} has no successor region",
                    t.addr,
                );
            }
        }
    }

    /// A `je` into the middle of a ten-byte `movabs` is interior to the region
    /// but off every instruction boundary. It reaches `split_region` with no
    /// guard in front of it, so it must degrade rather than fail the build.
    #[test]
    fn direct_cond_branch_into_an_instruction_interior_still_builds() {
        let base = 0x1000u64;
        let mut bytes = MOVABS_RAX_0.to_vec();
        bytes.extend_from_slice(&[0x74, 0xf9]); // 0x100a: je 0x1005
        bytes.push(0xc3); // 0x100c: ret

        let cfg = build_cfg(bytes, base, &crate::CfgOptions::default())
            .expect("a branch into an instruction interior must not fail the build");
        // The conditional survives with both arms, so the lifter can still
        // recover its polarity.
        let cond = cfg
            .region_ids()
            .find(|&r| {
                matches!(
                    cfg.region_graph().node_weight(r).unwrap().terminator,
                    RegionTerminator::CondBranch { .. }
                )
            })
            .expect("cond-branch region");
        let succ = cfg.region_if(cond).expect("region_if");
        assert!(succ.if_true_region.is_some() && succ.if_false_region.is_some());
    }

    /// A branch target interior to a region but off every instruction boundary
    /// is seated on the region that OWNS those bytes.  Splitting that region at
    /// an EARLIER boundary hands the bytes to the second half, so the seated
    /// edge has to move with them; retargeting it to the first half the way
    /// every other incoming edge moves leaves the branch with no successor
    /// holding its target, and `region_if` reports no taken side.
    ///
    /// Byte layout, chosen so the seat is wired BEFORE the split under the LIFO
    /// work queue: the entry's taken arm (0x1011, explored last) branches back
    /// to the boundary 0x1002, while its fall-through arm seats 0x1005,
    /// interior to the `movabs` at 0x1002.
    #[test]
    fn a_split_before_a_seated_interior_target_keeps_the_edge_on_the_owning_half() {
        let base = 0x1000u64;
        let mut bytes = vec![0x31, 0xc0]; // 0x1000: xor eax, eax
        bytes.extend_from_slice(&MOVABS_RAX_0); // 0x1002..0x100c
        bytes.extend_from_slice(&[0x74, 0x03]); // 0x100c: je 0x1011
        bytes.extend_from_slice(&[0x74, 0xf5]); // 0x100e: je 0x1005
        bytes.push(0xc3); // 0x1010: ret
        bytes.extend_from_slice(&[0xeb, 0xef]); // 0x1011: jmp 0x1002

        let cfg = build_cfg(bytes, base, &crate::CfgOptions::default()).expect("build");

        let seat_parent = cfg
            .region_ids()
            .find(|&r| {
                cfg.region_graph().node_weight(r).unwrap().terminator
                    == RegionTerminator::CondBranch {
                        true_target: addr(0x1005, 0),
                    }
            })
            .expect("region conditionally branching to 0x1005");

        let successors: std::collections::BTreeSet<PcodeInsnAddr> = cfg
            .region_graph()
            .neighbors(seat_parent)
            .map(|s| cfg.region_graph().node_weight(s).unwrap().start_addr)
            .collect();
        assert_eq!(
            successors,
            std::collections::BTreeSet::from([addr(0x1002, 0), addr(0x1010, 0)]),
            "taken arm must be the half that owns 0x1005, fall-through the ret at 0x1010"
        );

        let succ = cfg.region_if(seat_parent).expect("region_if");
        let taken = succ.if_true_region.expect("taken side");
        assert!(
            cfg.region_graph()
                .node_weight(taken)
                .unwrap()
                .contains_addr(addr(0x1005, 0))
        );
        assert_eq!(
            cfg.region_graph()
                .node_weight(succ.if_false_region.expect("fall-through side"))
                .unwrap()
                .start_addr,
            addr(0x1010, 0)
        );
    }

    /// The same interior target, but reached when the multi-byte instruction is
    /// its region's LAST one: the `nop` seals the region right after `movabs`,
    /// so the span has to cover the `movabs` bytes or the guard never sees the
    /// target and a fresh region decodes mid-immediate.
    #[test]
    fn direct_cond_branch_into_the_last_instructions_interior_still_builds() {
        let base = 0x1000u64;
        let mut bytes = MOVABS_RAX_0.to_vec();
        bytes.push(0x90); // 0x100a: nop, zero pcode ops: seals the movabs region
        bytes.extend_from_slice(&[0x74, 0xf8]); // 0x100b: je 0x1005
        bytes.push(0xc3); // 0x100d: ret

        let cfg = build_cfg(bytes, base, &crate::CfgOptions::default())
            .expect("a branch into the last instruction's interior must not fail the build");
        let cond = cfg
            .region_ids()
            .find(|&r| {
                matches!(
                    cfg.region_graph().node_weight(r).unwrap().terminator,
                    RegionTerminator::CondBranch { .. }
                )
            })
            .expect("cond-branch region");
        let succ = cfg.region_if(cond).expect("region_if");
        assert!(succ.if_true_region.is_some() && succ.if_false_region.is_some());
    }

    /// A resolved jump-table arm landing inside the LAST instruction of an
    /// already-decoded region. The bytes there decode cleanly, so nothing errors
    /// out; the arm must still be dropped rather than seated on a second region
    /// overlapping the `movabs`.
    #[test]
    fn switch_target_interior_to_a_regions_last_instruction_is_dropped() {
        let base = 0x1000u64;
        let mut bytes = vec![0xff, 0xe0]; // 0x1000: jmp rax
        let mut movabs = MOVABS_RAX_0; // 0x1002..0x100c: movabs rax, imm64
        movabs[3] = 0xc3; // the immediate byte at 0x1005 decodes as `ret`
        bytes.extend_from_slice(&movabs);
        bytes.push(0x90); // 0x100c: nop; seals the movabs region
        bytes.push(0xc3); // 0x100d: ret

        let branch = PcodeInsnAddr {
            machine_addr: base.into(),
            insn_index: branch_indirect_index(&bytes, base, base),
        };
        let mut known_targets = rustc_hash::FxHashMap::default();
        known_targets.insert(
            branch,
            crate::ResolvedTargets::Multiple(vec![0x1005.into(), 0x1002.into()]),
        );
        let opts = crate::CfgOptions {
            known_targets,
            ..crate::CfgOptions::default()
        };

        let cfg = build_cfg(bytes, base, &opts)
            .expect("a table target off an instruction boundary must not fail the build");
        assert_every_switch_target_has_an_arm(&cfg);
        assert!(
            !cfg.regions()
                .any(|r| r.start_addr.machine_addr.addr == 0x1005),
            "a region was decoded inside the movabs immediate: {:?}",
            cfg.regions()
                .map(|r| (r.start_addr, r.terminator.clone()))
                .collect::<Vec<_>>(),
        );
        let targets = cfg
            .regions()
            .find_map(|r| match &r.terminator {
                RegionTerminator::Switch { targets, .. } => Some(targets.clone()),
                _ => None,
            })
            .expect("switch region");
        assert_eq!(
            targets.iter().map(|t| t.addr).collect::<Vec<_>>(),
            vec![0x1002],
            "the off-boundary arm must be dropped from the table",
        );
    }

    /// A resolved jump-table target interior to the switch's OWN region: the
    /// seat-time guard runs before `finish_current_region`, so no region owns
    /// the address yet and the target slips through.
    #[test]
    fn switch_target_interior_to_its_own_region_still_builds() {
        let base = 0x1000u64;
        let mut bytes = MOVABS_RAX_0.to_vec();
        bytes.extend_from_slice(&[0xff, 0xe0]); // 0x100a: jmp rax
        bytes.push(0xc3); // 0x100c: ret

        let branch = PcodeInsnAddr {
            machine_addr: 0x100a.into(),
            insn_index: branch_indirect_index(&bytes, base, 0x100a),
        };
        let mut known_targets = rustc_hash::FxHashMap::default();
        known_targets.insert(
            branch,
            crate::ResolvedTargets::Multiple(vec![0x1005.into(), 0x100c.into()]),
        );
        let opts = crate::CfgOptions {
            known_targets,
            ..crate::CfgOptions::default()
        };

        let cfg = build_cfg(bytes, base, &opts)
            .expect("a table target off an instruction boundary must not fail the build");
        assert_every_switch_target_has_an_arm(&cfg);
    }

    /// Same defect, reached the other way: the target is on no boundary of a
    /// region decoded AFTER the switch was seated, so exploration order alone
    /// decides whether the site defers or explodes.
    #[test]
    fn switch_target_interior_to_a_later_decoded_region_still_builds() {
        let base = 0x1000u64;
        let mut bytes = vec![0xff, 0xe0]; // 0x1000: jmp rax
        bytes.extend_from_slice(&MOVABS_RAX_0); // 0x1002..0x100b
        bytes.push(0xc3); // 0x100c: ret

        let branch = PcodeInsnAddr {
            machine_addr: base.into(),
            insn_index: branch_indirect_index(&bytes, base, base),
        };
        let mut known_targets = rustc_hash::FxHashMap::default();
        // Supplied high-first: the enqueue sorts, so 0x1002 is still explored
        // first and its region then covers 0x1005.
        known_targets.insert(
            branch,
            crate::ResolvedTargets::Multiple(vec![0x1005.into(), 0x1002.into()]),
        );
        let opts = crate::CfgOptions {
            known_targets,
            ..crate::CfgOptions::default()
        };

        let cfg = build_cfg(bytes, base, &opts)
            .expect("a table target off an instruction boundary must not fail the build");
        assert_every_switch_target_has_an_arm(&cfg);
    }

    /// A direct edge needs exactly one var of the parent's context, so a second
    /// full snapshot of `source_addr` is wasted work on every CFG edge.
    #[test]
    fn enqueue_reads_one_parent_context_var_per_edge() {
        use crate::builder::flow::CONTEXT_READS;
        let arch = strider_target::SleighArch::arm();
        let reader = rsleigh::mem_readers::BufMemReader::new(vec![0u8; 0x100], 0x1000);
        let mut sleigh =
            rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("create Sleigh");
        let flow = crate::FlowVars::discover(&sleigh).expect("discover flow vars");

        CONTEXT_READS.with(|n| n.set(0));
        let _ = flow.snapshot(&sleigh, 0x1000);
        let per_snapshot = CONTEXT_READS.with(std::cell::Cell::get);
        assert!(per_snapshot > 1, "premise: the ARM sla flows several vars");

        let mut b =
            super::Builder::for_arch(&arch, &mut sleigh, 0x1000, &crate::CfgOptions::default())
                .with_flow_vars(&flow);
        CONTEXT_READS.with(|n| n.set(0));
        b.enqueue(None, addr(0x1000, 0), 0x1000);
        assert_eq!(
            CONTEXT_READS.with(std::cell::Cell::get),
            per_snapshot + 1,
            "one snapshot of the target plus the parent's ISA-mode var",
        );
    }

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
            empty_span_len: 0,
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
        // `build` segments at every zero-pcode-op instruction, so an
        // `Unconditional` region sealed at one owns no `RegionInstruction`
        // (`Region::insns`).
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let empty = Region {
            start_addr: addr(0x1000, 0),
            insns: Vec::new(),
            empty_span_len: 0,
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
            empty_span_len: 0,
            terminator: RegionTerminator::TailCall {
                target: 0x9000.into(),
            },
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

    /// A region starting inside another's last instruction is the greatest
    /// start at or below the rest of that instruction, so confirming
    /// containment on the last start alone reports those bytes unowned.
    #[test]
    fn find_region_at_addr_shadowed_by_an_overlapping_region() {
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let mut wide = make_region(&[(0x1000, 0)]);
        wide.insns[0].len = 10;
        let id = b.add_region(wide).unwrap();
        b.add_region(make_region(&[(0x1003, 0)])).unwrap();
        assert_eq!(
            b.find_region_containing_addr(addr(0x1006, 0))
                .map(|(i, _)| i),
            Some(id)
        );
    }

    /// A bound taken over ALL region spans never shrinks, so one long
    /// straight-line region makes every later lookup MISS scan every start in
    /// that window.  Only a shadowed region can own an address the greatest
    /// start below it does not.
    #[test]
    fn a_lookup_miss_over_disjoint_regions_probes_one_region() {
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let mut wide = make_region(&[(0x1000, 0)]);
        wide.insns[0].len = 0x1000;
        b.add_region(wide).unwrap();
        for i in 0..64u64 {
            b.add_region(make_region(&[(0x2000 + i, 0)])).unwrap();
        }

        super::REGION_PROBES.with(|n| n.set(0));
        assert!(b.find_region_containing_addr(addr(0x2100, 0)).is_none());
        assert_eq!(super::REGION_PROBES.with(std::cell::Cell::get), 1);
    }

    /// The other overlap direction: sequential decoding steps OVER an existing
    /// region's start (interior to one of the instructions decoded here) and
    /// produces a region covering it.  The later, shorter start shadows the
    /// wider region, which owns the bytes past it.
    #[test]
    fn find_region_at_addr_shadowed_by_an_earlier_added_region() {
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        b.add_region(make_region(&[(0x1005, 0)])).unwrap();
        let mut wide = make_region(&[(0x1000, 0)]);
        wide.insns[0].len = 0x10;
        let id = b.add_region(wide).unwrap();
        assert_eq!(
            b.find_region_containing_addr(addr(0x100c, 0))
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

    /// A `Switch` whose last over-approximated arm lands off every instruction
    /// boundary demotes to `UnresolvedIndirectBranch` rather than surviving as
    /// a table with zero arms, and carries the dispatch address the terminator
    /// recorded so a later round re-seats the same site.
    ///
    /// The recorded dispatch differs from the region's last instruction, so the
    /// assertion pins which of the two the demotion uses.
    #[test]
    fn emptied_switch_table_demotes_to_unresolved_at_the_dispatch_addr() {
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let target_vn = rsleigh::Vn {
            addr_space: rsleigh::VnSpace::REGISTER,
            addr_off: 0x38,
            size: 8,
        };
        // The dispatch is the second p-code op of the instruction at 0x1004,
        // so it is NOT `at_machine_start` of the region's last instruction.
        let dispatch = addr(0x1004, 2);
        let mut dispatcher = make_region(&[(0x1000, 0), (0x1004, 2), (0x1004, 3)]);
        dispatcher.terminator = RegionTerminator::Switch {
            target_vn,
            targets: vec![crate::ResolvedTarget::new(0x2002, None)],
            addr: dispatch,
        };
        let parent = b.add_region(dispatcher).unwrap();
        // 0x2002 is interior to this region but on no instruction boundary.
        let owner = b
            .add_region(make_region(&[(0x2000, 0), (0x2008, 0)]))
            .unwrap();
        let edges_before = b.region_graph.edge_references().count();

        b.seat_non_boundary_target(parent, owner, addr(0x2002, 0))
            .unwrap();

        assert_eq!(
            b.region_graph.node_weight(parent).unwrap().terminator,
            RegionTerminator::UnresolvedIndirectBranch {
                target_vn,
                addr: dispatch,
            },
            "an emptied table demotes, keeping the dispatch varnode and address"
        );
        assert_eq!(
            b.region_graph.edge_references().count(),
            edges_before,
            "a demoted site wires no arm edge"
        );
    }

    /// One address seeded from two dispatch sites: the failure belongs to the
    /// site that failed. Stripping by machine address takes the other site's
    /// arm too, and where that empties its table the demoted
    /// `UnresolvedIndirectBranch` keeps a live outgoing edge, which its
    /// contract forbids.
    #[test]
    fn an_undecodable_arm_is_dropped_only_from_the_site_that_seeded_it() {
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let target_vn = rsleigh::Vn {
            addr_space: rsleigh::VnSpace::REGISTER,
            addr_off: 0x38,
            size: 8,
        };
        let shared = 0x2000u64;
        let switch_region = |dispatch| {
            let mut r = make_region(&[(dispatch, 0)]);
            r.terminator = RegionTerminator::Switch {
                target_vn,
                targets: vec![crate::ResolvedTarget::new(shared, None)],
                addr: addr(dispatch, 0),
            };
            r
        };
        let failed = b.add_region(switch_region(0x1000)).unwrap();
        let live = b.add_region(switch_region(0x1100)).unwrap();
        let arm = b.add_region(make_region(&[(shared, 0)])).unwrap();
        b.region_graph.add_edge(live, arm, ());
        // The failing site wired its arm edge before the decode that errored.
        b.region_graph.add_edge(failed, arm, ());
        b.undecodable_seeded.push((Some(failed), addr(shared, 0)));

        b.drop_undecodable_switch_arms();

        assert_eq!(
            b.region_graph.node_weight(failed).unwrap().terminator,
            RegionTerminator::UnresolvedIndirectBranch {
                target_vn,
                addr: addr(0x1000, 0),
            },
            "the seeding site loses its only arm and re-defers"
        );
        assert!(
            b.region_graph.find_edge(failed, arm).is_none(),
            "a re-deferred site keeps no outgoing edge"
        );
        assert_eq!(
            b.region_graph.node_weight(live).unwrap().terminator,
            RegionTerminator::Switch {
                target_vn,
                targets: vec![crate::ResolvedTarget::new(shared, None)],
                addr: addr(0x1100, 0),
            },
            "the other site decoded the same address in its own context"
        );
        assert!(
            b.region_graph.find_edge(live, arm).is_some(),
            "the surviving arm keeps its edge"
        );
    }

    /// Only a `Switch` resolves arms by exact start address; every other
    /// terminator edges to the region that OWNS the interior address.
    #[test]
    fn non_switch_parent_edges_to_the_owning_region() {
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let parent = b.add_region(make_region(&[(0x1000, 0)])).unwrap();
        let owner = b
            .add_region(make_region(&[(0x2000, 0), (0x2008, 0)]))
            .unwrap();

        b.seat_non_boundary_target(parent, owner, addr(0x2002, 0))
            .unwrap();

        assert!(
            b.region_graph.find_edge(parent, owner).is_some(),
            "a non-Switch parent keeps its edge to the owning region"
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
}
