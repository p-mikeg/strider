//! Per-region lift: one IR region per CFG region, each region's instructions
//! in dominator-tree pre-order, then the edges no terminator wired.

use anyhow::Result;

use crate::{FunctionLifter, pruned_ssa};

pub(crate) type RegionMap = rustc_hash::FxHashMap<strider_cfg::RegionId, strider_ir::RegionId>;

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    /// Allocates one IR region per CFG region, keyed by CFG `RegionId`; every
    /// CFG region is present.
    pub(crate) fn build_region_map(
        &mut self,
        placement: &pruned_ssa::PhiPlacement,
    ) -> Result<RegionMap> {
        let cfg = self.cfg;
        let mut region_map: RegionMap = RegionMap::default();
        for cfg_rid in cfg.region_ids() {
            // Sorted, so `Phi` creation order (hence node-id assignment)
            // follows `InitialVnId` rather than the placement set's hash
            // layout.
            let mut placed: Vec<strider_ir::node::InitialVnId> = placement
                .get(&cfg_rid)
                .map(|s| s.iter().copied().collect())
                .unwrap_or_default();
            placed.sort_unstable();
            let ir_rid = self.builder.create_region(&placed)?;
            region_map.insert(cfg_rid, ir_rid);
            let region = &cfg.region_graph()[cfg_rid];
            let last_addr = region
                .insns
                .last()
                .map_or(region.start_addr.machine_addr.addr, |wrapped| {
                    wrapped.addr.machine_addr.addr
                });
            let ctrl = self.builder.region_cur_ctrl(ir_rid);
            let node = self.builder.function().graph().value_definition(ctrl).0;
            self.region_last_addrs.insert(node, last_addr);
        }
        let entry_ir = region_map[&cfg.entry()];
        self.builder.set_entry_region(entry_ir)?;
        // `bind_entry_address_register` writes a variable, which needs a
        // current region.
        self.builder.set_region(entry_ir);
        self.record_register_arg_carriers()?;
        self.bind_entry_address_register()?;
        Ok(region_map)
    }
    /// Translates every region's instructions and, when present, its special
    /// terminator.  A special terminator's pcode insn is skipped inside the
    /// per-insn loop and lifted post-loop by a dedicated handler, fingerprinted
    /// to the region's last machine address.
    pub(crate) fn translate_regions(
        &mut self,
        region_map: &RegionMap,
        doms: &graph_algorithms::dominance::Dominators<strider_cfg::RegionId>,
    ) -> Result<()> {
        let cfg = self.cfg;
        // Pre-order matters: a region must be processed after its immediate
        // dominator so `inherit_variables` reads that dominator's FINAL values.
        // Siblings go ascending by `RegionId`, and every body node's NodeId
        // follows this order. Regions unreachable from the entry are absent.
        for cfg_rid in doms.tree_preorder() {
            let ir_region = region_map[&cfg_rid];
            // The entry region has no idom; `set_entry_region` seeded it.
            if let Some(idom_cfg) = doms.immediate_dominator(cfg_rid) {
                self.builder
                    .inherit_variables(ir_region, region_map[&idom_cfg]);
            }
            self.builder.set_region(ir_region);
            #[cfg(debug_assertions)]
            {
                self.current_cfg_region = Some(cfg_rid);
            }
            let region = &cfg.region_graph()[cfg_rid];
            let terminator = &region.terminator;
            self.pcode_consts.reset();
            for wrapped_insn in &region.insns {
                // Observed BEFORE the skip, and reset per region above, because
                // `collect_def_sites` feeds every op of every region through an
                // identical resolver. The two must see the same sequence or a
                // register write can land where no phi was placed for it.
                self.pcode_consts
                    .observe(wrapped_insn.addr, &wrapped_insn.insn);
                if skips_opcode(terminator, wrapped_insn.insn.opcode) {
                    continue;
                }
                self.process_insn(cfg_rid, &wrapped_insn.insn, wrapped_insn.addr, region_map)?;
            }
            // Fingerprint contributor for the terminator handlers: the region's
            // last pcode insn. A region with none (a synthetic tail-call stub,
            // or a run of zero-op instructions) falls back to its predecessor's
            // branch or fall-through, or its nodes carry no fingerprint and fail
            // the validator. A predecessor with no pcode insn contributes its
            // region start. `max` picks one when several branches share a stub.
            let term_addr = region
                .insns
                .last()
                .map(|wrapped| wrapped.addr.machine_addr.addr)
                .or_else(|| {
                    cfg.region_predecessors(cfg_rid)
                        .map(|pred| {
                            let pred = &cfg.region_graph()[pred];
                            pred.insns
                                .last()
                                .map_or(pred.start_addr, |w| w.addr)
                                .machine_addr
                                .addr
                        })
                        .max()
                });
            // A `NoReturn` region ending in a `Call` or `CallIndirect` has an
            // open control edge (the per-insn loop lifted the call, which does
            // not terminate), so sink it into `Unreachable`.  Gate on the
            // opcode: a `CallOther` NoReturn region already self-terminated
            // inside `handle_call_other`, and terminating it twice fails.
            let noreturn_call =
                matches!(region.terminator, strider_cfg::RegionTerminator::NoReturn)
                    && region.insns.last().is_some_and(|w| {
                        matches!(
                            w.insn.opcode,
                            rsleigh::Opcode::Call | rsleigh::Opcode::CallIndirect
                        )
                    });
            self.with_lift_addr(term_addr, |s| match terminator {
                strider_cfg::RegionTerminator::UnresolvedIndirectBranch { target_vn, addr } => {
                    s.handle_unresolved_indirect_branch(target_vn, *addr)
                }
                strider_cfg::RegionTerminator::Switch {
                    target_vn,
                    targets,
                    addr,
                } => s.handle_switch(cfg_rid, target_vn, targets, region_map, *addr),
                // A target's `isa_bit` (the callee's ISA mode on an
                // interworking tail call) is the callee function's own
                // concern, applied when it is analyzed at its entry, not
                // while lifting this caller.
                strider_cfg::RegionTerminator::TailCall { target } => {
                    s.handle_tail_call(target.addr)
                }
                _ if noreturn_call => s.builder.build_unreachable(),
                _ => Ok(()),
            })?;
        }
        Ok(())
    }

    /// Wires the region successors no terminator handler wired: only
    /// `Unconditional` regions, whose `Branch` lifts to nothing.  `CondBranch`
    /// and `Switch` are already wired by their handlers.
    pub(crate) fn link_region_edges(&mut self, region_map: &RegionMap) -> Result<()> {
        let graph = self.cfg.region_graph();
        for (src, tgt) in graph.edge_indices().filter_map(|e| graph.edge_endpoints(e)) {
            if matches!(
                graph[src].terminator,
                strider_cfg::RegionTerminator::Unconditional
            ) {
                // Every CFG edge against a `translate_regions` that walks the
                // dominator pre-order, which covers every region only because
                // `strider_cfg::DecodeEngine::build_cfg` removes the unreachable ones.
                self.builder
                    .link_regions(region_map[&src], region_map[&tgt])?;
            }
        }
        Ok(())
    }
}

/// Whether the per-instruction loop must skip `opcode`, leaving the region's
/// terminator to the post-loop dispatch below.
///
/// `TailCall` skips `BranchIndirect` as well as `Branch`: a `known_targets`
/// resolution pointing out of the function marks the `jmp reg` a tail call,
/// and processing the `BranchIndirect` would emit an `IndirectBranch` and
/// terminate the region, failing `handle_tail_call`'s `build_call`.  A
/// `CondBranch` never lives in a TailCall region: it keeps its own
/// terminator, and the stub regions on its out-of-bounds arms have no insns.
///
/// Skipping by opcode alone is safe by region closure: `Branch` and
/// `BranchIndirect`, the only opcodes skipped, close the region in
/// `RegionBuilder::process_new_insn`, so at most one appears per region and
/// it is always the trailing entry, never an inner pcode op.  (`Call` /
/// `CallIndirect` / `CallOther` can close it too, but are never skipped.)
fn skips_opcode(terminator: &strider_cfg::RegionTerminator, opcode: rsleigh::Opcode) -> bool {
    match terminator {
        strider_cfg::RegionTerminator::UnresolvedIndirectBranch { .. }
        | strider_cfg::RegionTerminator::Switch { .. } => opcode == rsleigh::Opcode::BranchIndirect,
        strider_cfg::RegionTerminator::TailCall { .. } => matches!(
            opcode,
            rsleigh::Opcode::Branch | rsleigh::Opcode::BranchIndirect
        ),
        _ => false,
    }
}
