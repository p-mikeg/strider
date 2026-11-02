mod flow;
mod region_builder;
mod split;
#[cfg(test)]
mod tests;

pub(crate) use flow::{FlowContext, FlowVars, hold_var};
use region_builder::{DecodeFailure, RegionBuilder, decode_failure};
pub(crate) use strider_target::FlowCommit;

use std::collections::BTreeMap;

use petgraph::graph::NodeIndex;
use petgraph::stable_graph::EdgeIndex;

use crate::Cfg;
use crate::DecodeEngine;
use crate::options::CfgOptions;
use crate::region_index::RegionIndex;
use crate::types::{PcodeInsnAddr, Region, RegionGraph, RegionTerminator};
use anyhow::{anyhow, bail};

use crate::{Reason, Result};

/// A pending region to explore off a successor edge.
pub(super) struct WorkItem {
    /// The region edging into this one, or `None` for the entry.
    pub(super) parent: Option<NodeIndex>,
    pub(super) addr: PcodeInsnAddr,
    /// The context this target decodes in: what Sleigh flowed to it for a
    /// direct edge, else the function mode carrying the resolved branch's
    /// ISA-mode bit. Pinned before decode, undoing a forward-hold clobber.
    pub(super) carried: FlowContext,
    /// The seeded arm this target descends from, `None` when the entry reaches
    /// it through direct edges alone. Such a target that will not decode is a
    /// real error; one under an arm may be the tail of a misclassified
    /// jump-table entry, so the arm is dropped and reported instead.
    pub(super) seeded_by: Option<ArmSeed>,
}

/// A seeded `Switch` arm, carried down every edge decoded off it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ArmSeed {
    /// The indirect branch the arm was seated from.
    pub(super) site: PcodeInsnAddr,
    /// The region the site sealed as a `Switch`.
    pub(super) switch: NodeIndex,
    pub(super) arm: PcodeInsnAddr,
}

impl ArmSeed {
    /// Whether the item at `addr` off `parent` is the arm itself rather than
    /// code decoded off it.
    fn is_arm(&self, parent: Option<NodeIndex>, addr: PcodeInsnAddr) -> bool {
        parent == Some(self.switch) && self.arm == addr
    }
}

/// Incrementally constructs a [`Cfg`] from a binary entry point, on the
/// engine [`DecodeEngine::builder`] pinned for it.
///
/// A work queue seeded with the entry address drives decoding: each item
/// either decodes a new region or routes an edge to an existing one. A branch
/// landing mid-region splits that region in two.
pub(crate) struct Builder<'a, R: rsleigh::MemReader> {
    /// The engine this build decodes on: its Sleigh, its arch, its flowing
    /// context vars and its per-preset default tables, each documented there.
    pub(super) engine: &'a mut DecodeEngine<R>,
    pub(super) start_addr: u64,
    pub(super) options: &'a CfgOptions<'a>,
    pub(super) region_graph: RegionGraph,
    /// Which region owns an address.
    pub(super) index: RegionIndex,
    /// LIFO for depth-first exploration, direct edges only.
    pub(super) work_queue: Vec<WorkItem>,
    /// LIFO for seeded arms, drained only once no direct edge is left; see
    /// [`Self::next_work_item`].
    pub(super) seeded_queue: Vec<WorkItem>,
    /// This function's constant ISA mode, the base context a strider-resolved
    /// target decodes in ([`Self::enqueue_resolved`]).
    pub(super) function_mode: FlowContext,
    /// `inst_next` of every instruction THIS build decoded.
    ///
    /// Not "every address this build committed at": nothing here checks
    /// whether the instruction committed anything, and a constructor can
    /// commit at an address that is not its own `inst_next` (ARM `blx #imm`
    /// commits `TMode` at the branch TARGET). What makes a snapshot here
    /// trustworthy is that `RegionBuilder` re-derives the context at each
    /// `inst_next` from the decode that recorded it, so the value the engine
    /// holds there is this build's whatever an earlier one left. Anywhere else
    /// [`Self::enqueue`] falls back to `function_mode`.
    ///
    /// That decode is not always one the build KEEPS: a fall-through onto an
    /// existing region start, and an instruction `lift_code` then rejects as
    /// `data_ranges`, both record their `inst_next` before being discarded. The
    /// re-derivation still ran, so the value is some decode of this build's own
    /// bytes rather than a stale one, but it is not necessarily the context the
    /// owning region decodes `addr` in.
    pub(super) committed_next: rustc_hash::FxHashSet<u64>,
    /// The seeded arm the region under construction descends from, which every
    /// edge it enqueues inherits.
    pub(super) descent: Option<ArmSeed>,
    /// Seeded arms that, or whose code, would not decode, each with the
    /// `Switch` region that seated it; see [`Cfg::undecodable_seeded_targets`].
    /// Keyed on the SITE, not the address alone: two explorations of one
    /// address decode in different contexts by construction, so a failure at
    /// one site says nothing about the same address reached from another.
    pub(super) undecodable_seeded: Vec<(NodeIndex, crate::UndecodableTarget)>,
    /// The ISA-mode bit flowing into each indirect-branch site, sampled at the
    /// site's seal; see [`Self::sample_flowing_isa_bit`].
    pub(super) flowing_isa_bits: BTreeMap<PcodeInsnAddr, bool>,
    /// What this build cannot vouch for; see [`Cfg::incomplete`].
    pub(super) incomplete: crate::Incompleteness,
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
    /// Seeded arms whose address an earlier decode already owns in the other
    /// ISA mode, with the region that seated them. The arm goes, as the loser
    /// of [`Self::next_work_item`]'s arbitration.
    pub(super) clashing_seeded: Vec<(NodeIndex, PcodeInsnAddr)>,
}

impl<'a, R: rsleigh::MemReader> Builder<'a, R> {
    /// A build of the function at `start_addr` on `engine`, which
    /// [`DecodeEngine::builder`] has pinned to `function_mode`.
    pub(crate) fn new(
        engine: &'a mut DecodeEngine<R>,
        start_addr: u64,
        function_mode: FlowContext,
        options: &'a CfgOptions<'a>,
    ) -> Self {
        Self {
            engine,
            start_addr,
            options,
            region_graph: RegionGraph::new(),
            index: RegionIndex::default(),
            work_queue: Vec::new(),
            seeded_queue: Vec::new(),
            clashing_seeded: Vec::new(),
            undecodable_seeded: Vec::new(),
            flowing_isa_bits: BTreeMap::new(),
            incomplete: crate::Incompleteness::default(),
            non_boundary_seats: rustc_hash::FxHashMap::default(),
            committed_next: rustc_hash::FxHashSet::default(),
            link_register_seated: Vec::new(),
            tail_call_seated: Vec::new(),
            function_mode,
            descent: None,
        }
    }

    /// Reports `addr` as reached in two ISA modes when `want` differs from
    /// the mode the region `existing` decoded in, and says whether it did.
    ///
    /// An address an existing region owns is reused and the arriving edge's
    /// mode dropped, so for bytes reachable in two ISA modes the first arrival
    /// to decode them wins: under the LIFO work queue, the last-enqueued edge.
    /// Which one that is depends on queue order, so the loser's path would
    /// silently decode in the wrong ISA; recording the clash is what keeps the
    /// answer honest.
    ///
    /// Every seal that wires its own edge needs this too: that edge never
    /// passes through [`Self::explore`], so the check there never sees it and
    /// an ARM region running into a Thumb-decoded region would reach the
    /// caller as a clean answer.
    pub(super) fn note_isa_mode_clash(
        &mut self,
        addr: PcodeInsnAddr,
        existing: NodeIndex,
        want: Option<u32>,
    ) -> bool {
        let clash = matches!(
            (want, self.region_graph[existing].isa_mode),
            (Some(want), Some(decoded)) if want != decoded
        );
        if clash {
            self.incomplete.insert(addr, Reason::IsaModeConflict);
        }
        clash
    }

    /// Enqueue `addr` as a *direct* successor of the region at `source_addr`
    /// (branch or fall-through), capturing the context Sleigh flowed to it.
    ///
    /// `flowing` is the parent region's live value for each of its arch's
    /// FLOWING committed vars (`RegionBuilder::flowing_commits`), empty for
    /// the entry, which has no parent to carry anything from.
    ///
    /// The item inherits the region's [`Self::descent`]: an address a direct
    /// edge names is code only when that region is, and a region decoded off a
    /// seeded arm is only as certain as the arm.
    pub(super) fn enqueue(
        &mut self,
        parent: Option<NodeIndex>,
        addr: PcodeInsnAddr,
        isa_mode: Option<u32>,
        flowing: &[(&'static str, u32)],
    ) {
        // Per-instruction decode state (ARM/Thumb ITSTATE, MIPS16 register-list
        // iteration) flows only to `inst_next`, and the edges between an IT
        // block's regions carry what the `IT` committed. So read the engine,
        // but only where `committed_next` vouches for it; elsewhere the
        // snapshot is a previous `build_cfg`'s, and `pin_at` would adopt that
        // stale `globalset` (the entry pin's forward paint stops at its
        // commit). An arch naming no `flow_commit_vars` re-derives nothing, and
        // its out-of-band `is_benign_prefix_reentry` decode commits too, so it
        // takes the function's own baseline, as `enqueue_resolved` does.
        // A direct edge switches no ISA mode, so the child decodes in the
        // mode its parent region decoded in, which need not be the entry's.
        // Never the engine's value at the parent's start: an interworking
        // call commits the mode at its target, and a callee below the caller
        // paints forward over the caller's own bytes.
        let bit = isa_mode.map(|mode| mode != 0);
        let carried = if !self.engine.flow_commit.is_empty()
            && self.committed_next.contains(&addr.machine_addr.addr)
        {
            let snapshot = self
                .engine
                .flow_vars
                .snapshot(&self.engine.sleigh, addr.machine_addr.addr);
            self.carry(snapshot, &[], bit)
        } else {
            // The baseline is the entry's pspec defaults, and a FLOWING
            // committed var must not degrade to them: it was painted forward
            // to persist, and `pin_at` would write the default over the live
            // paint. An ARM `enterx` ahead of a forward `b` lost its ThumbEE
            // mode that way, and the target decoded as a plain-Thumb store.
            // A `noflow` var does degrade, being confined to one address:
            // carrying a live IT state forward predicates the code after it.
            self.carry(self.function_mode.clone(), flowing, bit)
        };
        self.work_queue.push(WorkItem {
            parent,
            addr,
            carried,
            seeded_by: self.descent,
        });
    }

    /// Enqueue `addr` as a strider-*resolved indirect* target of the `Switch`
    /// region `parent`, resolved from the branch at `branch_site`.  Sleigh
    /// never flowed the mode here, so decode in the function mode (default
    /// transients: unlike a direct edge, no source region's context reaches a
    /// resolved target, so IT-block and register-list state start fresh) with
    /// the ISA-mode var set to `bit`, what the branch commits: an interworking
    /// branch's own bit, else the mode flowing INTO the branch
    /// ([`Self::sample_flowing_isa_bit`]), so a `mov pc` in a Thumb region of
    /// an otherwise-ARM function keeps Thumb.
    ///
    /// `flowing` is the branch site's live value for each FLOWING committed var
    /// (`RegionBuilder::flowing_commits`), applied over the function mode as
    /// [`Self::enqueue`] applies it: such a var was painted forward to persist,
    /// so degrading it to the entry default would write that default over the
    /// live paint at the target. A `noflow` var does degrade, as there it is
    /// confined to the one address its commit covers.
    pub(super) fn enqueue_resolved(
        &mut self,
        parent: NodeIndex,
        addr: PcodeInsnAddr,
        bit: Option<bool>,
        branch_site: PcodeInsnAddr,
        flowing: &[(&'static str, u32)],
    ) {
        let carried = self.carry(self.function_mode.clone(), flowing, bit);
        self.seeded_queue.push(WorkItem {
            parent: Some(parent),
            addr,
            carried,
            seeded_by: Some(ArmSeed {
                site: branch_site,
                switch: parent,
                arm: addr,
            }),
        });
    }

    /// `ctx` carrying each FLOWING committed var's live value from `flowing`,
    /// then, on an arch with an ISA-mode var, the mode `bit`.
    fn carry(
        &self,
        mut ctx: FlowContext,
        flowing: &[(&'static str, u32)],
        bit: Option<bool>,
    ) -> FlowContext {
        let vars = &self.engine.flow_vars;
        for &(name, value) in flowing {
            vars.set_var(&mut ctx, name, value);
        }
        if let (Some(var), Some(bit)) = (self.engine.arch.isa_mode_var(), bit) {
            vars.set_var(&mut ctx, var, u32::from(bit));
        }
        ctx
    }

    /// The next target to explore, every direct edge before any seeded arm.
    ///
    /// Which edge decodes an address first owns its bytes, and both queues are
    /// LIFO, so order is what arbitrates two edges reaching one address in two
    /// ISA modes. A direct edge switches no mode, making the parent region's
    /// mode certain, where an arm's committed `isa_bit` is a classifier claim;
    /// draining direct edges first hands the bytes to the proved mode.
    ///
    /// Proved only for code the entry reaches without an arm. Within one switch
    /// the arms pop lowest-first, and a region decoded off an arm explores its
    /// own direct successors before the next arm, so such an edge can arrive
    /// at bytes an earlier arm decoded in the other mode. It is no more certain
    /// than its own arm, and un-decoding the earlier region is not expressible,
    /// so the first decode keeps the bytes and the clash is only reported.
    fn next_work_item(&mut self) -> Option<WorkItem> {
        self.work_queue.pop().or_else(|| self.seeded_queue.pop())
    }

    /// An empty region is rejected unless its terminator is `Unconditional` (a
    /// region sealed at a zero-pcode-op instruction, which `build` segments at
    /// every one of) or `TailCall` (a [`Self::edge_to_stub`] stub, whose bytes are
    /// never decoded).
    pub(super) fn add_region(&mut self, region: Region) -> Result<NodeIndex> {
        if region.insns.is_empty()
            && !matches!(
                region.terminator,
                RegionTerminator::Unconditional | RegionTerminator::TailCall { .. }
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

        let region_id = self.region_graph.add_node(region);
        self.index.insert(&self.region_graph, region_id);
        Ok(region_id)
    }

    /// The ISA mode `ctx` carries, `None` on an arch with no mode var or when
    /// the context does not name it.
    ///
    /// The one projection of `isa_mode_var` onto a `FlowContext`; the region
    /// mode, the clash check and the entry-mode degrade are all this question
    /// asked at different contexts.
    fn isa_mode_of(&self, ctx: &FlowContext) -> Option<u32> {
        self.engine
            .arch
            .isa_mode_var()
            .and_then(|var| self.engine.flow_vars.value_of(ctx, var))
    }

    /// Records, and returns, the ISA-mode bit flowing into the indirect branch
    /// at `site`: the mode a target seated there with no mode of its own
    /// decodes in.
    ///
    /// Sampled at the site's seal, where `RegionBuilder::hold_isa_mode` has just
    /// re-imposed the owning region's mode at this address, and kept: reading
    /// [`crate::flowing_isa_bit_at`] again after the build answers for whichever
    /// region painted the address last. `None` on an arch with no ISA-mode var.
    pub(super) fn sample_flowing_isa_bit(&mut self, site: PcodeInsnAddr) -> Option<bool> {
        self.engine.arch.isa_mode_var()?;
        if let Some(&bit) = self.flowing_isa_bits.get(&site) {
            return Some(bit);
        }
        // Degrading to the function's own mode, not to `false`, which would
        // decode every resolved same-mode target of a Thumb function as ARM.
        let entry_bit = self
            .isa_mode_of(&self.function_mode)
            .is_some_and(|v| v != 0);
        let bit = crate::flowing_isa_bit_at(
            &self.engine.arch,
            &self.engine.sleigh,
            site.machine_addr.addr,
            entry_bit,
        );
        self.flowing_isa_bits.insert(site, bit);
        Some(bit)
    }

    /// Edges `from` to the empty region at `addr` an edge leaving this
    /// function's decoded bytes hangs off, created on first use.  Its
    /// producers: the out-of-function arm of a conditional branch, a direct
    /// branch target that is unmapped or holds no instruction, and a
    /// sequential fall-through onto either.  It is wired as a regular
    /// successor but never enqueued, so no address outside the function's
    /// bound and no address holding no instruction is decoded.
    ///
    /// Keyed through the index like any region, so two edges to the same
    /// address share one stub.
    pub(super) fn edge_to_stub(&mut self, from: NodeIndex, addr: PcodeInsnAddr) -> Result<()> {
        let stub = match self.index.get(addr) {
            Some(existing) => existing,
            None => self.add_region(Region {
                start_addr: addr,
                insns: Vec::new(),
                empty_span_len: 0,
                isa_mode: None,
                terminator: RegionTerminator::TailCall {
                    target: addr.machine_addr.addr.into(),
                },
            })?,
        };
        self.region_graph.add_edge(from, stub, ());
        Ok(())
    }

    /// The region owning `addr`; see [`RegionIndex::containing`].
    pub(super) fn find_region_containing_addr(
        &self,
        addr: PcodeInsnAddr,
    ) -> Option<(NodeIndex, &Region)> {
        self.index.containing(&self.region_graph, addr)
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
    /// not left believing the edge is exact; see
    /// [`Reason::InteriorTarget`].
    fn seat_non_boundary_target(
        &mut self,
        parent: NodeIndex,
        owner: NodeIndex,
        addr: PcodeInsnAddr,
    ) -> Result<()> {
        let region = &mut self.region_graph[parent];
        if !matches!(region.terminator, RegionTerminator::Switch { .. }) {
            // An x86 branch to an address one prefix into an instruction (a
            // `je` over a `lock`) re-enters the same instruction's decode, and
            // the edge is exact where that instruction is the owner's FIRST:
            // an owner starting earlier runs instructions the taken path does
            // not.
            let benign = self.is_benign_prefix_reentry(owner, addr);
            let edge = self.region_graph.add_edge(parent, owner, ());
            self.non_boundary_seats.insert(edge, addr);
            if !benign {
                self.incomplete.insert(addr, Reason::InteriorTarget);
            }
            return Ok(());
        }
        // One arm per seating, not every arm at the address: a table can name
        // one address in both ISA modes, and each arm enqueues its own work
        // item, so dropping them all at the first would leave the second
        // nothing to consume and read as the no-arm violation the demote
        // inside `drop_switch_arms_at` exists to avoid.
        let dropped = region
            .terminator
            .drop_switch_arms_at(addr.machine_addr.addr, true);
        debug_assert!(
            dropped,
            "seated interior target {addr:?} names no arm of the Switch that routed it",
        );
        // Reported whether or not an arm went with it: this path wires no edge,
        // so anything unsaid here leaves the site an arm short in silence.
        self.incomplete.insert(addr, Reason::InteriorTarget);
        Ok(())
    }

    /// Whether `addr` lands one instruction-prefix into `owner`'s stream and
    /// re-enters the same instruction: decoding from `addr` ends at the byte
    /// the containing instruction does and carries the same p-code once the
    /// atomic-prefix markers (`LOCK` / `UNLOCK`) are set aside.
    ///
    /// The containing instruction must be `owner`'s FIRST, glibc's
    /// `je 0f; lock; 0:` shape, where the prefixed instruction is the branch's
    /// own fall-through. An owner starting earlier runs every instruction
    /// before the prefix too, so the taken path is not the stream the branch
    /// jumps to.
    ///
    /// Gated on the arch declaring no ISA-mode var, so a throwaway decode
    /// cannot leak mode state, and so this cannot reach a preset whose
    /// commits `Builder::enqueue` believes (see
    /// `a_preset_that_re_derives_context_has_an_isa_mode_var`).
    ///
    /// The gate admits every arch without a mode var, AArch64 and PowerPC
    /// among them, and the length check is AFTER the decode, so it is not what
    /// keeps them out: `oneInstruction` rejects a misaligned address itself,
    /// which covers AArch64 (`define alignment=4`) but NOT PowerPC
    /// (`alignment=2`), where an even-offset probe does decode. Only a
    /// byte-granular instruction prefix produces the shape the length check
    /// accepts, so elsewhere an interior target is a real overlap.
    fn is_benign_prefix_reentry(&mut self, owner: NodeIndex, addr: PcodeInsnAddr) -> bool {
        // The second clause is what makes the disjointness with `enqueue`'s
        // snapshot LOCAL rather than a coincidence of two per-preset tables:
        // whatever `flow_commit_vars` grows to, a throwaway decode cannot run
        // where a snapshot is believed.
        if self.engine.arch.isa_mode_var().is_some() || !self.engine.flow_commit.is_empty() {
            return false;
        }
        let region = &self.region_graph[owner];
        let target = addr.machine_addr.addr;
        let Some(insn) = region
            .insns
            .iter()
            .rev()
            .find(|i| i.addr.machine_addr.addr <= target)
        else {
            return false;
        };
        let start = insn.addr.machine_addr.addr;
        let full_end = start.saturating_add(u64::from(insn.len));
        if !(start < target && target < full_end) {
            return false;
        }
        if region.start_addr != PcodeInsnAddr::at_machine_start(start) {
            return false;
        }
        // The owner already holds the containing instruction's p-code, one
        // entry per op, all stamped at `start`.
        let full = region
            .insns
            .iter()
            .filter(|i| i.addr.machine_addr.addr == start)
            .map(|i| &i.insn);
        let Ok(tail) = self.engine.sleigh.lift_one(target) else {
            return false;
        };
        // `checked_add`, so a tail ending at the top of the address space
        // cannot saturate onto a different `full_end` and read as benign.
        if (tail.machine_insn_len as u64).checked_add(target) != Some(full_end) {
            return false;
        }
        let core = |op: &&rsleigh::Insn| !self.is_atomic_marker(op);
        full.filter(core).eq(tail.insns.iter().filter(core))
    }

    /// Whether `op` is a `LOCK` / `UNLOCK` marker an x86 `lock` prefix
    /// brackets its instruction with: they wrap the same core op an
    /// unprefixed decode emits bare.
    fn is_atomic_marker(&self, op: &rsleigh::Insn) -> bool {
        op.opcode == rsleigh::Opcode::CallOther
            && crate::user_op_of(op, &self.engine.tables.user_op_names)
                .is_some_and(|(_, name)| name == "LOCK" || name == "UNLOCK")
    }

    /// Routes `addr` to the region that owns it, or decodes a new one; a branch
    /// into an existing region's interior splits it. Before decoding a fresh
    /// region the `carried` context this edge captured is pinned, so a
    /// strider-resolved or backward target still decodes in the mode that
    /// reaches it.
    fn explore(
        &mut self,
        parent_region: Option<NodeIndex>,
        addr: PcodeInsnAddr,
        carried: FlowContext,
        seeded_by: Option<ArmSeed>,
    ) -> Result<()> {
        let isa_mode = self.isa_mode_of(&carried);
        if let Some((region_id, region)) = self.find_region_containing_addr(addr) {
            let is_start = region.start_addr == addr;
            let is_boundary = region.contains_insn_at(addr);
            let mode_clash = self.note_isa_mode_clash(addr, region_id, isa_mode);
            let parent_region_id = parent_region
                .ok_or_else(|| anyhow!("non-entry work-queue item has no parent edge"))?;
            if mode_clash && seeded_by.is_some_and(|s| s.is_arm(parent_region, addr)) {
                // The bytes decoded in another mode, so this arm names a stream
                // that is not there. Wiring the edge anyway would leave the seat
                // disagreeing with its own `IsaModeConflict` inside one build.
                self.clashing_seeded.push((parent_region_id, addr));
                return Ok(());
            }
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
        // Undoes a sibling region's forward-hold clobber of this address.
        //
        // Several sla context vars alias the ISA-mode bit (ARM `TMode`, `T`,
        // `LowBitCodeMode`, `ISA_MODE` all at bit (0,0)), so the diff can
        // write an alias last and flip the intended mode.  Repairing that is
        // `RegionBuilder::hold_isa_mode`'s job: the `RegionBuilder` below
        // opens with a `lift_one` on this same address, and that repair runs
        // unconditionally where one here would be gated on `pin_at` having
        // written at all.
        self.engine
            .flow_vars
            .pin_at(&mut self.engine.sleigh, addr.machine_addr.addr, &carried)?;
        self.descent = seeded_by;
        let built = RegionBuilder::new(self, addr, parent_region, isa_mode).build();
        self.descent = None;
        built
    }

    /// Refuses every `Switch` arm from which one of `regions` is reachable
    /// without passing through another dispatch.
    ///
    /// Each of `regions` was decoded off a seeded arm and lost a successor
    /// that would not decode. Every direct edge is explored before any arm, so
    /// the entry reaches such a region only through arms, and each of those
    /// arms leads into the same undecodable bytes.
    ///
    /// The reverse walk STOPS at every `Switch`: an arm reaching the bad bytes
    /// only back through a dispatch (an outer table's arm into an inner table,
    /// an arm looping to its own dispatch header) is no evidence about its own
    /// bound, and expanding past the dispatch would refuse every ancestor of
    /// it.
    fn refuse_arms_reaching(&mut self, regions: impl Iterator<Item = NodeIndex>) {
        let mut stack: Vec<NodeIndex> = regions.collect();
        // Two sets: a dispatch is DISCOVERED, which is what bounds the walk,
        // but it does not REACH, or an outer table's arm into it would.
        let mut reaching: rustc_hash::FxHashSet<NodeIndex> = stack.iter().copied().collect();
        let mut discovered = reaching.clone();
        while let Some(region) = stack.pop() {
            for pred in self
                .region_graph
                .neighbors_directed(region, petgraph::Incoming)
            {
                if !discovered.insert(pred) {
                    continue;
                }
                if matches!(
                    self.region_graph[pred].terminator,
                    RegionTerminator::Switch { .. }
                ) {
                    continue;
                }
                reaching.insert(pred);
                stack.push(pred);
            }
        }
        for switch in self.region_graph.node_indices() {
            let RegionTerminator::Switch { addr: site, .. } = self.region_graph[switch].terminator
            else {
                continue;
            };
            for arm in self.region_graph.neighbors(switch) {
                if reaching.contains(&arm) {
                    self.undecodable_seeded.push((
                        switch,
                        crate::UndecodableTarget {
                            site,
                            target: self.region_graph[arm].start_addr,
                        },
                    ));
                }
            }
        }
    }

    /// Removes the `Switch` arm behind each target this build refused: one that
    /// would not decode, and one whose bytes a direct edge had already decoded
    /// in the other ISA mode.
    ///
    /// Every arm must resolve to a successor region starting exactly at it, and
    /// the lifter errors on one that does not. A refused target leaves such an
    /// arm behind, so it is stripped here and the site is reported through
    /// [`Cfg::undecodable_seeded_targets`] or
    /// [`Reason::IsaModeConflict`] instead.
    ///
    /// Only the site that refused loses the arm. The same address reached from
    /// another site decodes in that site's own context (`enqueue_resolved` pins
    /// the branch's committed ISA mode, `enqueue` carries the parent region's),
    /// so on ARM/Thumb or MIPS16 one can fail while the other is live, and
    /// stripping both would take an arm whose edge is wired. Within the site,
    /// every arm at the refused address goes, whatever its mode: an address
    /// seated in both modes is a contradiction, which the orchestrator drops
    /// before seating it.
    fn drop_refused_switch_arms(&mut self) {
        for (parent, bad) in self.undecodable_seeded.clone() {
            self.drop_switch_arm(parent, bad.target);
        }
        for (parent, target) in std::mem::take(&mut self.clashing_seeded) {
            self.drop_switch_arm(parent, target);
        }
    }

    /// Strips every arm at `target`'s machine address from the `Switch` at
    /// `site`, in either ISA mode, and their successor edges with them.
    ///
    /// Stripping the LAST arm re-defers the site to an
    /// `UnresolvedIndirectBranch`: a zero-arm `Switch` fails the whole
    /// function's lift, which is the outcome dropping the arm exists to avoid.
    ///
    /// An arm resolves to the region starting exactly at its address. Leaving
    /// the edge behind outlives the arm: a demoted `UnresolvedIndirectBranch`
    /// would carry an outgoing edge its contract forbids, and a `Switch` that
    /// kept its other arms would keep a successor it no longer names.
    fn drop_switch_arm(&mut self, site: NodeIndex, target: PcodeInsnAddr) {
        if !self.region_graph[site]
            .terminator
            .drop_switch_arms_at(target.machine_addr.addr, false)
        {
            return;
        }
        let Some(arm) = self.index.get(target) else {
            return;
        };
        while let Some(edge) = self.region_graph.find_edge(site, arm) {
            self.region_graph.remove_edge(edge);
            self.non_boundary_seats.remove(&edge);
        }
    }

    /// Removes every region no path from `entry` reaches.
    ///
    /// A dropped arm edge can orphan the region its arm decoded, and the
    /// lifter translates only the regions the entry reaches.
    fn remove_unreachable_regions(&mut self, entry: NodeIndex) {
        let mut dfs = petgraph::visit::Dfs::new(&self.region_graph, entry);
        while dfs.next(&self.region_graph).is_some() {}
        self.region_graph
            .retain_nodes(|_, region| dfs.discovered.contains(region.index()));
        self.index.retain_live(&self.region_graph);
    }

    pub(crate) fn build(mut self) -> Result<Cfg> {
        let lost_successor = self.drain()?;
        self.finish(lost_successor)
    }

    /// Decodes the entry and everything it reaches. Returns the regions
    /// decoded off an arm whose successor would not decode, each with that
    /// failure.
    fn drain(&mut self) -> Result<Vec<(NodeIndex, anyhow::Error)>> {
        let entry = PcodeInsnAddr::at_machine_start(self.start_addr);
        let mode = self.isa_mode_of(&self.function_mode);
        self.enqueue(None, entry, mode, &[]);
        let mut lost_successor = Vec::new();
        while let Some(WorkItem {
            parent: parent_region,
            addr: address,
            carried,
            seeded_by,
        }) = self.next_work_item()
        {
            let Err(e) = self.explore(parent_region, address, carried, seeded_by) else {
                continue;
            };
            self.record_failure(parent_region, address, seeded_by, e, &mut lost_successor)?;
        }
        Ok(lost_successor)
    }

    /// Triages a work item `explore` could not decode. One failure can both
    /// report its bytes and drop its arm, so these are checks in sequence, not
    /// one verdict. `Err` is a failure no report covers, or a stub that could
    /// not be built.
    fn record_failure(
        &mut self,
        parent_region: Option<NodeIndex>,
        address: PcodeInsnAddr,
        seeded_by: Option<ArmSeed>,
        e: anyhow::Error,
        lost_successor: &mut Vec<(NodeIndex, anyhow::Error)>,
    ) -> Result<()> {
        let is_arm = seeded_by.is_some_and(|s| s.is_arm(parent_region, address));
        // A direct branch out of the mapped image (a firmware window, a
        // partially-mapped file, an unrelocated `jmp`) names bytes nobody
        // can supply, and one to bytes that hold no instruction (glibc's
        // PowerPC `abort` word, a literal pool) names nothing to decode.
        // The function is not broken by either, so the edge leaves through
        // a `TailCall` stub and the address is reported; every region that
        // did decode survives. Undecodable bytes under a seeded arm are the
        // arm's misclassification instead, handled below.
        //
        // The ENTRY has no parent to hang the stub off, and a function
        // whose first instruction does not exist has nothing to analyse,
        // so it stays an `Err`.
        if !is_arm && let Some(parent) = parent_region {
            let here = decode_failure(&e).filter(|f| f.addr() == address.machine_addr.addr);
            let reason = match here {
                Some(DecodeFailure::Unmapped(_)) => Some(Reason::UnmappedTarget),
                Some(DecodeFailure::NotCode { .. }) if seeded_by.is_none() => {
                    Some(Reason::UndecodableTarget)
                }
                _ => None,
            };
            if let Some(reason) = reason {
                self.incomplete.insert(address, reason);
                self.edge_to_stub(parent, address)?;
                return Ok(());
            }
        }
        // Under a seeded arm it is a misclassification, not a broken
        // function: drop the arm and report it against the site that named
        // it, so the caller freezes that site alone.
        let Some(seed) = seeded_by else {
            return Err(e);
        };
        // The bytes themselves, which `UndecodableTarget` cannot name: it
        // carries the ARM, and an arm that LED to the bytes decodes. Read
        // off the error rather than from `address`, the failure being able to
        // sit several instructions into the region this item decoded. A
        // failure carrying its own address (past the end, unmapped, not code)
        // is named there; any other names the item itself.
        let (at, reason) = match decode_failure(&e) {
            // The unmapped byte IS the one asked for: report the pcode
            // address, which carries the insn index the gap address cannot.
            Some(DecodeFailure::Unmapped(gap)) if gap == address.machine_addr.addr => {
                (address, Reason::UnmappedTarget)
            }
            Some(DecodeFailure::Unmapped(gap)) => {
                (PcodeInsnAddr::at_machine_start(gap), Reason::UnmappedTarget)
            }
            Some(DecodeFailure::PastEnd(end)) => (end, Reason::PastFunctionEnd),
            Some(DecodeFailure::NotCode { addr: bad, .. }) => (
                PcodeInsnAddr::at_machine_start(bad),
                Reason::UndecodableTarget,
            ),
            None => (address, Reason::UndecodableTarget),
        };
        self.incomplete.insert(at, reason);
        self.undecodable_seeded.push((
            seed.switch,
            crate::UndecodableTarget {
                site: seed.site,
                target: seed.arm,
            },
        ));
        if let Some(parent) = parent_region.filter(|_| !is_arm) {
            lost_successor.push((parent, e));
        }
        Ok(())
    }

    /// Drops the arms the lost successors refuse and every region the entry no
    /// longer reaches, then seals the cfg.
    fn finish(mut self, lost_successor: Vec<(NodeIndex, anyhow::Error)>) -> Result<Cfg> {
        self.refuse_arms_reaching(lost_successor.iter().map(|(region, _)| *region));
        self.drop_refused_switch_arms();
        let start_addr = PcodeInsnAddr::at_machine_start(self.start_addr);
        let (starting_region, _) = self.find_region_containing_addr(start_addr).ok_or_else(
            || {
                anyhow!(
                    "cfg build completed but no region contains the entry address {start_addr:?}; \
                     check that the entry is decodable"
                )
            },
        )?;
        self.remove_unreachable_regions(starting_region);
        // Defence: every path to such a region runs through a refused arm.
        if let Some((_, e)) = lost_successor
            .into_iter()
            .find(|(region, _)| self.region_graph.contains_node(*region))
        {
            return Err(e);
        }
        // No resolver ran, so every dispatch left without an edge is
        // unresolved and every seated answer unverified.
        for region in self.region_graph.node_weights() {
            match &region.terminator {
                RegionTerminator::UnresolvedIndirectBranch { addr, .. } => {
                    self.incomplete.insert(*addr, Reason::UnresolvedBranch);
                }
                RegionTerminator::Switch { addr, .. } => {
                    self.incomplete.insert(*addr, Reason::UnverifiedSeed);
                }
                _ => {}
            }
        }
        for &addr in self
            .link_register_seated
            .iter()
            .chain(&self.tail_call_seated)
        {
            self.incomplete.insert(addr, Reason::UnverifiedSeed);
        }

        let mut reported = rustc_hash::FxHashSet::default();
        Ok(Cfg {
            region_graph: self.region_graph,
            entry: starting_region,
            index: self.index,
            undecodable_seeded: self
                .undecodable_seeded
                .into_iter()
                .map(|(_, bad)| bad)
                .filter(|&bad| reported.insert(bad))
                .collect(),
            incomplete: self.incomplete,
            link_register_seated: self.link_register_seated,
            tail_call_seated: self.tail_call_seated,
            flowing_isa_bits: self.flowing_isa_bits,
            space_ids: self.engine.sleigh.space_ids(),
        })
    }
}
