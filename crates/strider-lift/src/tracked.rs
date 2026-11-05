use anyhow::Result;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::{Lifter, pcode_consts};

impl<R: rsleigh::MemReader> Lifter<R> {
    /// [`Self::scanned_vns`] plus [`Self::convention_vns`].
    pub(crate) fn tracked_vns(
        &self,
        cfg: &strider_cfg::Cfg,
        cc: &strider_target::BuiltCallingConvention,
        cfg_opts: &strider_cfg::CfgOptions,
        per_address_ccs: &FxHashMap<u64, strider_target::BuiltCallingConvention>,
        claims: &strider_ir::AssumptionOptions,
    ) -> Result<FxHashSet<rsleigh::Vn>> {
        let mut tracked = self.scanned_vns(cfg, cfg_opts, claims);
        tracked.extend(self.convention_vns(cfg, cc, cfg_opts, per_address_ccs)?);
        Ok(tracked)
    }

    /// The varnodes `cfg`'s instructions name: pcode operands, `CallOther` ABI
    /// footprints, rounding-mode registers (unless
    /// [`strider_ir::AssumptionOptions::fp_rounding_mode_is_nearest`]), and
    /// registers a LOAD / STORE reaches through the REGISTER space.
    pub(crate) fn scanned_vns(
        &self,
        cfg: &strider_cfg::Cfg,
        cfg_opts: &strider_cfg::CfgOptions,
        claims: &strider_ir::AssumptionOptions,
    ) -> FxHashSet<rsleigh::Vn> {
        let mut tracked: FxHashSet<rsleigh::Vn> = aliasable_vns(cfg).collect();
        self.call_other_footprint_vns(cfg, cfg_opts.call_other_overrides, &mut tracked);
        if !claims.fp_rounding_mode_is_nearest {
            tracked.extend(self.rounding_mode_register_vns(cfg, cfg_opts.call_other_overrides));
        }
        tracked.extend(self.register_space_vns(cfg));
        tracked
    }

    /// The stack pointer, plus the argument registers of each
    /// `per_address_ccs` override `cfg` selects, each checked by
    /// [`Self::check_override_cc`].
    pub(crate) fn convention_vns(
        &self,
        cfg: &strider_cfg::Cfg,
        cc: &strider_target::BuiltCallingConvention,
        cfg_opts: &strider_cfg::CfgOptions,
        per_address_ccs: &FxHashMap<u64, strider_target::BuiltCallingConvention>,
    ) -> Result<Vec<rsleigh::Vn>> {
        let mut vns = vec![cc.stack_ptr];
        for o in override_call_targets(cfg, self.sleigh().default_code_space())
            .filter_map(|addr| cfg_opts.local_entries.get(per_address_ccs, addr))
        {
            self.check_override_cc(o, cc.stack_ptr)?;
            vns.extend(o.arg_passing_regs.iter().chain(&o.arg_passing_regs_float));
        }
        Ok(vns)
    }

    /// The declared register enclosing each resolved REGISTER-space LOAD /
    /// STORE address ([`Self::register_slot_of`]), not the slice, so the set
    /// stays nested.
    fn register_space_vns<'s>(
        &'s self,
        cfg: &'s strider_cfg::Cfg,
    ) -> impl Iterator<Item = rsleigh::Vn> + 's {
        let declared = &self.tables().declared_reg_vns;
        cfg.regions().flat_map(move |region| {
            let mut consts = pcode_consts::PcodeConsts::default();
            region.insns.iter().filter_map(move |wrapped| {
                consts.observe(wrapped.addr, &wrapped.insn);
                self.register_slot_of(&wrapped.insn, &consts)
                    .and_then(|s| vn_container::smallest_enclosing(declared, &s))
            })
        })
    }

    /// The register of each input-less rounding-mode op in `cfg`
    /// ([`strider_target::call_other_abi::rounding_mode_register`]), except one
    /// an override classifies.
    fn rounding_mode_register_vns<'s>(
        &'s self,
        cfg: &'s strider_cfg::Cfg,
        overrides: &'s strider_target::call_other_abi::CallOtherOverrides,
    ) -> impl Iterator<Item = rsleigh::Vn> + 's {
        let preset = self.arch().preset();
        cfg.regions()
            .flat_map(|region| region.insns.iter())
            .map(|wrapped| &wrapped.insn)
            .filter(|insn| insn.opcode == rsleigh::Opcode::CallOther)
            .filter_map(|insn| strider_cfg::user_op_of(insn, self.user_op_names()))
            .filter(|(_, name)| overrides.get(name).is_none())
            .filter_map(move |(_, name)| {
                strider_target::call_other_abi::rounding_mode_register(preset, name)
            })
            .filter_map(|reg| self.sleigh_regs().name_to_vn(reg))
    }

    /// Adds every register a `CallOther` in `cfg` touches through its ABI
    /// footprint (x86-64 `syscall` reads `R10`, writes `R11`). What does not
    /// resolve is skipped; lifting the op reports it.
    pub(crate) fn call_other_footprint_vns(
        &self,
        cfg: &strider_cfg::Cfg,
        overrides: &strider_target::call_other_abi::CallOtherOverrides,
        tracked: &mut rustc_hash::FxHashSet<rsleigh::Vn>,
    ) {
        use strider_target::call_other_abi::{CallOtherClass, CallOtherLookup, classify_with};

        // A hand-built footprint can name any space.
        let aliasable = |vn: &rsleigh::Vn| vn_container::is_aliasable_space(vn.addr_space);
        for insn in cfg
            .regions()
            .flat_map(|region| region.insns.iter())
            .map(|wrapped| &wrapped.insn)
            .filter(|insn| insn.opcode == rsleigh::Opcode::CallOther)
        {
            let Some((_, name)) = strider_cfg::user_op_of(insn, self.user_op_names()) else {
                continue;
            };
            match classify_with(overrides, self.arch().preset(), name) {
                None | Some(CallOtherLookup::Class(CallOtherClass::NoOp)) => {}
                Some(CallOtherLookup::Class(CallOtherClass::Call(abi))) => tracked.extend(
                    abi.implicit_reads
                        .iter()
                        .chain(abi.implicit_writes)
                        .filter_map(|reg| self.sleigh_regs().name_to_vn(reg))
                        .filter(aliasable),
                ),
                Some(CallOtherLookup::Built(abi)) => tracked.extend(
                    abi.implicit_reads
                        .iter()
                        .chain(&abi.implicit_writes)
                        .copied()
                        .filter(aliasable),
                ),
            }
        }
    }
}

/// Every REGISTER / UNIQUE pcode operand in `cfg`: the spaces `read_vn` /
/// `write_vn` alias. CONST becomes a literal and RAM a Load/Store.
pub(crate) fn aliasable_vns(cfg: &strider_cfg::Cfg) -> impl Iterator<Item = rsleigh::Vn> + '_ {
    cfg.regions()
        .flat_map(|region| region.insns.iter())
        .flat_map(|wrapped| wrapped.insn.all_vns())
        .filter(|vn| vn_container::is_aliasable_space(vn.addr_space))
}

/// Each direct call's and tail call's target in `cfg`: the keys a `Call`
/// selects its `per_address_ccs` override by. Mirrors
/// [`crate::FunctionLifter::call_override`] and `handle_tail_call`.
fn override_call_targets(
    cfg: &strider_cfg::Cfg,
    code_space: rsleigh::VnSpace,
) -> impl Iterator<Item = u64> + '_ {
    cfg.regions().flat_map(move |region| {
        region
            .insns
            .iter()
            .filter_map(move |w| strider_cfg::direct_call_target(&w.insn, code_space))
            .chain(match region.terminator {
                strider_cfg::RegionTerminator::TailCall { target } => Some(target.addr),
                _ => None,
            })
    })
}
