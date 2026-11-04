//! Semi-pruned SSA phi placement: the iterated dominance frontier of each
//! variable's def sites, minus region-local Sleigh temporaries.
//!
//! `record_insn_effects` is a hand-written mirror of the lift's writes: an
//! extra def costs a dead phi, a missed one miscompiles. Debug builds assert
//! every write through [`FunctionLifter::write_variable`] was recorded.
//! [`strider_ir::FunctionBuilder::build_call`]'s SP rebind bypasses that
//! funnel, covered by [`FunctionLifter::record_cc_call_effects`] recording SP
//! and [`crate::Lifter::check_override_cc`]. Reads over-approximate, and
//! matter for UNIQUE only.

use anyhow::Result;
use rustc_hash::{FxHashMap, FxHashSet};
use strider_cfg::RegionId;

use rsleigh::Opcode;
use strider_ir::node::InitialVnId;
use strider_target::call_other_abi::classify_with;

use super::function_lifter::FunctionLifter;

/// Variables needing a value `Phi` at each region.
pub(crate) type PhiPlacement = FxHashMap<RegionId, FxHashSet<InitialVnId>>;

/// The regions defining each variable.
pub(crate) type DefSites = FxHashMap<InitialVnId, FxHashSet<RegionId>>;

/// Def sites, the current region's writes so far, and the temporaries some
/// region reads before writing.
#[derive(Default)]
struct DefScan {
    defs: DefSites,
    written: FxHashSet<InitialVnId>,
    nonlocal: FxHashSet<InitialVnId>,
}

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    /// Every region's writes, plus the UNIQUE temporaries upward-exposed in
    /// some region (Briggs's non-local names). Sleigh reuses unique offsets
    /// across instructions, so only these need phis.
    pub(crate) fn collect_def_sites(&self) -> Result<(DefSites, FxHashSet<InitialVnId>)> {
        let mut scan = DefScan::default();
        // Mirrors `bind_entry_address_register`, whose write no pcode names.
        if let Some(vn) = self.builder.function().default_cc().entry_address {
            self.add_def(&vn, self.cfg.entry(), &mut scan);
        }
        for r in self.cfg.region_ids() {
            let region = &self.cfg.region_graph()[r];
            scan.written.clear();
            // Fed exactly as the lift feeds its own resolver.
            let mut consts = super::pcode_consts::PcodeConsts::default();
            for wrapped in &region.insns {
                consts.observe(wrapped.addr, &wrapped.insn);
                self.record_insn_effects(&wrapped.insn, r, &mut scan, &consts)?;
            }
            // `handle_tail_call` lifts a CC `Call` + `Return` no pcode names.
            // They place no phi (no successors) but keep the mirror total.
            if let strider_cfg::RegionTerminator::TailCall { target } = region.terminator {
                self.record_cc_call_effects(self.override_at(target.addr), r, &mut scan);
                self.record_cc_return_reads(&mut scan);
            }
        }
        Ok((scan.defs, scan.nonlocal))
    }

    /// A UNIQUE temporary no region reads before writing: dead at every
    /// region boundary.
    pub(crate) fn is_local_temporary(
        &self,
        id: InitialVnId,
        nonlocal: &FxHashSet<InitialVnId>,
    ) -> bool {
        !nonlocal.contains(&id)
            && self.builder.function().initial_vn(id).addr_space == rsleigh::VnSpace::UNIQUE
    }

    /// Mirrors `build_cc_call`. Over-records SP, which `build_call` writes
    /// only for a nonzero `ret_stack_pop`.
    fn record_cc_call_effects(
        &self,
        override_cc: Option<&strider_target::BuiltCallingConvention>,
        r: RegionId,
        scan: &mut DefScan,
    ) {
        let cc = override_cc.unwrap_or_else(|| self.builder.function().default_cc());
        let stack_vn = cc.stack_ptr;
        for vn in cc
            .arg_passing_regs
            .iter()
            .chain(&self.call_float_arg_vns(override_cc))
        {
            self.add_read(vn, scan);
        }
        self.add_read(&stack_vn, scan);

        let (rets, clobbers) = self.call_ret_and_clobber_vns(override_cc);
        for vn in rets.iter().chain(clobbers.iter()) {
            self.add_def(vn, r, scan);
        }
        self.add_def(&stack_vn, r, scan);
    }

    /// Mirrors `build_cc_return`.
    fn record_cc_return_reads(&self, scan: &mut DefScan) {
        for vn in self.builder.function().ret_val_regs() {
            self.add_read(&vn, scan);
        }
    }

    /// The lift's write funnel, checked against the mirror in debug builds.
    pub(crate) fn write_variable(
        &mut self,
        vn: &rsleigh::Vn,
        val: strider_ir::node::ValueId,
    ) -> Result<()> {
        #[cfg(debug_assertions)]
        if let Some(region) = self.current_cfg_region
            && let Some(id) = self
                .builder
                .function()
                .vn_id_of(&self.container_map.container_of(vn))
        {
            debug_assert!(
                self.def_sites
                    .get(&id)
                    .is_some_and(|rs| rs.contains(&region)),
                "{vn:?} is written in {region:?}, which `record_insn_effects` did not record",
            );
        }
        self.builder.write_variable(vn, val)
    }

    fn record_insn_effects(
        &self,
        insn: &rsleigh::Insn,
        r: RegionId,
        scan: &mut DefScan,
        consts: &super::pcode_consts::PcodeConsts,
    ) -> Result<()> {
        // Inputs are read before the output is written.
        for vn in &insn.inputs {
            self.add_read(vn, scan);
        }
        match insn.opcode {
            Opcode::Call | Opcode::CallIndirect => {
                self.record_cc_call_effects(self.call_override(insn), r, scan);
            }
            // Mirrors `build_abi_call_other`; over-records the NoOp class.
            Opcode::CallOther => {
                // Same `built` as the lift, so a bad ABI name fails here too.
                let class = strider_cfg::user_op_of(insn, self.lifter.user_op_names()).and_then(
                    |(_, name)| {
                        classify_with(
                            self.cfg_opts.call_other_overrides,
                            self.lifter.arch().preset(),
                            name,
                        )
                    },
                );
                let abi = match class {
                    Some(class) => class.built(self.lifter.sleigh_regs())?,
                    None => None,
                };
                for vn in abi.iter().flat_map(|abi| abi.implicit_reads.iter()) {
                    self.add_read(vn, scan);
                }
                if let Some(out) = insn.output.as_ref() {
                    self.add_def(out, r, scan);
                }
                for vn in abi.iter().flat_map(|abi| abi.implicit_writes.iter()) {
                    self.add_def(vn, r, scan);
                }
            }
            // Mirrors `handle_store`'s REGISTER-space paths (ARM
            // `vld1.N {dX[i]}`): the resolved register, else the whole file.
            Opcode::Store => {
                if let Some(vn) = self.lifter.register_slot_of(insn, consts) {
                    self.add_def(&vn, r, scan);
                } else if self.lifter.is_register_space_access(insn) {
                    for vn in
                        super::pcode_consts::opaque_clobber_set(self.builder.function().all_vns())
                    {
                        self.add_def(&vn, r, scan);
                    }
                }
            }
            Opcode::Return | Opcode::BranchIndirect => self.record_cc_return_reads(scan),
            Opcode::Branch | Opcode::CondBranch | Opcode::Nop | Opcode::MultiEqual => {}
            // Only their pcode output.
            Opcode::Copy
            | Opcode::Load
            | Opcode::IntEqual
            | Opcode::IntNotEqual
            | Opcode::IntSless
            | Opcode::IntSlessEqual
            | Opcode::IntLess
            | Opcode::IntLessEqual
            | Opcode::IntZext
            | Opcode::IntSext
            | Opcode::IntAdd
            | Opcode::IntSub
            | Opcode::IntCarry
            | Opcode::IntScarry
            | Opcode::IntSborrow
            | Opcode::Int2Comp
            | Opcode::IntNeg
            | Opcode::IntXor
            | Opcode::IntAnd
            | Opcode::IntOr
            | Opcode::IntLeft
            | Opcode::IntRight
            | Opcode::IntSright
            | Opcode::IntMul
            | Opcode::IntDiv
            | Opcode::IntSdiv
            | Opcode::IntRem
            | Opcode::IntSrem
            | Opcode::BoolNeg
            | Opcode::BoolXor
            | Opcode::BoolAnd
            | Opcode::BoolOr
            | Opcode::FloatEqual
            | Opcode::FloatNotEqual
            | Opcode::FloatLess
            | Opcode::FloatLessEqual
            | Opcode::FloatNan
            | Opcode::FloatAdd
            | Opcode::FloatDiv
            | Opcode::FloatMul
            | Opcode::FloatSub
            | Opcode::FloatNeg
            | Opcode::FloatAbs
            | Opcode::FloatSqrt
            | Opcode::FloatInt2Float
            | Opcode::FloatFloat2Float
            | Opcode::FloatTrunc
            | Opcode::FloatCeil
            | Opcode::FloatFloor
            | Opcode::FloatRound
            | Opcode::Indirect
            | Opcode::Piece
            | Opcode::Subpiece
            | Opcode::Cast
            | Opcode::PtrAdd
            | Opcode::PtrSub
            | Opcode::SegmentOp
            | Opcode::CPoolRef
            | Opcode::New
            | Opcode::Insert
            | Opcode::Extract
            | Opcode::Popcount
            | Opcode::Lzcount => {
                if let Some(out) = insn.output.as_ref() {
                    self.add_def(out, r, scan);
                }
            }
        }
        Ok(())
    }

    /// Ignores an untracked (RAM) varnode.
    fn add_def(&self, vn: &rsleigh::Vn, r: RegionId, scan: &mut DefScan) {
        if let Some(id) = self.vn_id_of_container(vn) {
            scan.defs.entry(id).or_default().insert(r);
            scan.written.insert(id);
        }
    }

    /// Marks a UNIQUE read with no earlier write in its region upward-exposed.
    fn add_read(&self, vn: &rsleigh::Vn, scan: &mut DefScan) {
        if vn.addr_space != rsleigh::VnSpace::UNIQUE {
            return;
        }
        if let Some(id) = self.vn_id_of_container(vn)
            && !scan.written.contains(&id)
        {
            scan.nonlocal.insert(id);
        }
    }

    fn vn_id_of_container(&self, vn: &rsleigh::Vn) -> Option<InitialVnId> {
        self.builder
            .function()
            .vn_id_of(&self.container_map.container_of(vn))
    }
}
