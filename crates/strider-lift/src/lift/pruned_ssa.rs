//! Cytron pruned-SSA value-phi placement: per region, the iterated dominance
//! frontier of each variable's definition sites.  Placing a phi for every
//! tracked varnode at every region instead is `O(regions * varnodes)`, which
//! reached 4M nodes on a 32 KB kernel function, nearly all dead.
//!
//! Def-sites are collected here in the lifter, not in a generic pass, so they
//! reuse the EXACT write-set logic the lift emits (`container_of` for outputs,
//! the CC ret/clobber projection for calls, the CallOther ABI's implicit
//! writes).  Otherwise "where a phi is placed" could diverge from "what
//! actually gets written".

use rustc_hash::{FxHashMap, FxHashSet};
use strider_cfg::RegionId;

use rsleigh::Opcode;
use strider_ir::node::InitialVnId;
use strider_target::call_other_abi::{CallOtherClass, classify};

use super::call::decode_user_op;
use super::function_lifter::FunctionLifter;

/// Variables needing a value `Phi` at each region.
pub(crate) type PhiPlacement = FxHashMap<RegionId, FxHashSet<InitialVnId>>;

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    /// Exact, not conservative: mirrors every write path the lift emits.
    pub(crate) fn collect_def_sites(&self) -> FxHashMap<InitialVnId, FxHashSet<RegionId>> {
        let mut defs: FxHashMap<InitialVnId, FxHashSet<RegionId>> = FxHashMap::default();
        for r in self.cfg.region_ids() {
            let region = self
                .cfg
                .region_graph()
                .node_weight(r)
                .expect("region id from region_ids() is in the graph");
            for wrapped in &region.insns {
                self.record_insn_defs(&wrapped.insn, r, &mut defs);
            }
        }
        defs
    }

    fn record_insn_defs(
        &self,
        insn: &rsleigh::Insn,
        r: RegionId,
        defs: &mut FxHashMap<InitialVnId, FxHashSet<RegionId>>,
    ) {
        match insn.opcode {
            // A call writes the CC's ret + clobber registers and adjusts SP,
            // none of which appear as pcode outputs, so they come from the CC.
            // Mirrors `build_cc_call`.
            Opcode::Call | Opcode::CallIndirect => {
                let cc = self.call_cc_for(insn);
                let (rets, clobbers) = cc
                    .ret_and_clobber_vns(self.builder.function().all_vns(), |v| {
                        self.container_of(v)
                    });
                for vn in rets.iter().chain(clobbers.iter()) {
                    self.add_def(vn, r, defs);
                }
                self.add_def(&cc.stack_vn, r, defs);
            }
            // Mirrors `build_abi_call_other`: pcode output plus the ABI's
            // implicit writes.
            Opcode::CallOther => {
                if let Some(out) = insn.output.as_ref() {
                    self.add_def(out, r, defs);
                }
                if let Ok((_, name)) = decode_user_op(insn, self.lifter.user_op_names())
                    && let Some(CallOtherClass::Call(abi)) =
                        classify(self.lifter.arch.preset(), name)
                {
                    for wname in abi.implicit_writes {
                        if let Some(vn) = self.lifter.sleigh_regs().name_to_vn(wname) {
                            self.add_def(&vn, r, defs);
                        }
                    }
                }
            }
            // Write no tracked variable.
            Opcode::Store
            | Opcode::Branch
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
    /// registered direct-call target, else the function default.
    fn call_cc_for(&self, insn: &rsleigh::Insn) -> &strider_target::BuiltCallingConvention {
        if insn.opcode == rsleigh::Opcode::Call
            && let Some(target) = insn.inputs.first().map(|v| v.addr_off)
            && let Some(cc) = self.per_address_ccs.get(&target)
        {
            return cc;
        }
        self.builder.function().default_cc()
    }
}
