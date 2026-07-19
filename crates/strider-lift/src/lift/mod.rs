use anyhow::{Result, anyhow};

mod arithmetic;
mod boolean;
mod call;
mod cast;
mod cc_projection;
mod control;
mod dispatch;
mod dominance;
mod float;
mod function_lifter;
mod integer;
mod memory;
mod misc;
pub(crate) mod pcode_util;
mod pruned_ssa;
mod vn_io;

#[cfg(test)]
mod handler_tests;

#[cfg(test)]
mod aliasing_tests;

#[cfg(test)]
mod cc_projection_tests;

pub(crate) use function_lifter::FunctionLifter;

pub struct LiftOutcome {
    pub function: strider_ir::Function,
    /// One entry per region that terminated in an unresolved indirect branch,
    /// mapping the `BranchIndirect`'s pcode address to the `IndirectBranch`
    /// placeholder anchoring its dispatch varnode.
    pub unresolved_branches: Vec<(strider_cfg::PcodeInsnAddr, strider_ir::node::NodeId)>,
}

pub use crate::lift_options::LiftOptions;

/// The CFG-to-IR lift engine, built once and reused across every function and
/// rebuild iteration.
///
/// The calling convention is deliberately not stored: it is per-function, so
/// it is a per-call argument.
///
/// Not `Clone`, since the owned `Sleigh` is not cheaply cloneable.
pub struct Lifter<R: rsleigh::MemReader> {
    arch: strider_target::SleighArch,
    /// Borrowed `&mut` to build the CFG, then `&` to lift it.
    sleigh: rsleigh::Sleigh<R>,
    /// Cached at construction: `Sleigh::regs()` is expensive.
    sleigh_regs: rsleigh::SleighRegs,
    user_op_names: Vec<String>,
}

impl<R: rsleigh::MemReader> Lifter<R> {
    pub fn new(arch: strider_target::SleighArch, sleigh: rsleigh::Sleigh<R>) -> Result<Self> {
        let sleigh_regs = sleigh.regs()?;
        let user_op_names = sleigh.user_op_names().unwrap_or_default();
        Ok(Self {
            arch,
            sleigh,
            sleigh_regs,
            user_op_names,
        })
    }

    /// Indexed by `user_op_id`.
    #[must_use]
    pub fn user_op_names(&self) -> &[String] {
        &self.user_op_names
    }

    #[must_use]
    pub fn sleigh(&self) -> &rsleigh::Sleigh<R> {
        &self.sleigh
    }

    #[must_use]
    pub fn sleigh_regs(&self) -> &rsleigh::SleighRegs {
        &self.sleigh_regs
    }

    /// `per_address_ccs` supplies CC overrides for call TARGETS.  Pass an
    /// empty map for no overrides.
    pub fn build_cfg(
        &mut self,
        entry: strider_cfg::MachineInsnAddr,
        cfg_opts: &strider_cfg::CfgOptions,
        per_address_ccs: &rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>,
    ) -> Result<strider_cfg::Cfg> {
        // `Sleigh::lift_one` carries context-register state across calls, so on
        // a reused `Lifter` a prior function's `globalset` (an ARM `bx`/`blx`
        // switching Thumb `TMode`) leaks in and mis-decodes this one.  Each
        // function is an independent entry point and must start from the
        // processor-spec defaults.  Cheap, and a no-op on arches that never
        // commit context.
        self.sleigh.reset_context()?;
        strider_cfg::Builder::for_arch(&self.arch, &mut self.sleigh, entry.addr, cfg_opts)
            .with_per_address_ccs(per_address_ccs.clone())
            .build()
    }

    /// Returns the unique set only; ordering is applied by
    /// `FunctionBuilder::new`.
    pub(crate) fn find_all_unique_vns(&self, cfg: &strider_cfg::Cfg) -> Vec<rsleigh::Vn> {
        cfg.regions()
            .flat_map(|region| region.insns.iter())
            .flat_map(|wrapped| wrapped.insn.all_vns())
            .collect::<rustc_hash::FxHashSet<rsleigh::Vn>>()
            .into_iter()
            .collect()
    }

    /// [`Self::build_ir_with`] under default [`LiftOptions`].
    pub fn build_ir(
        &self,
        cfg: &strider_cfg::Cfg,
        cc: strider_target::BuiltCallingConvention,
    ) -> Result<LiftOutcome> {
        self.build_ir_with(cfg, cc, &LiftOptions::default())
    }

    /// `cc` is the function default; a direct Call whose target appears in
    /// [`LiftOptions::per_address_ccs`] is built with that override instead.
    pub fn build_ir_with(
        &self,
        cfg: &strider_cfg::Cfg,
        cc: strider_target::BuiltCallingConvention,
        opts: &LiftOptions,
    ) -> Result<LiftOutcome> {
        // The CFG is rebuilt from scratch each lift, so the tracked set is
        // always scanned fresh.  `FunctionLifter::new` adds the stack vn; the
        // lifter is the SSoT for that.
        let all_vns = self.find_all_unique_vns(cfg);
        let mut driver = FunctionLifter::new(self, cc, cfg, all_vns, &opts.per_address_ccs)?;

        // Cytron pruned-SSA phi placement: iterated dominance frontier of each
        // variable's definition sites.  This is what stops the lifter minting a
        // value `Phi` for every varnode at every region (millions of dead phis).
        let dom = dominance::DomInfo::compute(cfg);
        let def_sites = driver.collect_def_sites();
        let placement = dom.iterated_frontier(&def_sites);

        let region_map = driver.build_region_map(&placement)?;

        // Dominator-tree pre-order, so each region inherits reaching variable
        // values from its already-processed immediate dominator.  Then wire the
        // fallthrough edges the per-insn loop didn't reach.
        driver.translate_regions(&region_map, &dom)?;
        driver.link_region_edges(&region_map)?;

        let unresolved_branches = std::mem::take(&mut driver.unresolved_branches);
        let function = driver.builder.build()?;
        Ok(LiftOutcome {
            function,
            unresolved_branches,
        })
    }
}

pub(crate) type RegionMap = rustc_hash::FxHashMap<strider_cfg::RegionId, strider_ir::RegionId>;

pub(crate) fn ir_region_of(
    region_map: &RegionMap,
    cfg_rid: strider_cfg::RegionId,
) -> Result<strider_ir::RegionId> {
    region_map
        .get(&cfg_rid)
        .copied()
        .ok_or_else(|| anyhow!("no region {cfg_rid:?} in cfg"))
}

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
    /// Allocates one IR region per CFG region, keyed by CFG `RegionId`; every
    /// CFG region is present, hence no `Option` value.
    fn build_region_map(&mut self, placement: &pruned_ssa::PhiPlacement) -> Result<RegionMap> {
        self.builder.build_entry()?;
        let cfg = self.cfg;
        let mut region_map: RegionMap = RegionMap::default();
        for cfg_rid in cfg.region_ids() {
            let placed: Vec<strider_ir::node::InitialVnId> = placement
                .get(&cfg_rid)
                .map(|s| s.iter().copied().collect())
                .unwrap_or_default();
            region_map.insert(cfg_rid, self.builder.create_region(&placed)?);
        }
        let entry_ir = *region_map
            .get(&cfg.entry())
            .ok_or_else(|| anyhow!("entry region {:?} missing from region_map", cfg.entry()))?;
        self.builder.set_entry_region(entry_ir)?;
        self.record_register_arg_carriers();
        Ok(region_map)
    }

    /// Each arg-passing register's largest-container `InitialVar` output is the
    /// carrier for its positional index: a narrow ABI alias (`edi`) routes
    /// through its tracked container (`rdi`).
    fn record_register_arg_carriers(&mut self) {
        let arg_regs = self
            .builder
            .function()
            .default_cc()
            .arg_passing_regs
            .clone();
        for (i, reg) in arg_regs.iter().enumerate() {
            let container = self.container_of(reg);
            if let Some(value) = self.builder.function().initial_var_value(&container) {
                self.builder
                    .function_mut()
                    .side_tables_mut()
                    .register_arg_value(i as u32, value);
            }
        }
    }

    /// Translates every region's instructions and, when present, its special
    /// terminator.  A special terminator's pcode insn is skipped inside the
    /// per-insn loop and lifted post-loop by a dedicated handler, fingerprinted
    /// to the region's last machine address.
    fn translate_regions(
        &mut self,
        region_map: &RegionMap,
        dom: &dominance::DomInfo,
    ) -> Result<()> {
        let cfg = self.cfg;
        // Pre-order matters: a region must be processed after its immediate
        // dominator so `inherit_variables` reads that dominator's FINAL values.
        for &cfg_rid in dom.preorder() {
            let ir_region = ir_region_of(region_map, cfg_rid)?;
            // The entry region has no idom; `set_entry_region` seeded it.
            if let Some(idom_cfg) = dom.immediate_dominator(cfg_rid) {
                let idom_ir = ir_region_of(region_map, idom_cfg)?;
                self.builder.inherit_variables(ir_region, idom_ir);
            }
            self.builder.set_region(ir_region);
            let region = cfg
                .region_graph()
                .node_weight(cfg_rid)
                .ok_or_else(|| anyhow!("no region {cfg_rid:?} in cfg"))?;
            let special_terminator = SpecialTerm::from_terminator(&region.terminator);
            for wrapped_insn in &region.insns {
                if special_terminator
                    .as_ref()
                    .is_some_and(|s| s.skips_opcode(wrapped_insn.insn.opcode))
                {
                    continue;
                }
                self.process_insn(cfg_rid, &wrapped_insn.insn, wrapped_insn.addr, region_map)?;
            }
            // Fingerprint contributor for the terminator handlers: the
            // region's last pcode insn.  A region with zero pcode insns is a
            // synthetic tail-call stub (the cfg builder's lowering of a
            // CondBranch arm whose target is out of bounds); the insn that
            // proves its `Call + Return` is the predecessor's conditional
            // branch, so fall back to that (`max` picks one deterministic
            // contributor when several branches share a deduped stub).
            // Without the fallback the stub's nodes carry no fingerprint and
            // fail the validator's always-on non-empty check.
            let term_addr = region
                .insns
                .last()
                .map(|wrapped| wrapped.addr.machine_addr.addr)
                .or_else(|| {
                    cfg.region_predecessors(cfg_rid)
                        .filter_map(|pred| pred.insns.last())
                        .map(|wrapped| wrapped.addr.machine_addr.addr)
                        .max()
                });
            // A `NoReturn` region ending in a DIRECT `Call` has an open control
            // edge (the per-insn loop lifted the `Call`, which does not
            // terminate), so sink it into `Unreachable`.  Gate on the opcode: a
            // `CallOther` NoReturn region already self-terminated inside
            // `handle_call_other`, and terminating it twice fails.
            let noreturn_direct_call =
                matches!(region.terminator, strider_cfg::RegionTerminator::NoReturn)
                    && region
                        .insns
                        .last()
                        .is_some_and(|w| w.insn.opcode == rsleigh::Opcode::Call);
            self.with_lift_addr(term_addr, |s| {
                match special_terminator {
                    Some(SpecialTerm::UnresolvedIndirect { target_vn, addr }) => {
                        s.handle_unresolved_indirect_branch(&target_vn, addr)?;
                    }
                    Some(SpecialTerm::Switch(target_vn, targets)) => {
                        s.handle_switch(cfg_rid, &target_vn, &targets, region_map)?;
                    }
                    Some(SpecialTerm::TailCall(target)) => {
                        s.handle_tail_call(target)?;
                    }
                    None => {
                        if noreturn_direct_call {
                            s.builder.build_unreachable()?;
                        }
                    }
                }
                Ok(())
            })?;
        }
        Ok(())
    }

    /// Wires the region successors no terminator handler wired: only
    /// `Unconditional` regions, whose `handle_branch` is a no-op.  `CondBranch`
    /// and `Switch` are already wired by their handlers.
    fn link_region_edges(&mut self, region_map: &RegionMap) -> Result<()> {
        let cfg = self.cfg;
        for edge_idx in cfg.region_graph().edge_indices() {
            let Some((src, tgt)) = cfg.region_graph().edge_endpoints(edge_idx) else {
                continue;
            };
            let src_terminator = &cfg
                .region_graph()
                .node_weight(src)
                .ok_or_else(|| anyhow!("no region {src:?} in cfg"))?
                .terminator;
            if matches!(src_terminator, strider_cfg::RegionTerminator::Unconditional) {
                self.builder.link_regions(
                    ir_region_of(region_map, src)?,
                    ir_region_of(region_map, tgt)?,
                )?;
            }
        }
        Ok(())
    }
}

/// Marks a region whose terminator pcode insn the per-instruction loop must
/// skip, so the post-loop dispatch can lift it via a dedicated handler.
enum SpecialTerm {
    /// Emits an `IndirectBranch` placeholder anchoring the dispatch varnode.
    UnresolvedIndirect {
        target_vn: rsleigh::Vn,
        addr: strider_cfg::PcodeInsnAddr,
    },
    /// Resolved jump table; lifts to an If-ladder over `targets`.
    Switch(rsleigh::Vn, Vec<u64>),
    /// Branch out of the function (`fn_max_size` exceeded, or below
    /// `start_addr` with `allow_code_before_start_addr=false`).  Lifts to
    /// `Call(IntConst(target)) + Return`.  The synthetic conditional-tail-call
    /// stub region carries this too, but has zero insns to skip.
    TailCall(u64),
}

impl SpecialTerm {
    fn from_terminator(t: &strider_cfg::RegionTerminator) -> Option<Self> {
        match t {
            strider_cfg::RegionTerminator::UnresolvedIndirectBranch { target_vn, addr } => {
                Some(SpecialTerm::UnresolvedIndirect {
                    target_vn: *target_vn,
                    addr: *addr,
                })
            }
            strider_cfg::RegionTerminator::Switch { target_vn, targets } => {
                Some(SpecialTerm::Switch(*target_vn, targets.clone()))
            }
            strider_cfg::RegionTerminator::TailCall { target } => {
                Some(SpecialTerm::TailCall(*target))
            }
            _ => None,
        }
    }

    /// `TailCall` skips `BranchIndirect` as well as `Branch`: when the
    /// orchestrator hints a `known_targets` resolution for an indirect jump
    /// whose target is out of the function, the cfg builder marks the `jmp reg`
    /// a tail call.  Processing the `BranchIndirect` would emit an
    /// `IndirectBranch` and terminate the region, so `handle_tail_call`'s
    /// `build_call` would then fail on an already-terminated region.  A
    /// `CondBranch` never lives in a TailCall region: it keeps its own
    /// terminator, and the stub regions on its out-of-bounds arms have no insns.
    ///
    /// Skipping by opcode alone is safe by region closure:
    /// `RegionBuilder::process_new_insn` finishes a region the moment any
    /// control-flow opcode is processed, so at most one appears per region and
    /// it is always the trailing entry, never an inner pcode op.
    fn skips_opcode(&self, opcode: rsleigh::Opcode) -> bool {
        match self {
            SpecialTerm::UnresolvedIndirect { .. } | SpecialTerm::Switch(..) => {
                opcode == rsleigh::Opcode::BranchIndirect
            }
            SpecialTerm::TailCall(..) => matches!(
                opcode,
                rsleigh::Opcode::Branch | rsleigh::Opcode::BranchIndirect
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    /// AArch64 `ret` is a `BranchIndirect` on `x30` in Sleigh, but the cfg
    /// marks it `RegionTerminator::Return`, so it must route through
    /// `handle_return` and leave `unresolved_branches` empty.
    #[test]
    fn aarch64_bx_lr_lifts_to_cc_return_not_indirect() {
        use strider_ir::node::NodeKind;
        use strider_ir_test_utils::IrWalkerEx;

        let arch = strider_target::SleighArch::aarch64();
        // `probe_regs` consumes the arch, hence the second copy.
        let regs = strider_target::SleighArch::aarch64()
            .probe_regs()
            .expect("probe regs");
        let cc = strider_target::CallingConvention::aarch64_aapcs64()
            .build(&regs)
            .expect("build cc");
        // AArch64 `ret` = 0xD65F03C0, little-endian.
        let reader = rsleigh::mem_readers::BufMemReader::new(vec![0xc0, 0x03, 0x5f, 0xd6], 0x1000);
        let mut sleigh =
            rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
        let cfg = strider_cfg::Builder::for_arch(
            &arch,
            &mut sleigh,
            0x1000,
            &strider_cfg::CfgOptions::default(),
        )
        .build()
        .expect("cfg");
        let lifter = super::Lifter::new(arch, sleigh).expect("lifter");
        let outcome = lifter.build_ir(&cfg, cc).expect("build_ir");

        assert!(
            outcome.unresolved_branches.is_empty(),
            "`ret` must NOT defer an indirect branch; got {} unresolved",
            outcome.unresolved_branches.len(),
        );
        let function = &outcome.function;
        assert!(
            function.has_kind(|k| matches!(k, NodeKind::Return)),
            "`ret` must lift to a CC Return node"
        );
        assert!(
            !function.has_kind(|k| matches!(k, NodeKind::IndirectBranch)),
            "`ret` must NOT emit an IndirectBranch placeholder"
        );
    }

    /// A reused `Lifter` must decode each function from a clean context.  A
    /// Thumb `BLX <imm>` that switches to ARM `globalset`s the `TMode` for its
    /// target, and that commit persists across `lift_one` calls.  Without the
    /// per-function reset in `build_cfg`, lifting Thumb function A (ending in
    /// such a `BLX`) then Thumb function B at the BLX target on the same lifter
    /// decodes B as ARM: it mis-parses and walks off into unmapped memory.
    #[test]
    fn reused_lifter_resets_thumb_context_between_functions() {
        use strider_ir::node::NodeKind;
        use strider_ir_test_utils::IrWalkerEx;

        let arch = strider_target::SleighArch::arm_thumb();
        let regs = strider_target::SleighArch::arm_thumb()
            .probe_regs()
            .expect("regs");
        let cc = strider_target::CallingConvention::arm_aapcs()
            .build(&regs)
            .expect("cc");
        // Buffer at 0x1000:
        //   0x1000: BLX 0x1010  (Thumb T2, switches to ARM at 0x1010)
        //   0x1004: bx lr       ends function A
        //   0x1006: nop x3      padding up to 0x1010
        //   0x1010: bx lr; nop; bx lr; nop   function B, valid Thumb
        let code = vec![
            0x00, 0xf0, 0x03, 0xe8, // 0x1000 BLX 0x1010
            0x70, 0x47, // 0x1004 bx lr
            0x00, 0xbf, 0x00, 0xbf, 0x00, 0xbf, // 0x1006 nop padding
            0x70, 0x47, 0x00, 0xbf, 0x70, 0x47, 0x00, 0xbf, // 0x1010 function B
        ];
        let empty = rustc_hash::FxHashMap::default();
        let opts = strider_cfg::CfgOptions::default();
        let new_lifter = || {
            let reader = rsleigh::mem_readers::BufMemReader::new(code.clone(), 0x1000);
            let sleigh =
                rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
            super::Lifter::new(arch, sleigh).expect("lifter")
        };

        // Lift A (the polluter) first, then B on the same engine.
        let mut lifter = new_lifter();
        let cfg_a = lifter
            .build_cfg(0x1000u64.into(), &opts, &empty)
            .expect("A cfg");
        lifter.build_ir(&cfg_a, cc.clone()).expect("A ir");
        let cfg_b = lifter.build_cfg(0x1010u64.into(), &opts, &empty).expect(
            "reused lifter must reset context so B's Thumb decode does not inherit A's ARM mode",
        );
        let reused_b = lifter.build_ir(&cfg_b, cc.clone()).expect("B ir");
        assert!(
            reused_b
                .function
                .has_kind(|k| matches!(k, NodeKind::Return)),
            "B (Thumb `bx lr`) lifted after A must decode as Thumb and emit a Return"
        );

        // Same B on a fresh lifter: the ground-truth decode.
        let mut fresh = new_lifter();
        let cfg_fresh = fresh
            .build_cfg(0x1010u64.into(), &opts, &empty)
            .expect("fresh B cfg");
        let fresh_b = fresh.build_ir(&cfg_fresh, cc).expect("fresh B ir");
        assert_eq!(
            reused_b.function.count_kind(|_| true),
            fresh_b.function.count_kind(|_| true),
            "B lifted after A must have the same node count as a fresh Thumb lift"
        );
    }
}
