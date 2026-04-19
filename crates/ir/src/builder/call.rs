use super::FunctionBuilder;
use crate::error::{ErrorKind, Result};
use crate::node::{NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};
use smallvec::SmallVec;

impl FunctionBuilder {
    /// Terminates the current region with a `Call` node.
    pub fn build_call(&mut self, call_address: NodeOutputId) -> Result<()> {
        let ctrl = self.cur_region_control()?;
        let memory = self.cur_region_memory()?;

        let arg_passing: SmallVec<[NodeOutputId; 4]> = self
            .arg_passing_vars
            .iter()
            .map(|var| self.read_variable(var))
            .collect::<Result<_>>()?;
        let clobbered: SmallVec<[_; 4]> = self.call_cloberred_variables.iter().copied().collect();

        let clobbered_outputs: SmallVec<[NodeOutputId; 4]> = self
            .call_cloberred_variables
            .iter()
            .map(|var| self.read_variable(var))
            .collect::<Result<_>>()?;

        let cloberred_kinds: SmallVec<[NodeOutputKind; 4]> = clobbered_outputs
            .iter()
            .map(|v| self.graph().output_kind(*v))
            .collect();

        self.validate_value_inputs(&arg_passing)?;
        for k in &cloberred_kinds {
            if !k.is_value() {
                return Err(ErrorKind::ExpectedValue(NodeOutputId::default(), *k).into());
            }
        }
        let addr_kind = self.graph().output_kind(call_address);
        if !addr_kind.is_value() {
            return Err(ErrorKind::ExpectedValue(call_address, addr_kind).into());
        }

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
        Ok(())
    }

    /// Emits a `CallOther` (user-defined op) node and advances the control
    /// and memory chain of the active region.
    ///
    /// `args` are additional arguments to the intrinsic (may be empty).
    /// `output_ty` is `Some` when the source instruction has an output varnode
    /// and `None` when the intrinsic produces no value (e.g. `syscall` without
    /// an explicit return).  Memory is always treated as clobbered.
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
