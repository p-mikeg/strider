//! The tracked-varnode universe: every varnode a lift reads or writes as an
//! SSA variable, frozen before the first node is built. Six sources, because
//! a varnode can be named by a pcode operand, a `CallOther` ABI footprint, a
//! rounding-mode claim, a computed REGISTER-space address, or a convention.

use anyhow::Result;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::{Lifter, pcode_consts};

impl<R: rsleigh::MemReader> Lifter<R> {
    /// Every varnode the lift of `cfg` tracks as an SSA variable:
    /// [`Self::scanned_vns`] plus [`Self::convention_vns`].
    ///
    /// Only the set matters: `Function::new` dedups it into containers and
    /// SORTS, which is what fixes `InitialVnId` numbering.
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

    /// The four sources `cfg`'s instructions name: every pcode operand
    /// ([`aliasable_vns`]), every `CallOther` ABI footprint, the register of
    /// each rounding-mode op with no input while
    /// [`strider_ir::AssumptionOptions::fp_rounding_mode_is_nearest`] is off,
    /// and every register a LOAD / STORE reaches through the REGISTER space.
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

    /// The two sources the conventions name: the stack pointer, which a
    /// function may never name yet stack analysis needs, and the argument
    /// registers of each `per_address_ccs` override `cfg` selects, each
    /// checked by [`Self::check_override_cc`].
    ///
    /// `FunctionBuilder::new` seeds the function's OWN convention only, so
    /// an override's argument registers would be untracked: an integer one
    /// fails the lift outright in `read_vns`, a float one gaps
    /// `float_arg_slots` and truncates the call's float arguments there.
    /// Only the overrides THIS cfg selects: a caller passes a binary-wide
    /// table, and walking all of it costs the table per function.
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

    /// Every register a LOAD / STORE addresses through the REGISTER space.
    ///
    /// The address is computed, so the register appears in no pcode operand and
    /// `aliasable_vns` cannot see it; untracked, it has no slot for
    /// `write_vn` to write and the lift fails.
    ///
    /// Silent about an address that does not resolve: the lift takes its
    /// opaque path there, which clobbers registers already in the set.
    ///
    /// The ENCLOSING declared register is seeded, not the resolved slice. A
    /// computed offset need not land on a declared boundary, and seeding a
    /// partially-overlapping slice would break the nesting the tracked set
    /// relies on; see [`Self::register_slot_of`].
    fn register_space_vns<'s>(
        &'s self,
        cfg: &'s strider_cfg::Cfg,
    ) -> impl Iterator<Item = rsleigh::Vn> + 's {
        let declared = self.declared_reg_vns();
        cfg.regions().flat_map(move |region| {
            let mut consts = pcode_consts::PcodeConsts::default();
            region.insns.iter().filter_map(move |wrapped| {
                consts.observe(wrapped.addr, &wrapped.insn);
                self.register_slot_of(&wrapped.insn, &consts)
                    .and_then(|s| vn_container::smallest_enclosing(declared, &s))
            })
        })
    }

    /// The registers the `cfg`'s rounding-mode ops with no input are
    /// ([`strider_target::call_other_abi::rounding_mode_register`]), but for
    /// one an override classifies.
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

    /// Adds to `tracked` every register a `CallOther` in `cfg` touches through
    /// its ABI footprint instead of its pcode operands. x86-64
    /// `syscall` reads `R10` and writes `R11`, and SysV gives neither a role.
    ///
    /// Silent about what it cannot resolve: an unclassified user-op and an ABI
    /// name outside this arch's register table are both errors of lifting the
    /// op, raised there against the op's own name.
    pub(crate) fn call_other_footprint_vns(
        &self,
        cfg: &strider_cfg::Cfg,
        overrides: &strider_target::call_other_abi::CallOtherOverrides,
        tracked: &mut rustc_hash::FxHashSet<rsleigh::Vn>,
    ) {
        use strider_target::call_other_abi::{CallOtherClass, CallOtherLookup, classify_with};

        // Same universe rule as `aliasable_vns`: a footprint a Rust caller
        // built by hand can name any space.
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

/// `Function::new` keeps the largest of nested varnodes and orders them.
///
/// Only REGISTER and UNIQUE are tracked, the two spaces `read_vn` /
/// `write_vn` route through the aliasing path that consults this set; CONST
/// becomes a literal and RAM a Load/Store.  On x86-64 every `call`
/// contributes an `inst_next` CONST and every rip-relative operand a RAM
/// address, so tracking those grows the set, and the per-region variable map
/// `inherit_variables` clones, with the function.
pub(crate) fn aliasable_vns(cfg: &strider_cfg::Cfg) -> impl Iterator<Item = rsleigh::Vn> + '_ {
    cfg.regions()
        .flat_map(|region| region.insns.iter())
        .flat_map(|wrapped| wrapped.insn.all_vns())
        .filter(|vn| vn_container::is_aliasable_space(vn.addr_space))
}

/// Every address a `Call` this cfg holds selects its `per_address_ccs`
/// override by: a direct call's target ([`strider_cfg::direct_call_target`]),
/// and a tail-call terminator's target. Mirror of
/// [`FunctionLifter::call_override`] and `handle_tail_call`.
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
