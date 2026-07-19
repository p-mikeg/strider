use anyhow::anyhow;
use smallvec::SmallVec;

use super::FunctionBuilder;
use crate::IRViewer;
use crate::builder::IRBuilderExt;
use crate::error::Result;
use crate::node::{IntBinaryOp, NodeId, NodeKind, ValueId, ValueKind, VnTypeExt};

use super::require_reg_or_unique;

impl FunctionBuilder {
    /// Outputs are `[Control, Memory]` then one `Typed` slot per output vn,
    /// each slot's kind derived from the varnode's byte width and tagged with
    /// that varnode on `value_vn`.
    ///
    /// `inputs` must already be fully assembled. Region snapshotting and all
    /// post-node control/memory advancing or termination stay with the caller,
    /// since those diverge between `Call` and `CallOther`.
    fn emit_call_node(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = ValueId>,
        output_vns: &[rsleigh::Vn],
    ) -> Result<(NodeId, Vec<ValueId>)> {
        let mut output_kinds: SmallVec<[ValueKind; 8]> = SmallVec::new();
        output_kinds.push(ValueKind::Control);
        output_kinds.push(ValueKind::Memory);
        for vn in output_vns {
            output_kinds.push(ValueKind::Typed(vn.int_type()?));
        }
        let node = self.create_node(kind, inputs, output_kinds);
        let outputs: Vec<ValueId> = self.function().node_outputs(node).to_vec();

        // Tag each value output so pattern queries can recover its varnode.
        // An untracked clobber register has no meaningful id, so it is left
        // untagged rather than stored as a dangling `Vn`.
        for (value, vn) in core::iter::zip(&outputs[2..], output_vns) {
            self.function_mut().set_vn_for_value(*value, *vn);
        }
        Ok((node, outputs))
    }

    /// `output_vns` is ret-vals then clobbers, each getting one output slot
    /// tagged with its varnode. Returns the output values in that same order so
    /// the caller can write them back.
    ///
    /// Knows nothing about calling conventions: the lifter derives the vns and
    /// `ret_stack_pop` from a CC, reads the args, and does the writeback. SP is
    /// the exception, read here directly, since a `Call` always anchors on it.
    ///
    /// Always advances both control and memory, then models the callee's `ret`
    /// by rebinding SP to `pre_call_SP + ret_stack_pop`.
    pub fn build_call(
        &mut self,
        call_address: ValueId,
        args: &[ValueId],
        output_vns: &[rsleigh::Vn],
        ret_stack_pop: i64,
    ) -> Result<(NodeId, Vec<ValueId>)> {
        self.require_value_kind(call_address)?;
        self.validate_value_inputs(args)?;
        self.validate_call_output_vns(output_vns)?;

        // Also the base for the post-call adjust below.
        let sp_vn = self.function.stack_vn();
        let sp_value = self.read_variable(&sp_vn)?;

        let ctrl = self.cur_region_control()?;
        let memory = self.cur_region_memory()?;
        // Inputs: [ctrl, mem, target, sp] then args.
        let inputs = [ctrl, memory, call_address, sp_value]
            .into_iter()
            .chain(args.iter().copied());
        let (node, outputs) = self.emit_call_node(NodeKind::Call, inputs, output_vns)?;

        // The region stays open.
        self.advance_cur_region_ctrl(outputs[0])?;
        self.advance_cur_region_memory(outputs[1])?;

        // A stack-push ISA pops the return-address word here; a link-register
        // ISA passes 0. SP is guaranteed tracked, since the `read_variable`
        // above would have errored otherwise.
        if ret_stack_pop != 0 {
            let sp_ty = sp_vn.int_type()?;
            let const_id = self.build_int_const(ret_stack_pop as u64, sp_ty)?;
            let adjusted =
                self.build_int_binary_operation(sp_value, const_id, IntBinaryOp::Add, sp_ty)?;
            self.write_variable(&sp_vn, adjusted)?;
        }

        Ok((node, outputs[2..].to_vec()))
    }

    /// Every varnode must be REGISTER / UNIQUE space, and none may appear
    /// twice.
    ///
    /// Deliberately does NOT check that each vn is its own container: callers
    /// canonicalize sub-register ABI footprints upstream, and the container
    /// map is machine-register knowledge the target-agnostic IR does not hold.
    fn validate_call_output_vns(&self, output_vns: &[rsleigh::Vn]) -> Result<()> {
        for (i, vn) in output_vns.iter().enumerate() {
            require_reg_or_unique(vn)?;
            if output_vns[..i].contains(vn) {
                return Err(anyhow!("duplicate call output varnode {vn:?}"));
            }
        }
        Ok(())
    }

    /// The single IR builder for every IR-emitting `CallOther` form, both the
    /// `NoReturn` trap class and the modeled `Call(abi)` class. The `NoOp`
    /// class emits no node at all.
    ///
    /// Inputs are `[ctrl, mem]` then `args`: an intrinsic has no call target
    /// and no SP anchor. Outputs are `[Control, Memory]`, then the optional
    /// ret-val, then the clobbers, matching `output_vns` order.
    ///
    /// `terminate` closes the region here, so no separate termination call is
    /// needed, and requires `advance_memory` false since a trap advances no
    /// memory. Otherwise control advances and the region stays open.
    ///
    /// Name-agnostic: the caller stamps the user-op name onto the returned
    /// node separately. Writeback of the outputs is the caller's job too, and
    /// must do clobbers before the result so an aliased clobber cannot
    /// re-clobber it.
    pub fn build_call_other(
        &mut self,
        user_op_id: u64,
        args: &[ValueId],
        output_vns: &[rsleigh::Vn],
        advance_memory: bool,
        terminate: bool,
    ) -> Result<(NodeId, Vec<ValueId>)> {
        self.validate_call_output_vns(output_vns)?;
        self.validate_value_inputs(args)?;

        let ctrl = self.cur_region_control()?;
        let memory = self.cur_region_memory()?;
        let inputs = [ctrl, memory].into_iter().chain(args.iter().copied());
        let (node, outputs) =
            self.emit_call_node(NodeKind::CallOther { user_op_id }, inputs, output_vns)?;

        // The NoReturn class sinks the trap's control edge into `Unreachable`
        // rather than advancing it, leaving the memory edge dangling.
        if terminate {
            self.create_node(NodeKind::Unreachable, [outputs[0]], []);
            self.terminate_cur_region().map(|_| ())?;
        } else {
            self.advance_cur_region_ctrl(outputs[0])?;
        }
        if advance_memory {
            self.advance_cur_region_memory(outputs[1])?;
        }

        Ok((node, outputs[2..].to_vec()))
    }

    /// Test-only: the CC-aware `Call` construction the lifter performs in
    /// prod, so test call sites stay off the dumb constructor.
    #[allow(clippy::missing_errors_doc)]
    #[cfg(any(test, feature = "test-util"))]
    pub fn build_call_cc(
        &mut self,
        call_address: ValueId,
        override_cc: Option<&strider_target::BuiltCallingConvention>,
    ) -> Result<NodeId> {
        let cc = override_cc.unwrap_or_else(|| self.function.default_cc());
        let ret_stack_pop = cc.ret_stack_pop;

        let (ret_val_vars, clobber_vars) = crate::cc_ret_and_clobber_vns(self.function(), cc);

        let arg_vns: SmallVec<[rsleigh::Vn; 4]> = cc.arg_passing_regs.iter().copied().collect();
        let mut arg_passing: SmallVec<[ValueId; 4]> = SmallVec::new();
        for vn in &arg_vns {
            // CC arg regs are tracked full-width containers, so a plain
            // container-resolved read matches the lifter's aliasing dispatch
            // with no sub-register slice to insert.
            let c = vn_container::largest_container_in(self.function().all_vns(), vn);
            arg_passing.push(self.read_variable(&c)?);
        }

        let mut output_vns: SmallVec<[rsleigh::Vn; 8]> = ret_val_vars.iter().copied().collect();
        output_vns.extend(clobber_vars.iter().copied());
        let (call, output_values) =
            self.build_call(call_address, &arg_passing, &output_vns, ret_stack_pop)?;
        let (ret_val_values, clobber_values) = output_values.split_at(ret_val_vars.len());

        for (vn, new_val) in core::iter::zip(&clobber_vars, clobber_values) {
            self.write_variable(vn, *new_val)?;
        }
        for (vn, new_val) in core::iter::zip(&ret_val_vars, ret_val_values) {
            // Already container-resolved, so a direct write is exact.
            self.write_variable(vn, *new_val)?;
        }

        if let Some(cc) = override_cc {
            self.function_mut()
                .side_tables_mut()
                .set_call_cc(call, cc.clone());
        }
        Ok(call)
    }

    /// Test-only: terminates the region with a `Return` reading exactly the
    /// convention's return-value registers, as the lifter does in prod. Those
    /// regs arrive container-resolved, so a plain read is exact.
    #[cfg(any(test, feature = "test-util"))]
    pub fn build_function_return(&mut self) -> Result<()> {
        let ret_vars: SmallVec<[rsleigh::Vn; 4]> =
            self.function.ret_val_regs().into_iter().collect();
        let mut ret_values: SmallVec<[ValueId; 4]> = SmallVec::new();
        for var in &ret_vars {
            require_reg_or_unique(var)?;
            ret_values.push(self.read_variable(var)?);
        }
        self.build_return(None, &ret_values)
    }

    /// Test-only: the ABI resolution and writeback the lifter performs in prod
    /// around [`Self::build_call_other`].
    #[allow(clippy::missing_errors_doc, clippy::too_many_arguments)]
    #[cfg(any(test, feature = "test-util"))]
    pub fn build_call_other_abi(
        &mut self,
        user_op_id: u64,
        name: &str,
        explicit_args: &[ValueId],
        abi: &strider_target::BuiltCallOtherAbi,
        output: Option<rsleigh::Vn>,
        terminate: bool,
    ) -> Result<(NodeId, Option<ValueId>)> {
        for vn in &abi.implicit_reads {
            require_reg_or_unique(vn)?;
        }
        // Implicit-read register values first, then the explicit pcode
        // operands.
        let mut args: SmallVec<[ValueId; 4]> = SmallVec::new();
        for vn in &abi.implicit_reads {
            let c = vn_container::largest_container_in(self.function().all_vns(), vn);
            args.push(self.read_variable(&c)?);
        }
        args.extend_from_slice(explicit_args);

        // Result then implicit-write clobbers, each canonicalized to its
        // largest tracked container and deduplicated, result winning ties.
        let result_vn =
            output.map(|vn| vn_container::largest_container_in(self.function().all_vns(), &vn));
        let mut clobber_vns: SmallVec<[rsleigh::Vn; 4]> = SmallVec::new();
        for vn in &abi.implicit_writes {
            let c = vn_container::largest_container_in(self.function().all_vns(), vn);
            if Some(c) == result_vn || clobber_vns.contains(&c) {
                continue;
            }
            clobber_vns.push(c);
        }
        let mut output_vns: SmallVec<[rsleigh::Vn; 8]> = result_vn.into_iter().collect();
        output_vns.extend(clobber_vns.iter().copied());

        let (node, output_values) = self.build_call_other(
            user_op_id,
            &args,
            &output_vns,
            abi.clobbers_memory,
            terminate,
        )?;
        self.function_mut()
            .side_tables_mut()
            .set_call_other_name(node, name);
        let (ret_val_values, clobber_values) = output_values.split_at(result_vn.iter().count());

        // Clobbers before the result, so an aliased clobber cannot re-clobber
        // it. Both are full-container writes.
        for (vn, value) in core::iter::zip(&clobber_vns, clobber_values) {
            self.write_variable(vn, *value)?;
        }
        let result = ret_val_values.first().copied();
        if let (Some(c), Some(value)) = (result_vn, result) {
            self.write_variable(&c, value)?;
        }
        Ok((node, result))
    }
}
