//! Cytron pruned-SSA value-phi placement: per region, the iterated dominance
//! frontier of each variable's definition sites.
//!
//! Def-sites are collected in the lifter, but `record_insn_defs` is a
//! HAND-WRITTEN mirror of the lift's write paths, not a shared code path with
//! them: nothing cross-checks the two, so a change to what the lift writes has
//! to be made here as well. A def recorded that the lift never writes only
//! costs a dead phi; one the lift writes and this misses loses the phi and
//! miscompiles.

use anyhow::Result;
use rustc_hash::{FxHashMap, FxHashSet};
use strider_cfg::RegionId;

use rsleigh::Opcode;
use strider_ir::node::InitialVnId;
use strider_target::call_other_abi::classify_with;

use super::call::decode_user_op;
use super::function_lifter::FunctionLifter;

/// Variables needing a value `Phi` at each region.
pub(crate) type PhiPlacement = FxHashMap<RegionId, FxHashSet<InitialVnId>>;

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    /// Every write the lift emits from a region's PCODE, which is what phi
    /// placement needs.
    ///
    /// One path is deliberately absent: a `TailCall` region is lifted into a
    /// full CC `Call` writing the return and clobber registers, but its pcode
    /// is only a `Branch`, so nothing is recorded for it. That is sound because
    /// such a region terminates in `Return`: it has no successors, so an
    /// empty dominance frontier, so no phi anywhere depends on those writes.
    pub(crate) fn collect_def_sites(&self) -> Result<FxHashMap<InitialVnId, FxHashSet<RegionId>>> {
        let mut defs: FxHashMap<InitialVnId, FxHashSet<RegionId>> = FxHashMap::default();
        for r in self.cfg.region_ids() {
            let region = self
                .cfg
                .region_graph()
                .node_weight(r)
                .expect("region id from region_ids() is in the graph");
            // One resolver per region, fed every op in order, exactly as the
            // lift feeds its own: the register a store resolves to must be the
            // same on both sides or a write lands with no phi placed for it.
            let mut consts = super::pcode_consts::PcodeConsts::default();
            for wrapped in &region.insns {
                consts.observe(wrapped.addr, &wrapped.insn);
                self.record_insn_defs(&wrapped.insn, r, &mut defs, &consts)?;
            }
        }
        Ok(defs)
    }

    fn record_insn_defs(
        &self,
        insn: &rsleigh::Insn,
        r: RegionId,
        defs: &mut FxHashMap<InitialVnId, FxHashSet<RegionId>>,
        consts: &super::pcode_consts::PcodeConsts,
    ) -> Result<()> {
        match insn.opcode {
            // A call writes the CC's ret + clobber registers and adjusts SP,
            // none of which appear as pcode outputs, so they come from the CC.
            // Mirrors `build_cc_call`, over-recording SP: `build_call` reads it
            // and only writes it back for a nonzero `ret_stack_pop`.
            Opcode::Call | Opcode::CallIndirect => {
                let override_cc = self.call_cc_override_for(insn);
                let (rets, clobbers) = self.call_ret_and_clobber_vns(override_cc);
                for vn in rets.iter().chain(clobbers.iter()) {
                    self.add_def(vn, r, defs);
                }
                let stack_vn = override_cc
                    .unwrap_or_else(|| self.builder.function().default_cc())
                    .stack_vn;
                self.add_def(&stack_vn, r, defs);
            }
            // Mirrors `build_abi_call_other`: pcode output plus the ABI's
            // implicit writes. Over-records the output for the NoOp class,
            // where `handle_call_other` drops it and emits no node at all.
            Opcode::CallOther => {
                if let Some(out) = insn.output.as_ref() {
                    self.add_def(out, r, defs);
                }
                // Resolved through the same `built` call the lift uses, so an
                // unresolvable ABI register name fails here exactly as it does
                // there instead of silently placing no phi.
                let class = decode_user_op(insn, self.lifter.user_op_names())
                    .ok()
                    .and_then(|(_, name)| {
                        classify_with(self.call_other_overrides, self.lifter.arch.preset(), name)
                    });
                if let Some(class) = class
                    && let Some(abi) = class.built(self.lifter.sleigh_regs())?
                {
                    for vn in &abi.implicit_writes {
                        self.add_def(vn, r, defs);
                    }
                }
            }
            // A STORE into the REGISTER space writes a register, not memory:
            // the sla addresses one that way when an instruction field picks
            // it (ARM `vld1.N {dX[i]}`). Mirrors `handle_store`, which fails
            // the lift when the same address does not fold, so a def is
            // recorded exactly when one is written.
            Opcode::Store => {
                if let Some(vn) = super::pcode_consts::register_store_target(insn, consts) {
                    self.add_def(&vn, r, defs);
                }
            }
            // Write no tracked variable.
            Opcode::Branch
            | Opcode::CondBranch
            | Opcode::Return
            | Opcode::BranchIndirect
            | Opcode::Nop
            | Opcode::MultiEqual => {}
            _ => {
                if let Some(out) = insn.output.as_ref() {
                    self.add_def(out, r, defs);
                }
            }
        }
        Ok(())
    }

    /// A write to a non-tracked varnode (a RAM address) resolves to no
    /// `InitialVnId` and is ignored.
    fn add_def(
        &self,
        vn: &rsleigh::Vn,
        r: RegionId,
        defs: &mut FxHashMap<InitialVnId, FxHashSet<RegionId>>,
    ) {
        let container = self.container_of(vn);
        if let Some(id) = self.builder.function().vn_id_of(&container) {
            defs.entry(id).or_default().insert(r);
        }
    }

    /// Mirrors `handle_call`'s CC selection: the per-address override for a
    /// registered direct-call target, `None` for the function default.
    fn call_cc_override_for(
        &self,
        insn: &rsleigh::Insn,
    ) -> Option<&strider_target::BuiltCallingConvention> {
        if insn.opcode == rsleigh::Opcode::Call
            && let Some(target) = insn.inputs.first().map(|v| v.addr_off)
        {
            return self.per_address_ccs.get(&target);
        }
        None
    }
}
