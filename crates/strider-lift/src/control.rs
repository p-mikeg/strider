use crate::pcode_util::nth_input_or_err;
use anyhow::{Result, anyhow, bail};
use strider_ir::node::ValueId;
use strider_ir::{IRBuilderExt, IRViewer};

use super::FunctionLifter;

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
    /// The ISA mode the instruction at `machine_addr` itself committed, if any.
    fn isa_mode_committed_at(&self, machine_addr: u64) -> Option<ValueId> {
        self.pending_isa_mode
            .and_then(|(mode, at)| (at == machine_addr).then_some(mode))
    }

    /// Lowers a resolved jump table to one `Switch` node with a control output
    /// per target.  `region_id` is the dispatch region.
    pub(crate) fn handle_switch(
        &mut self,
        region_id: strider_cfg::RegionId,
        target_vn: &rsleigh::Vn,
        targets: &[strider_cfg::ResolvedTarget],
        region_map: &super::RegionMap,
        switch_addr: strider_cfg::PcodeInsnAddr,
    ) -> Result<()> {
        if targets.is_empty() {
            bail!("switch terminator at region {region_id:?} has no targets");
        }
        let mut arms: Vec<(strider_ir::RegionId, u64)> = Vec::with_capacity(targets.len());
        let by_start = self.cfg.switch_arm_regions(region_id);
        for target in targets {
            // `isa_bit` was applied when the arm's region was decoded.
            let target = target.addr;
            let cfg_region = by_start
                .get(&strider_cfg::PcodeInsnAddr::at_machine_start(target))
                .copied()
                .ok_or_else(|| {
                    anyhow!("switch target machine address {target:#x} has no successor region")
                })?;
            arms.push((region_map[&cfg_region], target));
        }
        let idx = self.read_vn(target_vn)?;
        // A widened table is re-derived through this node; new arms need the mode.
        let isa_mode = self.isa_mode_committed_at(switch_addr.machine_addr.addr);
        // A one-arm table stays a `Switch` so the resolver keeps its selector.
        let node = self.builder.build_switch_with_mode(idx, isa_mode, &arms)?;
        self.sites.switch_anchors.push((switch_addr, node));
        Ok(())
    }

    /// `cond != 0` at the condition's own width, as `Xor(Equal(cond, 0), 1):I1`.
    /// p-code does not normalise a branch condition to 0/1, so it is not
    /// narrowed to its low bit.
    fn build_branch_condition(&mut self, cond_raw: ValueId) -> Result<ValueId> {
        let ty = self.builder.value_type(cond_raw)?;
        let zero = self.builder.build_int_const(0u128, ty)?;
        let is_zero = self.builder.build_int_cmp_operation(
            cond_raw,
            zero,
            strider_ir::IntCmpOp::Equal,
            ty,
        )?;
        self.build_logical_not(is_zero)
    }

    pub(super) fn handle_cond_branch(
        &mut self,
        region_id: strider_cfg::RegionId,
        insn: &rsleigh::Insn,
        region_map: &super::RegionMap,
    ) -> Result<()> {
        let cond_raw = self.read_input(insn, 1)?;
        let cond = self.build_branch_condition(cond_raw)?;
        let res = self.cfg.region_if(region_id)?;
        let terminator = self
            .cfg
            .region_graph()
            .node_weight(region_id)
            .map(|r| &r.terminator);
        let if_true_region = res.if_true_region.ok_or_else(|| {
            anyhow!(
                "no successor of region {region_id:?} contains the taken target of {terminator:?}"
            )
        })?;
        let if_false_region = res.if_false_region.ok_or_else(|| {
            anyhow!(
                "region {region_id:?} has no fall-through successor beside the one holding the \
                 taken target of {terminator:?}"
            )
        })?;
        self.builder.build_if(
            cond,
            region_map[&if_true_region],
            region_map[&if_false_region],
        )?;
        Ok(())
    }

    /// Lifts a return, recording its unverified target in
    /// [`super::LiftSites::return_sites`]. The returned values come from the
    /// convention, not the p-code input.
    pub(super) fn handle_sleigh_return(
        &mut self,
        insn: &rsleigh::Insn,
        addr: strider_cfg::PcodeInsnAddr,
    ) -> Result<()> {
        let target = self.read_vn(nth_input_or_err(insn, 0)?)?;
        let ret = self.build_cc_return()?;
        self.sites.return_sites.push((addr, ret, target));
        Ok(())
    }

    fn build_cc_return(&mut self) -> Result<strider_ir::node::NodeId> {
        let ret_vns = self.builder.function().ret_val_regs();
        let ret_values = self.read_vns(&ret_vns)?;
        self.builder.build_return_node(None, &ret_values)
    }

    /// Resolves the call's CC register lists and emits the `Call` node.
    /// `stack_pop`, when given, replaces the CC's `ret_stack_pop`.
    fn build_cc_call(
        &mut self,
        call_address: ValueId,
        override_cc: Option<&strider_target::BuiltCallingConvention>,
        stack_pop: Option<i64>,
    ) -> Result<()> {
        let (ret_vns, clobber_vns, arg_vns, float_arg_vns, ret_stack_pop) = {
            let cc = override_cc.unwrap_or_else(|| self.builder.function().default_cc());
            let (ret_vns, clobber_vns) = self.call_ret_and_clobber_vns(override_cc);
            let float_arg_vns = self.call_float_arg_vns(override_cc);
            (
                ret_vns,
                clobber_vns,
                cc.arg_passing_regs.clone(),
                float_arg_vns,
                stack_pop.unwrap_or(cc.ret_stack_pop),
            )
        };

        // Float args are appended: float ABI position `j` is arg
        // `arg_vns.len() + j`, as `float_arg_index_to_values` expects.
        let mut args = self.read_vns(&arg_vns)?;
        args.extend(self.read_vns(&float_arg_vns)?);

        let mut output_vns = ret_vns.clone();
        output_vns.extend_from_slice(&clobber_vns);
        // `build_call` applies `ret_stack_pop` to SP, which is never an output.
        let (call, outputs) =
            self.builder
                .build_call(call_address, &args, &output_vns, ret_stack_pop)?;
        let (ret_vals, clobbers) = outputs.split_at(ret_vns.len());

        // Clobbers first, so an aliased clobber cannot overwrite a return value.
        for (vn, v) in core::iter::zip(&clobber_vns, clobbers) {
            self.write_variable(vn, *v)?;
        }
        for (vn, v) in core::iter::zip(&ret_vns, ret_vals) {
            self.write_vn(vn, *v)?;
        }

        if let Some(cc) = override_cc {
            self.builder.function_mut().set_call_cc(call, cc.clone());
        }
        Ok(())
    }

    pub(super) fn handle_call(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // The target varnode's offset is the address itself.
        let target_vn = nth_input_or_err(insn, 0)?;
        let call_address = self.build_addr_const(
            target_vn.addr_space,
            target_vn.addr_off,
            "call target space",
        )?;
        let override_cc = self.call_override(insn);
        self.build_cc_call(call_address, override_cc, None)
    }

    /// A direct branch out of the function lifts to `Call(target) + Return`.
    pub(crate) fn handle_tail_call(&mut self, target: u64) -> Result<()> {
        let default_code_space = self.lifter.sleigh().default_code_space();
        let call_address =
            self.build_addr_const(default_code_space, target, "default code space")?;
        let override_cc = self.override_at(target);
        self.build_cc_call(call_address, override_cc, None)?;
        self.build_cc_return()?;
        Ok(())
    }

    /// A call through a `swi` vector (x86 `int imm8`) pushed no return
    /// address, so it pops none.
    pub(super) fn handle_call_indirect(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let call_address = self.read_vn(nth_input_or_err(insn, 0)?)?;
        let trap_vector = match self.builder.node_kind(self.builder.producer(call_address)) {
            strider_ir::node::NodeKind::CallOther { user_op_id } => usize::try_from(*user_op_id)
                .ok()
                .and_then(|id| self.lifter.user_op_names().get(id))
                .is_some_and(|name| name == "swi"),
            _ => false,
        };
        self.build_cc_call(call_address, None, trap_vector.then_some(0))
    }

    /// Emits an `IndirectBranch` placeholder over `target_vn`, with the ISA
    /// mode this instruction committed, if any. The node, not the value, is
    /// recorded: the optimizer may replace the value.
    pub(crate) fn handle_unresolved_indirect_branch(
        &mut self,
        target_vn: &rsleigh::Vn,
        addr: strider_cfg::PcodeInsnAddr,
    ) -> Result<()> {
        let target_value = self.read_vn(target_vn)?;
        // An earlier commit is the flowing mode, not an interworking switch.
        let isa_mode = self.isa_mode_committed_at(addr.machine_addr.addr);
        let placeholder = self
            .builder
            .build_indirect_branch_with_mode(target_value, isa_mode)?;
        self.sites.unresolved_branches.push((addr, placeholder));
        Ok(())
    }
}
