mod flow;
mod region_builder;
mod split;
#[cfg(test)]
mod tests;

pub(crate) use flow::{FlowContext, FlowVars, hold_var};
use petgraph::visit::EdgeRef;
use region_builder::{DecodeFailure, RegionBuilder, decode_failure, is_call};
pub(crate) use strider_target::FlowCommit;

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
    /// The context this target decodes in, pinned before decode.
    pub(super) carried: FlowContext,
    /// The seeded arm this target descends from. A failure to decode under an
    /// arm drops the arm instead of failing the build.
    pub(super) seeded_by: Option<ArmSeed>,
}

/// A seeded `Switch` arm, carried down every edge decoded off it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ArmSeed {
    /// The region the indirect branch sealed as a `Switch`.
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

/// Builds a [`Cfg`] from a work queue seeded with the entry: each item decodes
/// a new region or edges to an existing one, splitting it when the edge lands
/// mid-region.
pub(crate) struct Builder<'a, R: rsleigh::MemReader> {
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
    /// This function's ISA mode, the base context of a resolved target.
    pub(super) function_mode: FlowContext,
    /// `inst_next` of every instruction this build decoded, kept or not.
    /// `RegionBuilder` re-derives the context there, so the engine's value at
    /// such an address comes from this build's own bytes, never a stale one.
    pub(super) committed_next: rustc_hash::FxHashSet<u64>,
    /// The seeded arm the region under construction descends from.
    pub(super) descent: Option<ArmSeed>,
    pub(super) report: crate::BuildReport,
    /// The address each [`Self::seat_non_boundary_target`] edge points INTO;
    /// only the split half keeping those bytes may keep the edge.
    pub(super) non_boundary_seats: rustc_hash::FxHashMap<EdgeIndex, PcodeInsnAddr>,
    /// `(Switch region, arm)` for each arm [`Self::drop_refused_switch_arms`]
    /// strips: undecodable, or owned in the other ISA mode. Keyed on the site:
    /// the same address from another site decodes in another context.
    pub(super) refused_arms: Vec<(NodeIndex, PcodeInsnAddr)>,
    /// Empty regions of zero-pcode-op instructions sealed onto a stub at bytes
    /// that are not code, each with the decode error [`Self::finish`] returns
    /// when no call falls into it; `finish` ends every call that does.
    pub(super) nops_into_no_code: Vec<(NodeIndex, Option<anyhow::Error>)>,
}

impl<'a, R: rsleigh::MemReader> Builder<'a, R> {
    /// `engine` is already pinned to `function_mode`.
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
            refused_arms: Vec::new(),
            nops_into_no_code: Vec::new(),
            report: crate::BuildReport::default(),
            non_boundary_seats: rustc_hash::FxHashMap::default(),
            committed_next: rustc_hash::FxHashSet::default(),
            function_mode,
            descent: None,
        }
    }

    /// Reports `addr` as [`Reason::IsaModeConflict`] when `want` differs from
    /// the mode `existing` decoded in, and returns whether it did. The first
    /// decode keeps the bytes, so the losing edge's mode would otherwise be
    /// dropped silently. Every edge-wiring seal must call this, as
    /// [`Self::explore`] does.
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
            self.report.incomplete.insert(addr, Reason::IsaModeConflict);
        }
        clash
    }

    /// Enqueues `addr` as a direct successor of `parent` (branch or
    /// fall-through), inheriting [`Self::descent`].
    ///
    /// `isa_mode` is the parent region's: a direct edge switches no mode.
    /// `flowing` is the parent's live value of each flowing committed var
    /// (`RegionBuilder::flowing_commits`), empty for the entry.
    pub(super) fn enqueue(
        &mut self,
        parent: Option<NodeIndex>,
        addr: PcodeInsnAddr,
        isa_mode: Option<u32>,
        flowing: &[(&'static str, u32)],
    ) {
        // The engine is read only where `committed_next` vouches for it;
        // elsewhere it may hold a previous build's commit. An arch that
        // re-derives nothing takes the function baseline.
        let bit = isa_mode.map(|mode| mode != 0);
        let carried = if !self.engine.tables.flow_commit.is_empty()
            && self.committed_next.contains(&addr.machine())
        {
            let snapshot = self
                .engine
                .flow_vars()
                .snapshot(&self.engine.sleigh, addr.machine());
            self.carry(snapshot, &[], bit)
        } else {
            // A flowing committed var keeps its live value (ARM `TEEMode`); a
            // `noflow` one degrades to the default, since carrying a live IT
            // state forward would predicate the code after it.
            self.carry(self.function_mode.clone(), flowing, bit)
        };
        self.work_queue.push(WorkItem {
            parent,
            addr,
            carried,
            seeded_by: self.descent,
        });
    }

    /// Enqueues `addr` as a resolved arm of the `Switch` region `parent`, in
    /// the function mode with the ISA-mode var set to `bit` (the branch's own
    /// interworking bit, else [`Self::sample_flowing_isa_bit`]) and `flowing`
    /// applied as in [`Self::enqueue`].
    pub(super) fn enqueue_resolved(
        &mut self,
        parent: NodeIndex,
        addr: PcodeInsnAddr,
        bit: Option<bool>,
        flowing: &[(&'static str, u32)],
    ) {
        let carried = self.carry(self.function_mode.clone(), flowing, bit);
        self.seeded_queue.push(WorkItem {
            parent: Some(parent),
            addr,
            carried,
            seeded_by: Some(ArmSeed {
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
        let vars = self.engine.flow_vars();
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
    /// The first decode owns the bytes, so this hands bytes reached in two ISA
    /// modes to the direct edge's proved mode over an arm's claimed one. Code
    /// decoded off an arm is no more certain than the arm: there a clash is
    /// only reported.
    fn next_work_item(&mut self) -> Option<WorkItem> {
        self.work_queue.pop().or_else(|| self.seeded_queue.pop())
    }

    /// An empty region must be `Unconditional` (a zero-pcode-op instruction)
    /// or `TailCall` (a [`Self::edge_to_stub`] stub).
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

        // `build` segments at every zero-pcode-op instruction.
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

    /// The ISA mode `ctx` carries, `None` on an arch with no mode var.
    fn isa_mode_of(&self, ctx: &FlowContext) -> Option<u32> {
        self.engine
            .arch
            .isa_mode_var()
            .and_then(|var| self.engine.flow_vars().value_of(ctx, var))
    }

    /// Records and returns the ISA-mode bit flowing into the indirect branch
    /// at `site`, `None` on an arch with no ISA-mode var. Sampled at the seal,
    /// while the owning region's mode is held there: a later read sees
    /// whichever region painted the address last.
    pub(super) fn sample_flowing_isa_bit(&mut self, site: PcodeInsnAddr) -> Option<bool> {
        let var = self.engine.arch.isa_mode_var()?;
        if let Some(&bit) = self.report.flowing_isa_bits.get(&site) {
            return Some(bit);
        }
        // Not `false`: that would decode a Thumb function's targets as ARM.
        let entry_bit = self
            .isa_mode_of(&self.function_mode)
            .is_some_and(|v| v != 0);
        let bit = self
            .engine
            .sleigh
            .get_context_at(site.machine(), var)
            .map_or(entry_bit, |mode| mode != 0);
        self.report.flowing_isa_bits.insert(site, bit);
        Some(bit)
    }

    /// Edges `from` to the empty `TailCall` stub at `addr`, created on first
    /// use: the successor of an edge leaving the decoded bytes (out of bound,
    /// unmapped, or not code). A stub is never enqueued, so never decoded.
    pub(super) fn edge_to_stub(&mut self, from: NodeIndex, addr: PcodeInsnAddr) -> Result<()> {
        let stub = match self.index.get(addr) {
            Some(existing) => existing,
            None => self.add_region(Region {
                start_addr: addr,
                insns: Vec::new(),
                empty_span_len: 0,
                isa_mode: None,
                terminator: RegionTerminator::TailCall {
                    target: addr.machine().into(),
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

    /// Whether `addr` lies inside a decoded region but off every instruction
    /// start, where no split can express it.
    pub(super) fn addr_is_interior_non_boundary(&self, addr: PcodeInsnAddr) -> bool {
        self.find_region_containing_addr(addr)
            .is_some_and(|(_, region)| region.start_addr != addr && !region.contains_insn_at(addr))
    }

    /// Seats an edge for a target interior to `owner` but off every instruction
    /// boundary of it, which no split can express.
    ///
    /// A `Switch` drops the arm (an over-approximated entry). Any other
    /// terminator edges to the owner, which `Cfg::region_if` matches by
    /// containment; its stream starts earlier than the target, so the edge is
    /// reported [`Reason::InteriorTarget`] unless it is a benign prefix
    /// re-entry.
    fn seat_non_boundary_target(
        &mut self,
        parent: NodeIndex,
        owner: NodeIndex,
        addr: PcodeInsnAddr,
    ) -> Result<()> {
        let region = &mut self.region_graph[parent];
        if !matches!(region.terminator, RegionTerminator::Switch { .. }) {
            let benign = self.is_benign_prefix_reentry(owner, addr);
            let edge = self.region_graph.add_edge(parent, owner, ());
            self.non_boundary_seats.insert(edge, addr);
            if !benign {
                self.report.incomplete.insert(addr, Reason::InteriorTarget);
            }
            return Ok(());
        }
        // One arm per seating: an address named in both ISA modes has one
        // work item per arm.
        let dropped = region.terminator.drop_switch_arms_at(addr.machine(), true);
        debug_assert!(
            dropped,
            "seated interior target {addr:?} names no arm of the Switch that routed it",
        );
        self.report.incomplete.insert(addr, Reason::InteriorTarget);
        Ok(())
    }

    /// Whether `addr` lands one instruction-prefix into `owner`'s stream and
    /// re-enters the same instruction: decoding from `addr` ends at the byte
    /// the containing instruction does and carries the same p-code once the
    /// atomic-prefix markers (`LOCK` / `UNLOCK`) are set aside.
    ///
    /// The containing instruction must be `owner`'s first (glibc's
    /// `je 0f; lock; 0:`): an owner starting earlier runs instructions the
    /// taken path does not.
    ///
    /// Skipped on an arch with an ISA-mode var or committed flow vars, so the
    /// throwaway decode leaks no mode and no context [`Self::enqueue`] reads.
    fn is_benign_prefix_reentry(&mut self, owner: NodeIndex, addr: PcodeInsnAddr) -> bool {
        if self.engine.arch.isa_mode_var().is_some() || !self.engine.tables.flow_commit.is_empty() {
            return false;
        }
        let region = &self.region_graph[owner];
        let target = addr.machine();
        let Some(insn) = region
            .insns
            .iter()
            .rev()
            .find(|i| i.addr.machine() <= target)
        else {
            return false;
        };
        let start = insn.addr.machine();
        let full_end = start.saturating_add(u64::from(insn.len));
        if !(start < target && target < full_end) {
            return false;
        }
        if region.start_addr != PcodeInsnAddr::at_machine_start(start) {
            return false;
        }
        let full = region
            .insns
            .iter()
            .filter(|i| i.addr.machine() == start)
            .map(|i| &i.insn);
        let Ok(tail) = self.engine.sleigh.lift_one(target) else {
            return false;
        };
        // Not saturating: a tail ending at the top must not match `full_end`.
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

    /// Routes `addr` to the region owning it, splitting at an interior
    /// boundary, or pins `carried` and decodes a new region there.
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
                // The arm names a stream these bytes were not decoded as.
                self.refused_arms.push((parent_region_id, addr));
                return Ok(());
            }
            let target = if is_start {
                region_id
            } else if is_boundary {
                self.split_region(region_id, addr)?
            } else {
                return self.seat_non_boundary_target(parent_region_id, region_id, addr);
            };
            self.region_graph.add_edge(parent_region_id, target, ());
            return Ok(());
        }
        // An ISA-mode alias the diff writes last can flip the mode;
        // `RegionBuilder::hold_isa_mode` repairs that on its first lift.
        self.engine
            .flow_vars()
            .pin_at(&mut self.engine.sleigh, addr.machine(), &carried)?;
        self.descent = seeded_by;
        let built = RegionBuilder::new(self, addr, parent_region, isa_mode).build();
        self.descent = None;
        built
    }

    /// Refuses every `Switch` arm reaching one of `regions` (each decoded off
    /// an arm and missing an undecodable successor) without passing through
    /// another `Switch`: an arm reaching the bytes only back through a dispatch
    /// says nothing about its own bound.
    fn refuse_arms_reaching(&mut self, regions: impl Iterator<Item = NodeIndex>) {
        let mut stack: Vec<NodeIndex> = regions.collect();
        // A dispatch is discovered, bounding the walk, but does not reach.
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
        let refused: Vec<_> = self
            .region_graph
            .node_indices()
            .filter(|&switch| {
                matches!(
                    self.region_graph[switch].terminator,
                    RegionTerminator::Switch { .. }
                )
            })
            .flat_map(|switch| {
                self.region_graph
                    .neighbors(switch)
                    .filter(|arm| reaching.contains(arm))
                    .map(move |arm| (switch, arm))
            })
            .collect();
        for (switch, arm) in refused {
            self.refuse_undecodable(switch, self.region_graph[arm].start_addr);
        }
    }

    /// Refuses `arm` of the dispatch region `switch` as undecodable, reported
    /// against the site its terminator names.
    fn refuse_undecodable(&mut self, switch: NodeIndex, arm: PcodeInsnAddr) {
        let (RegionTerminator::Switch { addr: site, .. }
        | RegionTerminator::UnresolvedIndirectBranch { addr: site, .. }) =
            self.region_graph[switch].terminator
        else {
            unreachable!("an arm's seating region {switch:?} is no dispatch");
        };
        self.refused_arms.push((switch, arm));
        self.report
            .undecodable_seeded
            .push(crate::UndecodableTarget { site, target: arm });
    }

    /// Strips each [`Self::refused_arms`] arm from its own site only: the
    /// lifter rejects an arm with no successor starting at it.
    fn drop_refused_switch_arms(&mut self) {
        for (parent, target) in std::mem::take(&mut self.refused_arms) {
            self.drop_switch_arm(parent, target);
        }
    }

    /// Strips every arm at `target`'s machine address, in either ISA mode,
    /// from the `Switch` at `site`, with its edges. Stripping the last arm
    /// re-defers the site as `UnresolvedIndirectBranch`.
    fn drop_switch_arm(&mut self, site: NodeIndex, target: PcodeInsnAddr) {
        if !self.region_graph[site]
            .terminator
            .drop_switch_arms_at(target.machine(), false)
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

    /// The regions in `from` or falling into them through empty regions of
    /// zero-pcode-op instructions whose last instruction is a call.
    pub(super) fn calls_falling_into(
        &self,
        from: impl IntoIterator<Item = NodeIndex>,
    ) -> Vec<NodeIndex> {
        let mut stack: Vec<NodeIndex> = from.into_iter().collect();
        let mut seen: rustc_hash::FxHashSet<NodeIndex> = stack.iter().copied().collect();
        let mut calls = Vec::new();
        while let Some(id) = stack.pop() {
            let region = &self.region_graph[id];
            if region.terminator != RegionTerminator::Unconditional {
                continue;
            }
            match region.insns.last() {
                Some(last) if is_call(last) => calls.push(id),
                Some(_) => {}
                None => stack.extend(
                    self.region_graph
                        .neighbors_directed(id, petgraph::Incoming)
                        .filter(|&p| seen.insert(p)),
                ),
            }
        }
        calls
    }

    /// Re-seals `region`, which ends in a call whose fall-through holds no
    /// code, as `NoReturn`.
    fn end_as_no_return(&mut self, region: NodeIndex) {
        let outgoing: Vec<_> = self
            .region_graph
            .edges_directed(region, petgraph::Outgoing)
            .map(|e| e.id())
            .collect();
        for edge in outgoing {
            self.region_graph.remove_edge(edge);
            self.non_boundary_seats.remove(&edge);
        }
        self.region_graph[region].terminator = RegionTerminator::NoReturn;
    }

    /// Removes every region no path from `entry` reaches.
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

    /// Triages a work item `explore` could not decode; `Err` is a failure no
    /// report covers.
    fn record_failure(
        &mut self,
        parent_region: Option<NodeIndex>,
        address: PcodeInsnAddr,
        seeded_by: Option<ArmSeed>,
        e: anyhow::Error,
        lost_successor: &mut Vec<(NodeIndex, anyhow::Error)>,
    ) -> Result<()> {
        let is_arm = seeded_by.is_some_and(|s| s.is_arm(parent_region, address));
        // A direct target that is unmapped or not code leaves through a stub
        // and is reported. The entry has no parent to hang one off.
        if !is_arm && let Some(parent) = parent_region {
            let here = decode_failure(&e).filter(|f| f.addr() == address.machine());
            let reason = match here {
                Some(DecodeFailure::Unmapped(_)) => Some(Reason::UnmappedTarget),
                Some(DecodeFailure::NotCode { .. }) if seeded_by.is_none() => {
                    Some(Reason::UndecodableTarget)
                }
                _ => None,
            };
            if let Some(reason) = reason {
                self.report.incomplete.insert(address, reason);
                self.edge_to_stub(parent, address)?;
                return Ok(());
            }
        }
        // Under a seeded arm: drop the arm against its site.
        let Some(seed) = seeded_by else {
            return Err(e);
        };
        // The bad bytes, read off the error: they may sit several
        // instructions past `address`.
        let (at, reason) = match decode_failure(&e) {
            // `address` carries the insn index the gap address cannot.
            Some(DecodeFailure::Unmapped(gap)) if gap == address.machine() => {
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
        self.report.incomplete.insert(at, reason);
        self.refuse_undecodable(seed.switch, seed.arm);
        if let Some(parent) = parent_region.filter(|_| !is_arm) {
            lost_successor.push((parent, e));
        }
        Ok(())
    }

    /// Ends calls falling into no code, drops refused arms and unreachable
    /// regions, and derives the site reports.
    fn finish(mut self, lost_successor: Vec<(NodeIndex, anyhow::Error)>) -> Result<Cfg> {
        let mut no_return = Vec::new();
        for (nop, e) in std::mem::take(&mut self.nops_into_no_code) {
            let preds: Vec<_> = self
                .region_graph
                .neighbors_directed(nop, petgraph::Incoming)
                .collect();
            let calls = self.calls_falling_into(preds);
            if let Some(e) = e.filter(|_| calls.is_empty()) {
                return Err(e);
            }
            no_return.extend(calls);
        }
        for call in no_return {
            self.end_as_no_return(call);
        }
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
        let graph = &self.region_graph;
        let index = &self.index;
        let live = |site: &PcodeInsnAddr| index.containing(graph, *site).is_some();
        let report = &mut self.report;
        report.link_register_seated.retain(live);
        report.tail_call_seated.retain(live);
        report.flowing_isa_bits.retain(|site, _| live(site));
        // No resolver ran: every deferred site is unresolved, every seat
        // unverified.
        for region in graph.node_weights() {
            match &region.terminator {
                RegionTerminator::UnresolvedIndirectBranch { addr, .. } => {
                    report.incomplete.insert(*addr, Reason::UnresolvedBranch);
                }
                RegionTerminator::Switch { addr, .. } => {
                    report.incomplete.insert(*addr, Reason::UnverifiedSeed);
                }
                _ => {}
            }
        }
        for &addr in report
            .link_register_seated
            .iter()
            .chain(&report.tail_call_seated)
        {
            report.incomplete.insert(addr, Reason::UnverifiedSeed);
        }
        let mut reported = rustc_hash::FxHashSet::default();
        report
            .undecodable_seeded
            .retain(|&bad| reported.insert(bad));

        Ok(Cfg {
            region_graph: self.region_graph,
            entry: starting_region,
            index: self.index,
            report: self.report,
            space_ids: self.engine.space_ids().clone(),
        })
    }
}
