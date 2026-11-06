use anyhow::Result;

use crate::{FunctionLifter, pruned_ssa};

pub(crate) type RegionMap = rustc_hash::FxHashMap<strider_cfg::RegionId, strider_ir::RegionId>;

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    /// One IR region per CFG region.
    pub(crate) fn build_region_map(
        &mut self,
        placement: &pruned_ssa::PhiPlacement,
    ) -> Result<RegionMap> {
        let cfg = self.cfg;
        let mut region_map: RegionMap = RegionMap::default();
        for cfg_rid in cfg.region_ids() {
            // Sorted, so node ids do not depend on hash order.
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
                .back()
                .map_or(region.start_addr.machine_addr.addr, |wrapped| {
                    wrapped.addr.machine_addr.addr
                });
            let ctrl = self.builder.region_cur_ctrl(ir_rid);
            let node = self.builder.function().graph().value_definition(ctrl).0;
            self.region_last_addrs.insert(node, last_addr);
        }
        let entry_ir = region_map[&cfg.entry()];
        self.builder.set_entry_region(entry_ir)?;
        // Writing a variable needs a current region.
        self.builder.set_region(entry_ir);
        self.record_register_arg_carriers()?;
        self.bind_entry_registers()?;
        Ok(region_map)
    }

    /// Lifts every region's instructions, then its special terminator (whose
    /// own pcode insn the loop skips).
    pub(crate) fn translate_regions(
        &mut self,
        region_map: &RegionMap,
        doms: &graph_algorithms::dominance::Dominators<strider_cfg::RegionId>,
    ) -> Result<()> {
        let cfg = self.cfg;
        // Pre-order, so `inherit_variables` reads the idom's final values.
        for cfg_rid in doms.tree_preorder() {
            let ir_region = region_map[&cfg_rid];
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
                // Observed before the skip: `collect_def_sites` must see the
                // same op sequence, or a write lands where no phi was placed.
                self.pcode_consts
                    .observe(wrapped_insn.addr, &wrapped_insn.insn);
                if skips_opcode(terminator, wrapped_insn.insn.opcode) {
                    continue;
                }
                self.process_insn(cfg_rid, &wrapped_insn.insn, wrapped_insn.addr, region_map)?;
            }
            // The terminator's fingerprint address: the region's last pcode
            // insn, else (a stub) the greatest predecessor's last insn or start.
            let term_addr = region
                .insns
                .back()
                .map(|wrapped| wrapped.addr.machine_addr.addr)
                .or_else(|| {
                    cfg.region_predecessors(cfg_rid)
                        .map(|pred| {
                            let pred = &cfg.region_graph()[pred];
                            pred.insns
                                .back()
                                .map_or(pred.start_addr, |w| w.addr)
                                .machine_addr
                                .addr
                        })
                        .max()
                });
            // A `NoReturn` region ending in a call is still open; one ending
            // in a `CallOther` already terminated in `handle_call_other`.
            let noreturn_call =
                matches!(region.terminator, strider_cfg::RegionTerminator::NoReturn)
                    && region.insns.back().is_some_and(|w| {
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
                // The target's `isa_bit` is the callee's concern.
                strider_cfg::RegionTerminator::TailCall { target } => {
                    s.handle_tail_call(target.addr)
                }
                _ if noreturn_call => s.builder.build_unreachable(),
                _ => Ok(()),
            })?;
        }
        Ok(())
    }

    /// Wires the successors of `Unconditional` regions, whose `Branch` lifts
    /// to nothing; other terminators' handlers wire their own.
    pub(crate) fn link_region_edges(&mut self, region_map: &RegionMap) -> Result<()> {
        let graph = self.cfg.region_graph();
        for (src, tgt) in graph.edge_indices().filter_map(|e| graph.edge_endpoints(e)) {
            if matches!(
                graph[src].terminator,
                strider_cfg::RegionTerminator::Unconditional
            ) {
                // `build_cfg` drops unreachable regions, so every endpoint was
                // lifted.
                self.builder
                    .link_regions(region_map[&src], region_map[&tgt])?;
            }
        }
        Ok(())
    }
}

/// Whether the per-instruction loop skips `opcode`, leaving it to the
/// terminator handler. Safe by opcode: a skipped one closes its region.
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
