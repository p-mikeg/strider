use crate::lift::pcode_util::nth_input_or_err;
use anyhow::{Result, anyhow, bail};
use strider_ir::IRBuilderExt;

use super::FunctionLifter;

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
    pub(super) fn handle_branch(&mut self) -> Result<()> {
        // Every unconditional successor, branch or fall-through alike, is
        // wired uniformly by `link_region_edges`.  Emitting an IR branch here
        // too would double-link the successor and its `Phi` predecessor
        // inputs.
        Ok(())
    }

    /// Lowers a resolved jump table to one `Switch` node with a control output
    /// per target.  `region_id` is the dispatch region.
    pub(crate) fn handle_switch(
        &mut self,
        region_id: strider_cfg::RegionId,
        target_vn: &rsleigh::Vn,
        targets: &[u64],
        region_map: &super::RegionMap,
        switch_addr: strider_cfg::PcodeInsnAddr,
    ) -> Result<()> {
        if targets.is_empty() {
            bail!("switch terminator at region {region_id:?} has no targets");
        }
        // The cfg builder enqueues each target while constructing the Switch,
        // so every target starts a region by lift time.
        let mut arms: Vec<(strider_ir::RegionId, u64)> = Vec::with_capacity(targets.len());
        // One pass over the successor list, not one per target: a wide kernel
        // dispatch table is re-scanned on every re-lift round otherwise.
        let by_start = self.cfg.switch_arm_regions(region_id);
        for &target in targets {
            let machine_addr = strider_cfg::MachineInsnAddr::from(target);
            let cfg_region = by_start
                .get(&strider_cfg::PcodeInsnAddr::at_machine_start(
                    machine_addr.addr,
                ))
                .copied()
                .ok_or_else(|| {
                    anyhow!("switch target machine address {target:#x} has no successor region")
                })?;
            let ir_region = super::ir_region_of(region_map, cfg_region)?;
            arms.push((ir_region, target));
        }
        let idx = self.read_vn(target_vn)?;
        // A one-arm table stays a `Switch`, which holds the dispatch selector
        // the resolver re-reads to widen a site that seated early.  A plain
        // branch drops the selector as dead and latches the first answer.
        let node = self.builder.build_switch(idx, &arms)?;
        self.switch_anchors.push((switch_addr, node));
        Ok(())
    }

    pub(super) fn handle_cond_branch(
        &mut self,
        region_id: strider_cfg::RegionId,
        insn: &rsleigh::Insn,
        region_map: &super::RegionMap,
    ) -> Result<()> {
        let cond_raw = self.read_input(insn, 1)?;
        // Sleigh always feeds `CBRANCH` an already-`I1` condition (verified
        // across arches), so this truncate is normally a no-op.  If a wider
        // provably-0/1 value ever arrives (a flag zero-extended into a
        // multi-byte register), narrowing to the low bit is exact for it.
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

    /// Also serves the link-register return (ARM `bx lr`), which emits a CC
    /// `Return` too.
    ///
    /// The p-code `Return` carries a fabricated input, typically the popped
    /// return address on stack-push ISAs.  That is not an ABI return slot, so
    /// it is discarded and the real return values come from the CC.
    pub(super) fn handle_return(&mut self) -> Result<()> {
        self.build_cc_return()
    }

    /// Reads each CC return register through the aliasing-aware `read_vn`, so a
    /// sub-register ret reg is sliced out of its container.
    fn build_cc_return(&mut self) -> Result<()> {
        let ret_vns = self.builder.function().ret_val_regs();
        let ret_values = self.read_vns(&ret_vns)?;
        self.builder.build_return(None, &ret_values)
    }

    /// Resolves the call's CC register lists and emits the `Call` node.
    fn build_cc_call(
        &mut self,
        call_address: strider_ir::Value,
        override_cc: Option<&strider_target::BuiltCallingConvention>,
    ) -> Result<()> {
        // Snapshot the CC-derived pieces so the immutable borrow of the
        // function ends before the &mut read / build / write path below.
        let (ret_vns, clobber_vns, arg_vns, float_arg_vns, ret_stack_pop) = {
            let cc = override_cc.unwrap_or_else(|| self.builder.function().default_cc());
            // Ret-vals and clobbers are two halves of the same projection over
            // `all_vns`, so one scan yields both.
            let (ret_vns, clobber_vns) = self.call_ret_and_clobber_vns(override_cc);
            let float_arg_vns = self.call_float_arg_vns(override_cc);
            (
                ret_vns,
                clobber_vns,
                cc.arg_passing_regs.clone(),
                float_arg_vns,
                cc.ret_stack_pop,
            )
        };

        // APPENDED, never interleaved: an integer argument keeps its `args`
        // position (hence its `call().arg(N)` index), and float ABI position
        // `j` lands at `arg_vns.len() + j`, the index
        // `float_arg_index_to_values` uses on the callee side.
        let mut args = self.read_vns(&arg_vns)?;
        args.extend(self.read_vns(&float_arg_vns)?);

        let mut output_vns = ret_vns.clone();
        output_vns.extend_from_slice(&clobber_vns);
        // `build_call` reads SP and applies the post-call `ret_stack_pop`
        // adjust itself.  SP is never a clobber or ret-val, so the writebacks
        // below cannot race with it.
        let (call, outputs) =
            self.builder
                .build_call(call_address, &args, &output_vns, ret_stack_pop)?;
        let (ret_vals, clobbers) = outputs.split_at(ret_vns.len());

        // Clobbers first, so an aliased clobber cannot re-clobber the return
        // value.  Both groups are drawn from `all_vns()`, so every entry is
        // already a tracked container with no slice to insert.
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
        // The target varnode is in the code space and its offset IS the target
        // address, not a pointer to dereference.
        let target_vn = nth_input_or_err(insn, 0)?;
        let space = target_vn.addr_space;
        let target_addr = target_vn.addr_off;
        let call_address = self.build_addr_const(space, target_addr, "call target space")?;
        // Cloned so the `per_address_ccs` borrow ends before the &mut call.
        let override_cc = self.per_address_ccs.get(&target_addr).cloned();
        self.build_cc_call(call_address, override_cc.as_ref())?;
        Ok(())
    }

    /// A direct branch out of the function range is semantically a tail call,
    /// so it lifts to `Call(IntConst(target)) + Return`.  Runs post-loop; the
    /// per-region loop skipped the trailing `Branch` insn.
    pub(crate) fn handle_tail_call(&mut self, target: u64) -> Result<()> {
        let default_code_space = self.lifter.sleigh().default_code_space();
        let call_address =
            self.build_addr_const(default_code_space, target, "default code space")?;
        let override_cc = self.per_address_ccs.get(&target).cloned();
        self.build_cc_call(call_address, override_cc.as_ref())?;
        self.build_cc_return()
    }

    pub(super) fn handle_call_indirect(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let call_address = self.read_vn(nth_input_or_err(insn, 0)?)?;
        self.build_cc_call(call_address, None)?;
        Ok(())
    }

    /// Emits an `IndirectBranch [ctrl, mem, target]` placeholder anchoring
    /// `target_vn`'s lifted value, carrying the same control and memory
    /// snapshot a real `Return` would, plus the optional ISA-mode input when
    /// this instruction committed one.
    ///
    /// The NODE id, not the lifted value, is recorded against `addr`: the
    /// optimizer may `replace_all_uses` the value away, and the resolver needs
    /// the placeholder's CURRENT dispatch input.
    pub(crate) fn handle_unresolved_indirect_branch(
        &mut self,
        target_vn: &rsleigh::Vn,
        addr: strider_cfg::PcodeInsnAddr,
    ) -> Result<()> {
        // Read through the register-aliasing path so a sub-register dispatch
        // (`jmp *eax` on x86-64) folds via the same chain as everything else.
        let target_value = self.read_vn(target_vn)?;
        // Only a mode this very instruction committed is the branch's own; an
        // earlier commit is already the flowing mode and must not be re-read as
        // an interworking switch.
        let isa_mode = self
            .pending_isa_mode
            .and_then(|(mode, mode_addr)| (mode_addr == addr.machine_addr.addr).then_some(mode));
        let placeholder = self
            .builder
            .build_indirect_branch_with_mode(target_value, isa_mode)?;
        self.unresolved_branches.push((addr, placeholder));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use strider_ir::node::NodeKind;
    use strider_ir::{FunctionBuilder, IRViewer, IRWalker};
    use strider_ir_test_utils::{IrWalkerEx, RegisterSet};

    fn idx_vn() -> rsleigh::Vn {
        rsleigh::Vn {
            addr_off: 0x10,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 4,
        }
    }

    /// Builder with one tracked variable, an entry region, `n` target regions,
    /// and the dispatch region active.
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
        // Each target must terminate for `build()` to produce a valid graph.
        // The test builder has no CC-resolved ret regs, hence the empty list.
        for &tr in &target_regions {
            b.set_region(tr);
            b.build_return(None, &[]).expect("target return");
        }
        b.set_region(dispatch);
        let idx_val = b.read_variable(&idx).expect("read idx");
        (b, idx_val, target_regions)
    }

    fn count_if_nodes(function: &strider_ir::Function) -> usize {
        function.count_kind(|k| matches!(k, NodeKind::If))
    }

    #[test]
    fn handle_switch_emits_single_switch_node() {
        // One `Switch` with a control output per arm.
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
