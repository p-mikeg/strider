use anyhow::anyhow;
use smallvec::SmallVec;

use super::FunctionBuilder;
use crate::IRViewer;
use crate::builder::IRBuilderExt;
use crate::builder::checks::{require_value_kind, validate_value_inputs};
use crate::error::Result;
use crate::node::{IntBinaryOp, NodeId, NodeKind, ValueId, ValueKind, VnTypeExt};

use super::require_reg_or_unique;

impl FunctionBuilder {
    /// Outputs are `[Control, Memory]` then one `Typed` slot per output vn,
    /// each tagged with its varnode. `inputs` must already be fully
    /// assembled; the caller does all control/memory advancing.
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
        let values = kind.output_head_len();
        let outputs: Vec<ValueId> = self.function().node_outputs(node).to_vec();

        for (value, vn) in core::iter::zip(&outputs[values..], output_vns) {
            self.function_mut().set_vn_for_value(*value, *vn);
        }
        Ok((node, outputs))
    }

    /// `output_vns` is ret-vals then clobbers; the returned values come back
    /// in that same order. Writeback is the caller's job.
    ///
    /// Advances both control and memory, then rebinds SP to
    /// `pre_call_SP + ret_stack_pop`.
    pub fn build_call(
        &mut self,
        call_address: ValueId,
        args: &[ValueId],
        output_vns: &[rsleigh::Vn],
        ret_stack_pop: i64,
    ) -> Result<(NodeId, Vec<ValueId>)> {
        require_value_kind(self.function(), call_address)?;
        validate_value_inputs(self.function(), args)?;
        self.validate_call_output_vns(output_vns)?;

        let sp_vn = self.function.stack_vn();
        let sp_value = self.read_variable(&sp_vn)?;
        // A callee can read a register its convention neither passes nor
        // preserves (i386 `regparm`, a static chain, AArch64's indirect-result
        // register), and the call node carries no such value.  A Sleigh
        // temporary and an sla-internal register are scratch no callee sees.
        let internal = &self.internal_registers;
        let reaches_callee = |vn: &&rsleigh::Vn| {
            vn.addr_space == rsleigh::VnSpace::REGISTER
                && vn.size == sp_vn.size
                && !internal.iter().any(|r| vn_container::vn_contains(r, vn))
        };
        for vn in output_vns.iter().filter(reaches_callee) {
            if let Ok(held) = self.read_variable(vn) {
                self.call_register_values.push(held);
            }
        }

        let ctrl = self.cur_region_control()?;
        let memory = self.cur_region_memory()?;
        let inputs = [ctrl, memory, call_address, sp_value]
            .into_iter()
            .chain(args.iter().copied());
        let kind = NodeKind::Call { cc: None };
        let (node, outputs) = self.emit_call_node(kind, inputs, output_vns)?;

        // The region stays open.
        self.advance_cur_region_ctrl(outputs[NodeKind::CTRL_OUT_SLOT])?;
        self.advance_cur_region_memory(outputs[NodeKind::CTRL_MEM_OUT_SLOT])?;

        // A link-register ISA passes 0.
        if ret_stack_pop != 0 {
            let sp_ty = sp_vn.int_type()?;
            let const_id = self.build_int_const(ret_stack_pop as u64, sp_ty)?;
            let adjusted =
                self.build_int_binary_operation(sp_value, const_id, IntBinaryOp::Add, sp_ty)?;
            self.write_variable(&sp_vn, adjusted)?;
        }

        Ok((node, outputs[kind.output_head_len()..].to_vec()))
    }

    /// Every varnode must be REGISTER / UNIQUE space, and none may appear
    /// twice. Does NOT check that a vn is its own container.
    fn validate_call_output_vns(&self, output_vns: &[rsleigh::Vn]) -> Result<()> {
        for (i, vn) in output_vns.iter().enumerate() {
            require_reg_or_unique(vn)?;
            if output_vns[..i].contains(vn) {
                return Err(anyhow!("duplicate call output varnode {vn:?}"));
            }
        }
        Ok(())
    }

    /// Inputs are `[ctrl, mem]` then `args`: an intrinsic has no call target
    /// and no SP anchor. Outputs are `[Control, Memory]`, then the optional
    /// ret-val, then the clobbers, matching `output_vns` order.
    ///
    /// `terminate` closes the region here and ignores `advance_memory`;
    /// otherwise control advances and the region stays open. The
    /// caller stamps the user-op name and writes the outputs back.
    pub fn build_call_other(
        &mut self,
        user_op_id: u64,
        args: &[ValueId],
        output_vns: &[rsleigh::Vn],
        advance_memory: bool,
        terminate: bool,
    ) -> Result<(NodeId, Vec<ValueId>)> {
        self.validate_call_output_vns(output_vns)?;
        validate_value_inputs(self.function(), args)?;

        let ctrl = self.cur_region_control()?;
        let memory = self.cur_region_memory()?;
        let inputs = [ctrl, memory].into_iter().chain(args.iter().copied());
        let kind = NodeKind::CallOther { user_op_id };
        let (node, outputs) = self.emit_call_node(kind, inputs, output_vns)?;

        // Sinking control into `Unreachable` leaves the memory edge dangling,
        // so a terminating op advances neither: the region is closed and has no
        // successor to read either edge. A memory-clobbering op that also never
        // returns (a PowerPC `BUG()` trap) reaches exactly this.
        if terminate {
            self.create_node(
                NodeKind::Unreachable,
                [outputs[NodeKind::CTRL_OUT_SLOT]],
                [],
            );
            self.terminate_cur_region().map(|_| ())?;
        } else {
            self.advance_cur_region_ctrl(outputs[NodeKind::CTRL_OUT_SLOT])?;
            if advance_memory {
                self.advance_cur_region_memory(outputs[NodeKind::CTRL_MEM_OUT_SLOT])?;
            }
        }

        Ok((node, outputs[kind.output_head_len()..].to_vec()))
    }

    /// Test-only: the CC-aware `Call` construction the lifter performs in
    /// prod.
    ///
    /// # Errors
    /// Propagates the variable reads and [`Self::build_call`] unchanged.
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
        // Float argument registers come from a register file the integer list
        // never names, and are APPENDED so an integer argument keeps its slot.
        // By ABI POSITION, truncated at the first untracked one, so float
        // position `j` lands at `arg_vns.len() + j`; registers sharing a
        // container (AAPCS-VFP `d0`/`d1` inside `q0`) each pass their own
        // slice of it.
        let float_arg_vns = cc.float_arg_prefix(self.function().all_vns(), |v| {
            vn_container::largest_container_in(self.function().all_vns(), v)
        });
        let mut arg_passing: SmallVec<[ValueId; 4]> = SmallVec::new();
        for vn in arg_vns.iter().chain(&float_arg_vns) {
            let v = self.read_arg_slice(vn)?;
            arg_passing.push(v);
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
            self.write_variable(vn, *new_val)?;
        }

        if let Some(cc) = override_cc {
            self.function_mut().set_call_cc(call, cc.clone());
        }
        Ok(call)
    }

    /// The lifter's sub-register read: shift the slice out of its container
    /// and truncate, in the register byte order.
    #[cfg(any(test, feature = "test-util"))]
    fn read_arg_slice(&mut self, vn: &rsleigh::Vn) -> Result<ValueId> {
        let container = vn_container::largest_container_in(self.function().all_vns(), vn);
        if container == *vn {
            return self.read_variable(&container);
        }
        let shift_bits = self
            .function()
            .byte_order()
            .register
            .slice_shift_bits(vn, &container)
            .ok_or_else(|| anyhow!("{vn:?} is not inside {container:?}"))?;
        let container_ty = container.int_type()?;
        let whole = self.read_variable(&container)?;
        let shifted =
            self.build_shift_by_const(whole, shift_bits, IntBinaryOp::ShiftRight, container_ty)?;
        self.truncate_if_needed(shifted, vn.int_type()?)
    }

    /// Test-only: terminates the region with a `Return` reading exactly the
    /// convention's return-value registers.
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
}
