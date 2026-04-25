use super::FunctionBuilder;
use crate::error::{ErrorKind, Result};
use crate::node::{NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};
use crate::ops::IntBinaryOp;
use smallvec::SmallVec;

impl FunctionBuilder {
    /// Terminates the current region with a `Call` node.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::NoCurrentRegion`] / [`ErrorKind::RegionTerminated`]
    /// when there is no active region to advance, [`ErrorKind::ExpectedValue`]
    /// when `call_address` or any read clobbered/arg-passing variable is not
    /// a value edge, [`ErrorKind::VariableNotFound`] when an arg-passing or
    /// clobbered varnode is not tracked, and [`ErrorKind::UnsupportedOutputSize`]
    /// when the stack-pointer varnode's byte size has no matching
    /// [`NodeOutputType`] (only applicable on stack-push ISAs).
    pub fn build_call(&mut self, call_address: NodeOutputId) -> Result<()> {
        let ctrl = self.cur_region_control()?;
        let memory = self.cur_region_memory()?;

        let arg_passing: SmallVec<[NodeOutputId; 4]> = self
            .arg_passing_vars
            .iter()
            .map(|var| self.read_variable(var))
            .collect::<Result<_>>()?;
        self.validate_value_inputs(&arg_passing)?;

        let clobbered: SmallVec<[_; 4]> = self.call_cloberred_variables.iter().copied().collect();

        // Single pass over clobbered variables: read, validate kind, collect
        // kinds. Preserves the offending NodeOutputId in the error (was
        // previously emitted as NodeOutputId::default() — unactionable).
        let mut cloberred_kinds: SmallVec<[NodeOutputKind; 4]> = SmallVec::new();
        for var in &self.call_cloberred_variables {
            let out = self.read_variable(var)?;
            let k = self.graph().output_kind(out);
            if !k.is_value() {
                return Err(ErrorKind::ExpectedValue(out, k).into());
            }
            cloberred_kinds.push(k);
        }

        let addr_kind = self.graph().output_kind(call_address);
        if !addr_kind.is_value() {
            return Err(ErrorKind::ExpectedValue(call_address, addr_kind).into());
        }

        // Snapshot the pre-call SP before creating the `Call` node, so the
        // post-call adjust consumes the caller's SP as of the call site.
        let sp_pre_call = match self.stack_ptr_vn {
            Some(sp) if self.ret_stack_pop != 0 => {
                self.read_variable_optional(&sp)?.map(|out| (sp, out))
            }
            _ => None,
        };

        let inputs = [ctrl, memory, call_address].into_iter().chain(arg_passing);
        let outputs = [NodeOutputKind::Control, NodeOutputKind::Memory]
            .into_iter()
            .chain(cloberred_kinds);
        let call = self.create_node(NodeKind::Call, inputs, outputs);
        let call_outputs: Vec<_> = self.graph().node_outputs(call).into_iter().collect();

        self.advance_cur_region_ctrl(call_outputs[0])?;
        self.advance_cur_region_memory(call_outputs[1])?;
        for (variable, new_val) in core::iter::zip(clobbered, call_outputs.iter().skip(2)) {
            self.write_variable(&variable, *new_val)?;
        }

        // Model the caller-visible effect of the callee's `ret` on SP: on
        // stack-push ISAs `ret` pops the return-address word, so the
        // caller's post-call SP is `pre_call_SP + ret_stack_pop`.  On
        // link-register ISAs `ret_stack_pop == 0` and we skip this entirely.
        if let Some((sp, pre)) = sp_pre_call {
            let sp_ty: NodeOutputType = sp.size.try_into()?;
            let const_id = self.build_int_const(self.ret_stack_pop as u64, sp_ty);
            let adjusted =
                self.build_int_binary_operation(pre, const_id, IntBinaryOp::Add, sp_ty)?;
            self.write_variable(&sp, adjusted)?;
        }
        Ok(())
    }

    /// Emits a `CallOther` (user-defined op) node and advances the control
    /// and memory chain of the active region.
    ///
    /// `args` are additional arguments to the intrinsic (may be empty).
    /// `output_ty` is `Some` when the source instruction has an output varnode
    /// and `None` when the intrinsic produces no value (e.g. `syscall` without
    /// an explicit return).  Memory is always treated as clobbered.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::NoCurrentRegion`] / [`ErrorKind::RegionTerminated`]
    /// when there is no active region, or [`ErrorKind::ExpectedValue`] when
    /// any element of `args` is not a value edge.
    pub fn build_call_other(
        &mut self,
        user_op_id: u64,
        args: &[NodeOutputId],
        output_ty: Option<NodeOutputType>,
    ) -> Result<Option<NodeOutputId>> {
        let ctrl = self.cur_region_control()?;
        let memory = self.cur_region_memory()?;

        self.validate_value_inputs(args)?;

        let mut output_kinds: SmallVec<[NodeOutputKind; 3]> = SmallVec::new();
        output_kinds.push(NodeOutputKind::Control);
        output_kinds.push(NodeOutputKind::Memory);
        if let Some(ty) = output_ty {
            output_kinds.push(NodeOutputKind::OutputType(ty));
        }

        let inputs = [ctrl, memory].into_iter().chain(args.iter().copied());
        let node = self.create_node(NodeKind::CallOther { user_op_id }, inputs, output_kinds);
        let outputs: SmallVec<[NodeOutputId; 3]> =
            self.graph().node_outputs(node).into_iter().collect();
        self.advance_cur_region_ctrl(outputs[0])?;
        self.advance_cur_region_memory(outputs[1])?;
        Ok(outputs.get(2).copied())
    }
}
