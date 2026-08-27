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
        let outputs: Vec<ValueId> = self.function().node_outputs(node).to_vec();

        for (value, vn) in core::iter::zip(&outputs[2..], output_vns) {
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
        self.require_value_kind(call_address)?;
        self.validate_value_inputs(args)?;
        self.validate_call_output_vns(output_vns)?;

        let sp_vn = self.function.stack_vn();
        let sp_value = self.read_variable(&sp_vn)?;

        let ctrl = self.cur_region_control()?;
        let memory = self.cur_region_memory()?;
        let inputs = [ctrl, memory, call_address, sp_value]
            .into_iter()
            .chain(args.iter().copied());
        let (node, outputs) = self.emit_call_node(NodeKind::Call, inputs, output_vns)?;

        // The region stays open.
        self.advance_cur_region_ctrl(outputs[0])?;
        self.advance_cur_region_memory(outputs[1])?;

        // A link-register ISA passes 0.
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
    /// `terminate` closes the region here and requires `advance_memory`
    /// false; otherwise control advances and the region stays open. The
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
        self.validate_value_inputs(args)?;

        let ctrl = self.cur_region_control()?;
        let memory = self.cur_region_memory()?;
        let inputs = [ctrl, memory].into_iter().chain(args.iter().copied());
        let (node, outputs) =
            self.emit_call_node(NodeKind::CallOther { user_op_id }, inputs, output_vns)?;

        // Sinking control into `Unreachable` leaves the memory edge dangling.
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
    /// prod.
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
        // Float argument registers come from a register file the integer list
        // never names, and are APPENDED so an integer argument keeps its slot.
        // By ABI POSITION, truncated at the first untracked one, so float
        // position `j` lands at `arg_vns.len() + j`; registers sharing a
        // container (AAPCS-VFP `d0`/`d1` inside `q0`) each pass their own
        // slice of it.
        let float_arg_vns: SmallVec<[rsleigh::Vn; 4]> = cc
            .float_arg_slots(self.function().all_vns(), |v| {
                vn_container::largest_container_in(self.function().all_vns(), v)
            })
            .into_iter()
            .take_while(Option::is_some)
            .flatten()
            .collect();
        let mut arg_passing: SmallVec<[ValueId; 4]> = SmallVec::new();
        for vn in &arg_vns {
            let c = vn_container::largest_container_in(self.function().all_vns(), vn);
            arg_passing.push(self.read_variable(&c)?);
        }
        for vn in &float_arg_vns {
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
            self.function_mut()
                .side_tables_mut()
                .set_call_cc(call, cc.clone());
        }
        Ok(call)
    }

    /// The lifter's sub-register read: shift the slice out of its container
    /// and truncate. Register endianness, which differs from the data
    /// endianness only on ARM BE8, is not reachable through a mock convention.
    #[cfg(any(test, feature = "test-util"))]
    fn read_arg_slice(&mut self, vn: &rsleigh::Vn) -> Result<ValueId> {
        let container = vn_container::largest_container_in(self.function().all_vns(), vn);
        if container == *vn {
            return self.read_variable(&container);
        }
        let offset_bytes = vn.addr_off - container.addr_off;
        let shift_bits = 8 * match self.function().endianness() {
            strider_target::Endianness::Little => offset_bytes,
            strider_target::Endianness::Big => {
                u64::from(container.size) - u64::from(vn.size) - offset_bytes
            }
        };
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
        // Both footprints, so a RAM or const-space varnode is named as such
        // rather than surfacing later as "variable not found".
        for vn in abi.implicit_reads.iter().chain(abi.implicit_writes.iter()) {
            require_reg_or_unique(vn)?;
        }
        // Implicit reads first, then the explicit pcode operands.
        let mut args: SmallVec<[ValueId; 4]> = SmallVec::new();
        for vn in &abi.implicit_reads {
            let c = vn_container::largest_container_in(self.function().all_vns(), vn);
            args.push(self.read_variable(&c)?);
        }
        args.extend_from_slice(explicit_args);

        // Result then implicit-write clobbers, deduplicated with the result
        // winning ties.
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
        // it.
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
