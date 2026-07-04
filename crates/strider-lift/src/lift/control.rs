use crate::lift::pcode_util::nth_input_or_err;
use anyhow::{Result, anyhow, bail};
use strider_ir::IRBuilderExt;

use super::FunctionLifter;

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
    pub(super) fn handle_branch(&mut self) -> Result<()> {
        // Nothing to do per-region.  An unconditional pcode `Branch`
        // produces a single `Unconditional` CFG edge to its successor,
        // and EVERY unconditional successor — branch or plain
        // fall-through — is wired uniformly by the post-loop region
        // linker (`pipeline.rs::link_region_edges`).  Lowering an
        // explicit IR branch here as well would double-link the
        // successor (and thus its `Phi` predecessor inputs).
        Ok(())
    }

    /// Lifts a region whose CFG terminator is
    /// [`strider_cfg::RegionTerminator::Switch`] into a single
    /// `Switch` node with one control output per target.
    ///
    /// `region_id` is the dispatch region (the one terminated by
    /// the Switch).  For each target machine address in `targets`,
    /// the helper looks up the corresponding CFG region via
    /// [`strider_cfg::Cfg::region_id_at_start`] and resolves it to an IR
    /// region through `region_map` (via [`super::ir_region_of`]).
    ///
    /// # Errors
    ///
    /// Returns an error when `targets` is empty, when a target
    /// machine address has no matching CFG region, or propagates IR
    /// construction failures from `read_vn` / `build_branch` /
    /// `build_switch`.
    pub(crate) fn handle_switch(
        &mut self,
        region_id: strider_cfg::RegionId,
        target_vn: &rsleigh::Vn,
        targets: &[u64],
        region_map: &super::RegionMap,
    ) -> Result<()> {
        if targets.is_empty() {
            bail!("switch terminator at region {region_id:?} has no targets");
        }
        // Resolve every target machine address to its IR region.
        // The cfg builder enqueues each target with a `Branch` edge
        // as it constructs the Switch (see
        // `cfg/src/cfg/builder/region_builder.rs:436`), so each
        // target IS the start of a region by lift time.
        let mut targets_and_regions: Vec<(u64, strider_ir::RegionId)> =
            Vec::with_capacity(targets.len());
        for &target in targets {
            let machine_addr = strider_cfg::MachineInsnAddr::from(target);
            let cfg_region = self.cfg.region_id_at_start(machine_addr).ok_or_else(|| {
                anyhow!("switch target machine address {target:#x} has no CFG region")
            })?;
            let ir_region = super::ir_region_of(region_map, cfg_region)?;
            targets_and_regions.push((target, ir_region));
        }
        // Read the dispatch value at the region exit — the switch
        // address value.
        let idx = self.read_vn(target_vn)?;
        // n == 1 degenerates to a plain branch (unchanged behavior).
        if targets_and_regions.len() == 1 {
            return self.builder.build_branch(targets_and_regions[0].1);
        }
        let arms: Vec<(strider_ir::RegionId, u64)> = targets_and_regions
            .iter()
            .map(|&(addr, region)| (region, addr))
            .collect();
        self.builder.build_switch(idx, &arms)
    }

    pub(super) fn handle_cond_branch(
        &mut self,
        region_id: strider_cfg::RegionId,
        insn: &rsleigh::Insn,
        region_map: &super::RegionMap,
    ) -> Result<()> {
        let cond_raw = self.read_input(insn, 1)?;
        // Sleigh always feeds `CBRANCH` an already-`I1` condition (a 1-byte
        // comparison / flag result, value 0 or 1 — verified across arches).
        // `build_if` requires `I1`; `truncate_if_needed` is a no-op in that
        // common case.  Should a wider, provably-0/1 value ever reach here
        // (e.g. a flag zero-extended into a multi-byte register), narrowing
        // to the low bit is exact for a 0/1 value and keeps the IR sound.
        let cond = self
            .builder
            .truncate_if_needed(cond_raw, strider_ir::ValueType::I1)?;
        let res = self.cfg.region_if(region_id)?;
        let if_true_region = res
            .if_true_region
            .ok_or_else(|| anyhow!("invalid region index {region_id:?}"))?;
        let if_false_region = res
            .if_false_region
            .ok_or_else(|| anyhow!("invalid region index {region_id:?}"))?;
        let true_block = super::ir_region_of(region_map, if_true_region)?;
        let false_block = super::ir_region_of(region_map, if_false_region)?;
        self.builder.build_if(cond, true_block, false_block)?;
        Ok(())
    }

    /// Lowers a p-code `Return` into the IR's calling-convention-aware return
    /// node, emitting the convention's `ret_val_regs` in ABI order.
    ///
    /// Also handles the `BranchIndirect` link-register-return case (e.g. ARM
    /// `bx lr`): the dispatcher routes both `Opcode::Return` and
    /// `Opcode::BranchIndirect` here, since both emit a CC `Return` (tail
    /// calls / jump tables are split off earlier into dedicated terminators).
    ///
    /// The p-code `Return` op carries a single fabricated input (typically the
    /// popped return address on stack-push ISAs).  That value is *not* an ABI
    /// return slot, so we discard the lifted input here and let the IR resolve
    /// the real return values from the calling convention's resolved register
    /// list.
    pub(super) fn handle_return(&mut self) -> Result<()> {
        // The p-code `Return` input is discarded (see above); nothing from the
        // insn is needed.  `build_cc_return` resolves + reads the CC return
        // registers and terminates the region with a `Return`.
        self.build_cc_return()
    }

    /// Emits a function-ABI `Return` for the function-default calling
    /// convention: resolve the CC's return registers, read each through the
    /// aliasing-aware `read_vn` (which slices a sub-register ret reg out of
    /// its tracked container), then emit the dumb `Return` node.  Terminates
    /// the current region.
    fn build_cc_return(&mut self) -> Result<()> {
        let ret_vns = self.builder.function().ret_val_regs();
        let ret_values = self.read_vns(&ret_vns)?;
        self.builder.build_return(None, &ret_values)
    }

    /// Builds a `Call` from the (override or function-default) calling
    /// convention — the prod call-construction orchestration.  Derives the
    /// ret-val / clobber / arg vns from the CC, reads the args through the
    /// shared vn read path, emits the dumb [`strider_ir::FunctionBuilder::build_call`]
    /// (which reads SP, emits the node, and applies the post-call SP adjust from
    /// `ret_stack_pop` itself), then writes the clobbers then ret-vals back and
    /// records the override CC.  (strider-ir's constructor is dumb: it takes
    /// resolved Vn lists and knows nothing about calling conventions.)
    fn build_cc_call(
        &mut self,
        call_address: strider_ir::Value,
        override_cc: Option<&strider_target::BuiltCallingConvention>,
    ) -> Result<()> {
        // Snapshot the CC-derived ingredients (owned) so the immutable borrow of
        // the function ends before the &mut read / build / write path below.
        let (ret_vns, clobber_vns, arg_vns, ret_stack_pop) = {
            let cc = override_cc.unwrap_or_else(|| self.builder.function().default_cc());
            // One scan yields both the ret-val and clobber lists (they are the
            // two halves of the same projection over `all_vns`).
            let (ret_vns, clobber_vns) =
                cc.ret_and_clobber_vns(self.builder.function().all_vns(), |v| self.container_of(v));
            (
                ret_vns,
                clobber_vns,
                cc.arg_passing_regs.clone(),
                cc.ret_stack_pop,
            )
        };

        let args = self.read_vns(&arg_vns)?;

        let mut output_vns = ret_vns.clone();
        output_vns.extend_from_slice(&clobber_vns);
        // `build_call` reads SP, emits the node, and applies the post-call SP
        // adjust (`ret_stack_pop`) itself — SP is never a clobber/ret-val, so
        // the writebacks below cannot race with it.
        let (call, outputs) =
            self.builder
                .build_call(call_address, &args, &output_vns, ret_stack_pop)?;
        let (ret_vals, clobbers) = outputs.split_at(ret_vns.len());

        // Writeback: clobbers first, then ret-vals (an aliased clobber must not
        // re-clobber the return value).  Clobbers go through `write_variable`
        // (the set legitimately includes CONST / RAM Sleigh temps that the
        // aliasing-aware register path can't slice); ret-vals are REGISTER /
        // UNIQUE, so they take the aliasing-aware `write_vn`.
        for (vn, v) in core::iter::zip(&clobber_vns, clobbers) {
            self.builder.write_variable(vn, *v)?;
        }
        for (vn, v) in core::iter::zip(&ret_vns, ret_vals) {
            self.write_vn(vn, *v)?;
        }

        if let Some(cc) = override_cc {
            self.builder
                .function_mut()
                .side_tables_mut()
                .set_call_cc(call, cc.clone());
        }
        Ok(())
    }

    pub(super) fn handle_call(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // Direct call: the target varnode is in the code space and its offset
        // *is* the target address — it's not a pointer to dereference.
        let target_vn = nth_input_or_err(insn, 0)?;
        let space = target_vn.addr_space;
        let target_addr = target_vn.addr_off;
        let call_address = self.build_addr_const(space, target_addr, "call target space")?;
        // Per-address CC override: when the call target matches a user-supplied
        // entry, build the Call with that CC instead of the function-default.
        // Cloned so the per_address_ccs borrow ends before the &mut build call.
        let override_cc = self.per_address_ccs.get(&target_addr).cloned();
        self.build_cc_call(call_address, override_cc.as_ref())?;
        Ok(())
    }

    /// Lifts a region whose CFG terminator is
    /// [`strider_cfg::RegionTerminator::TailCall`] as
    /// `Call(IntConst(target)) + Return`.  The per-region loop
    /// SKIPS the trailing `Opcode::Branch` insn (see
    /// `pipeline.rs::SpecialTerm::skips_opcode`); this method is the
    /// post-loop handler that emits the `Call + Return` pair.
    ///
    /// This is the lowering the
    /// [`strider_cfg::RegionTerminator::TailCall`] doc-comment promises: a
    /// direct branch out of the function range is semantically a
    /// tail call, so the IR carries the explicit Call (with the
    /// resolved constant target) and a Return that hands the
    /// caller's frame back.
    pub(crate) fn handle_tail_call(&mut self, target: u64) -> Result<()> {
        let default_code_space = self.lifter.sleigh().default_code_space();
        let call_address =
            self.build_addr_const(default_code_space, target, "default code space")?;
        // Per-address CC override applies to lift-time tail calls too.
        let override_cc = self.per_address_ccs.get(&target).cloned();
        self.build_cc_call(call_address, override_cc.as_ref())?;
        // build_cc_return terminates the region unconditionally.
        self.build_cc_return()
    }

    pub(super) fn handle_call_indirect(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // Indirect call: target is a register/memory value holding the address.
        let call_address = self.read_vn(nth_input_or_err(insn, 0)?)?;
        self.build_cc_call(call_address, None)?;
        Ok(())
    }

    /// Lifts a region whose CFG terminator is
    /// [`strider_cfg::RegionTerminator::UnresolvedIndirectBranch`] by emitting
    /// an `IndirectBranch(target_value)` placeholder anchoring
    /// `target_vn`'s lifted value in the IR.  The indirect-branch
    /// resolver inspects this placeholder after the optimiser runs.
    ///
    /// `IndirectBranch [ctrl, mem, target]` carries the same control
    /// and memory snapshot a real `Return` would, so the resolver can
    /// rewrite it in place into a real `Return` for `LinkRegister`
    /// resolutions or splice in a `Call+Return` pair for `Single`
    /// tail-call resolutions without re-walking the CFG.
    ///
    /// The `(addr, placeholder_node)` pair is recorded on
    /// `FunctionLifter::unresolved_branches` so the resolver can correlate
    /// each placeholder node with the offending pcode address.  The node
    /// id (not the lifted value) is recorded so the resolver can read the
    /// placeholder's *current* dispatch input after the optimizer rewrites
    /// it — the lifted value may be `replace_all_uses`-rewired away.
    pub(crate) fn handle_unresolved_indirect_branch(
        &mut self,
        target_vn: &rsleigh::Vn,
        addr: strider_cfg::PcodeInsnAddr,
    ) -> Result<()> {
        // Read target_vn through pcode-lift's register-aliasing path
        // so sub-register dispatches (e.g. `jmp *eax` on x86-64) fold
        // correctly via the same Piece/Insert chain the rest of the
        // lifter uses.
        let target_value = self.read_vn(target_vn)?;
        // IndirectBranch placeholder — the resolver reads slot 2 of this
        // node (its live dispatch input) and inspects the producer.
        let placeholder = self.builder.build_indirect_branch(target_value)?;
        self.unresolved_branches.push((addr, placeholder));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests pinning the `Switch`-node shape `handle_switch` emits.
    //! Cover the IR construction shape in isolation (no Cfg required, via
    //! `FunctionBuilder::build_switch` directly); the integration coverage
    //! that drives the full `handle_switch` → `build_ir` path lives in
    //! `crates/strider-orchestrator/tests/jump_table_lifting.rs`.

    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use strider_ir::node::NodeKind;
    use strider_ir::{FunctionBuilder, IRViewer, IRWalker};
    use strider_ir_test_utils::{IrWalkerEx, RegisterSet};

    /// Build a 4-byte register VN to act as the `idx` source.
    fn idx_vn() -> rsleigh::Vn {
        rsleigh::Vn {
            addr_off: 0x10,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 4,
        }
    }

    /// Set up a builder with a single tracked variable (`idx`), an
    /// entry region, N target regions (one per case), and the
    /// dispatch region as the active region.  Returns the builder,
    /// the lifted `idx` value, and the per-target IR region IDs.
    fn make_builder_with_targets(
        n: usize,
    ) -> (
        FunctionBuilder,
        strider_ir::Value,
        Vec<strider_ir::RegionId>,
    ) {
        let idx = idx_vn();
        let mut b = RegisterSet::new()
            .tracked(idx)
            .build_fn()
            .expect("RegisterSet::build_fn");
        let dispatch = b.create_region_all().expect("dispatch region");
        b.set_entry_region_all(dispatch).expect("set_entry_region");
        let target_regions: Vec<strider_ir::RegionId> = (0..n)
            .map(|_| b.create_region_all().expect("create target region"))
            .collect();
        // Each target region terminates with a Return so `build()`
        // produces a valid graph.  We use the calling convention's
        // empty ret-val list (no values returned) since the test
        // builder has no convention-resolved ret regs.
        for &tr in &target_regions {
            b.set_region(tr);
            b.build_return(None, &[]).expect("target return");
        }
        b.set_region(dispatch);
        let idx_val = b.read_variable(&idx).expect("read idx");
        (b, idx_val, target_regions)
    }

    /// Count `If` nodes via the post-build preorder walk.
    fn count_if_nodes(function: &strider_ir::Function) -> usize {
        function.count_kind(|k| matches!(k, NodeKind::If))
    }

    #[test]
    fn handle_switch_emits_single_switch_node() {
        // Multi-target switch: emits exactly one `Switch` node with one
        // control output per arm (no If-ladder — this is what the
        // handle_switch → build_switch rewrite replaces the ladder with).
        let n = 3;
        let (mut b, idx, regions) = make_builder_with_targets(n);
        let arms: Vec<(strider_ir::RegionId, u64)> = regions
            .iter()
            .enumerate()
            .map(|(i, &r)| (r, 0x1000u64 + i as u64 * 0x1000))
            .collect();
        b.build_switch(idx, &arms).expect("build_switch");
        let function = b.build().expect("build");
        let switches: Vec<_> = function
            .walk_kind(|k| matches!(k, NodeKind::Switch))
            .collect();
        assert_eq!(switches.len(), 1, "exactly one Switch node");
        assert_eq!(
            function.node_outputs(switches[0]).len(),
            n,
            "one control output per arm"
        );
        assert_eq!(
            count_if_nodes(&function),
            0,
            "no If nodes from a Switch-lowered dispatch"
        );
    }
}
