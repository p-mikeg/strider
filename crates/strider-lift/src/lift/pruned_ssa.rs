//! Cytron pruned-SSA value-phi placement.
//!
//! The old lifter minted a value `Phi` for EVERY tracked varnode at EVERY
//! region (`O(regions × varnodes)` — millions of dead phis on large functions,
//! e.g. 4M nodes for a 32 KB kernel function).  This module computes, per
//! region, the small set of variables that actually need a phi there: the
//! iterated dominance frontier of each variable's definition sites (Cytron et
//! al.).
//!
//! Def-sites are collected here in the lifter so they reuse the EXACT write-set
//! logic the lift emits — `container_of` for instruction outputs, the CC's
//! ret/clobber projection for calls, the CallOther ABI's implicit writes — so
//! there is no divergence between "where a phi is placed" and "what actually
//! gets written".

use rustc_hash::{FxHashMap, FxHashSet};
use strider_cfg::RegionId;

use strider_ir::node::InitialVnId;
use strider_target::call_other_abi::{CallOtherClass, classify};

use super::call::decode_user_op;
use super::function_lifter::FunctionLifter;

/// The set of variables that need a value `Phi` at each region — the output of
/// Cytron placement, keyed by CFG region.  This is the return type of the
/// generic [`graph_algorithms::dominance::phi_placement`] (via
/// [`super::dominance::DomInfo::iterated_frontier`]).
pub(crate) type PhiPlacement = FxHashMap<RegionId, FxHashSet<InitialVnId>>;

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    /// For each tracked variable, the set of CFG regions that WRITE it.
    ///
    /// Exact — mirrors every write path the lift emits (instruction outputs,
    /// call ret/clobber registers + SP, CallOther implicit writes).
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

    /// Records every tracked variable that `insn` writes into `defs` under
    /// region `r`.
    fn record_insn_defs(
        &self,
        insn: &rsleigh::Insn,
        r: RegionId,
        defs: &mut FxHashMap<InitialVnId, FxHashSet<RegionId>>,
    ) {
        use rsleigh::Opcode;
        match insn.opcode {
            // A direct / indirect call writes the CC's return + clobber
            // registers and adjusts the stack pointer — none of which appear as
            // pcode outputs, so they must come from the CC here (mirrors
            // `build_cc_call`).
            Opcode::Call | Opcode::CallIndirect => {
                let cc = self.call_cc_for(insn);
                let (rets, clobbers) = cc.ret_and_clobber_vns(
                    self.builder.function().all_vns(),
                    |v| self.container_of(v),
                );
                for vn in rets.iter().chain(clobbers.iter()) {
                    self.add_def(vn, r, defs);
                }
                self.add_def(&cc.stack_vn, r, defs);
            }
            // A CallOther writes its pcode output (if any) plus the ABI's
            // implicit-write registers (mirrors `build_abi_call_other`).
            Opcode::CallOther => {
                if let Some(out) = insn.output.as_ref() {
                    self.add_def(out, r, defs);
                }
                if let Ok((_, name)) = decode_user_op(insn, self.lifter.sleigh())
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
            // Pure control / memory ops write no tracked variable.
            Opcode::Store
            | Opcode::Branch
            | Opcode::CondBranch
            | Opcode::Return
            | Opcode::BranchIndirect
            | Opcode::Nop
            | Opcode::MultiEqual => {}
            // Every value-producing op writes its (container-resolved) output.
            _ => {
                if let Some(out) = insn.output.as_ref() {
                    self.add_def(out, r, defs);
                }
            }
        }
    }

    /// Resolves `vn` to its tracked container variable and records region `r` as
    /// one of that variable's definition sites.  Writes to a non-tracked
    /// varnode (e.g. a RAM address) resolve to no `InitialVnId` and are ignored.
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

    /// The calling convention that governs the callee `insn` targets — the
    /// per-address override for a direct call whose target is registered, else
    /// the function default.  Mirrors `handle_call`'s CC selection.
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

