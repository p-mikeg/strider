//! Semi-pruned SSA value-phi placement: per region, the iterated dominance
//! frontier of each variable's definition sites, with the Sleigh temporaries
//! local to a region dropped from the placement.
//!
//! Def-sites are collected in the lifter, but `record_insn_effects` is a
//! HAND-WRITTEN mirror of the lift's write paths, not a shared code path with
//! them, so a change to what the lift writes has to be made here as well. A
//! def recorded that the lift never writes only costs a dead phi; one the lift
//! writes and this misses loses the phi and miscompiles. Debug builds catch
//! that half for every write the lift itself makes: each routes through
//! [`FunctionLifter::write_variable`], which asserts the mirror covers it.
//! [`strider_ir::FunctionBuilder::build_call`]'s post-call SP rebind is the
//! one production write outside that funnel, made in strider-ir, which cannot
//! reach the guard. [`FunctionLifter::record_cc_call_effects`] records SP for every call, and
//! [`crate::Lifter::check_override_cc`] pins an override's stack pointer
//! to the function's, so the varnode the rebind writes is always one the
//! mirror recorded.
//!
//! The read half has the opposite polarity: a read it misses can drop a needed
//! phi, so it over-approximates, and it governs UNIQUE temporaries alone.

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

/// The def-site scan's state: the regions defining each variable, what the
/// region under scan has written so far, and the temporaries some region read
/// before writing.
#[derive(Default)]
struct DefScan {
    defs: DefSites,
    written: FxHashSet<InitialVnId>,
    nonlocal: FxHashSet<InitialVnId>,
}

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    /// Every write the lift emits from a region's PCODE, which is what phi
    /// placement needs, plus the UNIQUE temporaries upward-exposed in some
    /// region (Briggs's non-local names).
    ///
    /// Sleigh reuses one unique offset per constructor across every instance of
    /// that instruction, so a temporary is written in almost every region and a
    /// phi for it would land at almost every join. It is live across a region
    /// boundary only where one machine instruction's pcode is split across
    /// regions, which is what upward exposure detects.
    pub(crate) fn collect_def_sites(&self) -> Result<(DefSites, FxHashSet<InitialVnId>)> {
        let mut scan = DefScan::default();
        // Mirrors `bind_entry_address_register`, whose write no pcode names.
        if let Some(vn) = self.builder.function().default_cc().entry_address {
            self.add_def(&vn, self.cfg.entry(), &mut scan);
        }
        for r in self.cfg.region_ids() {
            let region = &self.cfg.region_graph()[r];
            scan.written.clear();
            // One resolver per region, fed every op in order, exactly as the
            // lift feeds its own: the register a store resolves to must be the
            // same on both sides or a write lands with no phi placed for it.
            let mut consts = super::pcode_consts::PcodeConsts::default();
            for wrapped in &region.insns {
                consts.observe(wrapped.addr, &wrapped.insn);
                self.record_insn_effects(&wrapped.insn, r, &mut scan, &consts)?;
            }
            // A `TailCall` region's pcode is only a `Branch`, yet
            // `handle_tail_call` lifts it into a full CC `Call` plus `Return`.
            // These defs place no phi (the region has no successors, so an
            // empty dominance frontier); they are recorded so the mirror is
            // total and `write_variable`'s check needs no exemption.
            if let strider_cfg::RegionTerminator::TailCall { target } = region.terminator {
                self.record_cc_call_effects(self.override_at(target.addr), r, &mut scan);
                self.record_cc_return_reads(&mut scan);
            }
        }
        Ok((scan.defs, scan.nonlocal))
    }

    /// A UNIQUE temporary no region reads before writing it: dead at every
    /// region boundary, so a phi for it can feed nothing but other phis.
    /// `id` comes from `vn_id_of`, hence is in the interner.
    pub(crate) fn is_local_temporary(
        &self,
        id: InitialVnId,
        nonlocal: &FxHashSet<InitialVnId>,
    ) -> bool {
        !nonlocal.contains(&id)
            && self.builder.function().initial_vn(id).addr_space == rsleigh::VnSpace::UNIQUE
    }

    /// Mirrors `build_cc_call`: reads the CC's argument registers and SP, then
    /// writes its return and clobber registers, plus SP. Over-records SP, which
    /// `build_call` only writes back for a nonzero `ret_stack_pop`.
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

    /// Mirrors `build_cc_return`: every CC return register is read.
    fn record_cc_return_reads(&self, scan: &mut DefScan) {
        for vn in self.builder.function().ret_val_regs() {
            self.add_read(&vn, scan);
        }
    }

    /// The lift's single write funnel, so the hand-written mirror above can be
    /// checked against it. A variable written in a region the mirror did not
    /// record loses its phi at every join the region reaches.
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
        // Every pcode input is read before this op writes its own output. A
        // space-id or user-op-id operand is CONST and resolves to nothing.
        for vn in &insn.inputs {
            self.add_read(vn, scan);
        }
        match insn.opcode {
            // A call writes the CC's ret + clobber registers and adjusts SP,
            // none of which appear as pcode outputs, so they come from the CC.
            Opcode::Call | Opcode::CallIndirect => {
                self.record_cc_call_effects(self.call_override(insn), r, scan);
            }
            // Mirrors `build_abi_call_other`: pcode output plus the ABI's
            // implicit reads and writes. Over-records the output for the NoOp
            // class, where `handle_call_other` drops it and emits no node at
            // all.
            Opcode::CallOther => {
                // Resolved through the same `built` call the lift uses, so an
                // unresolvable ABI register name fails here exactly as it does
                // there instead of silently placing no phi.
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
            // A STORE into the REGISTER space writes a register, not memory:
            // the sla addresses one that way when an instruction field picks
            // it (ARM `vld1.N {dX[i]}`). Mirrors `handle_store` on both of
            // its paths, the named register when the address resolves and the
            // whole register file when it does not, so a def is recorded for
            // exactly what is written.
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
            // A CC `Return`: reads the return registers, writes nothing.
            Opcode::Return | Opcode::BranchIndirect => self.record_cc_return_reads(scan),
            // Write no tracked variable.
            Opcode::Branch | Opcode::CondBranch | Opcode::Nop | Opcode::MultiEqual => {}
            // Write only their pcode output. Listed, not `_`, so a new opcode
            // must be classified here as in the dispatcher.
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

    /// A write to a non-tracked varnode (a RAM address) resolves to no
    /// `InitialVnId` and is ignored.
    fn add_def(&self, vn: &rsleigh::Vn, r: RegionId, scan: &mut DefScan) {
        if let Some(id) = self.vn_id_of_container(vn) {
            scan.defs.entry(id).or_default().insert(r);
            scan.written.insert(id);
        }
    }

    /// A read reaching no write in its own region leaves the variable
    /// upward-exposed. Tracked for UNIQUE temporaries alone, the only ones
    /// [`Self::is_local_temporary`] prunes.
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
