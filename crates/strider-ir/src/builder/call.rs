use anyhow::anyhow;
use smallvec::SmallVec;

use super::FunctionBuilder;
use crate::IRViewer;
use crate::builder::IRBuilderExt;
use crate::error::Result;
use crate::node::{IntBinaryOp, NodeId, NodeKind, ValueId, ValueKind, VnTypeExt};

use super::require_reg_or_unique;

impl FunctionBuilder {
    /// Low-level call-class node emitter shared by [`Self::build_call`] and
    /// [`Self::build_call_other`].  Builds a node with outputs
    /// `[Control, Memory] ++ one Typed slot per output vn` (each slot's kind
    /// a pure function of the varnode's byte width), then tags every value
    /// output with its varnode via `value_vn`.
    ///
    /// `inputs` is the fully assembled input-edge list (`[ctrl, mem, ...]`);
    /// the caller owns region snapshotting and all post-node control /
    /// memory advancing or termination — those diverge between `Call` and
    /// `CallOther`, so they are NOT handled here.
    ///
    /// Returns `(node, outputs)` where `outputs[0]` is the Control output,
    /// `outputs[1]` the Memory output, and `outputs[2..]` one value per
    /// `output_vns` entry, in order.
    ///
    /// # Errors
    ///
    /// Errors when an `output_vns` varnode's byte size has no matching
    /// [`ValueType`] (the only failure of deriving a slot kind via
    /// `int_for_byte_size`).
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

        // Tag each value output (outputs[2..]) with the tracked varnode it
        // represents so pattern queries can recover it.  Only a TRACKED vn
        // (one with a `VnId`) is tagged — an untracked clobber register
        // carries no meaningful id, so it is left untagged rather than stored
        // as a dangling `Vn`.
        for (value, vn) in core::iter::zip(&outputs[2..], output_vns) {
            self.function_mut().set_vn_for_value(*value, *vn);
        }
        Ok((node, outputs))
    }

    /// Emits a dumb `Call` node into the current region from already-resolved
    /// ingredients: the `call_address` target, the `args` value inputs, and the
    /// combined `output_vns` (ret-vals then clobbers) — each producing one
    /// output slot tagged with its varnode on `value_vn`.  Reads the current
    /// stack-pointer value itself (a `Call` always anchors on SP, and SP is the
    /// CC's stack vn — no caller need pass it).  A `Call` always advances both
    /// control and memory, then models the callee's `ret` on SP: it rebinds SP
    /// to `pre_call_SP + ret_stack_pop` (a stack-push ISA pops the return-
    /// address word; on link-register ISAs `ret_stack_pop == 0`, a no-op).
    /// Returns `(NodeId, output_values)` (one value per `output_vns` entry,
    /// ret-vals first) so the caller can write them back.
    ///
    /// Knows NOTHING about calling conventions: the caller (the lifter) derives
    /// the vns + `ret_stack_pop` from a CC, reads the args, and writes the
    /// outputs back.  `output_vns` are validated by
    /// [`Self::validate_call_output_vns`].
    ///
    /// # Errors
    ///
    /// `NoCurrentRegion` when there is no active region; `ExpectedValue` when
    /// `call_address` is not a value edge; an error when an output varnode is
    /// not REGISTER/UNIQUE or the list has a duplicate; `UnsupportedOutputSize`
    /// when an output varnode's byte size has no matching `ValueType`; an error
    /// when the stack-pointer varnode is not tracked.
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

        // A `Call` always anchors on the stack pointer: read the pre-call SP
        // (also the base for the post-call adjust below).
        let sp_vn = self.function.stack_vn();
        let sp_value = self.read_variable(&sp_vn)?;

        let ctrl = self.cur_region_control()?;
        let memory = self.cur_region_memory()?;
        // Inputs: [ctrl, mem, target, sp] ++ args.
        let inputs = [ctrl, memory, call_address, sp_value]
            .into_iter()
            .chain(args.iter().copied());
        let (node, outputs) = self.emit_call_node(NodeKind::Call, inputs, output_vns)?;

        // A `Call` always advances both control and memory (region stays open).
        self.advance_cur_region_ctrl(outputs[0])?;
        self.advance_cur_region_memory(outputs[1])?;

        // Model the callee's `ret` on SP: rebind SP to `pre_call_SP +
        // ret_stack_pop` (a stack-push ISA pops the return-address word; a
        // link-register ISA passes `ret_stack_pop == 0`, a no-op).  SP is
        // always tracked here — the `read_variable(&sp_vn)` above errors
        // otherwise — so no "is SP tracked?" guard is needed.
        if ret_stack_pop != 0 {
            let sp_ty = sp_vn.int_type()?;
            let const_id = self.build_int_const(ret_stack_pop as u64, sp_ty)?;
            let adjusted =
                self.build_int_binary_operation(sp_value, const_id, IntBinaryOp::Add, sp_ty)?;
            self.write_variable(&sp_vn, adjusted)?;
        }

        Ok((node, outputs[2..].to_vec()))
    }

    /// Validates a Call / CallOther `output_vns` list: every varnode must be in
    /// REGISTER / UNIQUE space, and no varnode may appear in two slots.
    ///
    /// Callers canonicalize sub-register ABI footprints to their largest
    /// tracked container before reaching here (the lifter's CC / ABI projection,
    /// which owns the machine-register `container_of` map, does this), so a
    /// "each output vn is its own container" check would be redundant with that
    /// guarantee and is deliberately not repeated here — the IR is
    /// target-agnostic and no longer carries the container map.
    fn validate_call_output_vns(&self, output_vns: &[rsleigh::Vn]) -> Result<()> {
        for (i, vn) in output_vns.iter().enumerate() {
            require_reg_or_unique(vn)?;
            if output_vns[..i].contains(vn) {
                return Err(anyhow!("duplicate call output varnode {vn:?}"));
            }
        }
        Ok(())
    }

    /// Emits a `CallOther` node into the current region, resolving the
    /// ABI's implicit register/memory footprint itself.
    ///
    /// This is the single IR builder for every IR-emitting `CallOther`
    /// form (the `NoReturn` trap-class and the modeled `Call(abi)`
    /// class of [`strider_target::call_other_abi::classify`]).  The
    /// `NoOp` class skips IR emission entirely (no node is produced).
    ///
    /// Given the lifter-resolved pcode operands (`explicit_args`) and the
    /// vn-resolved [`strider_target::BuiltCallOtherAbi`], this method:
    ///
    /// - Reads each `abi.implicit_reads` register via [`Self::read_variable`]
    ///   (container-resolved) and appends those values after `explicit_args`,
    ///   so the node's value inputs are `explicit_args ++ implicit_read_values`.
    /// - Derives the ret-val group from `output`: when `Some(vn)`, a
    ///   single `Typed(int_for_byte_size(vn.size))` output slot tagged
    ///   with `vn`; when `None`, no ret-val slot.
    /// - Emits one clobber output per `abi.implicit_writes` register,
    ///   each typed by the register's byte width and tagged with its vn.
    /// - Advances the region's memory token IFF `abi.clobbers_memory`.
    /// - Writes the implicit-write clobbers back first (via
    ///   [`Self::write_variable`], matching the `Call` clobber path — a
    ///   full-register write is identical and this unifies clobber
    ///   handling), THEN the result to `output` via [`Self::write_variable`]
    ///   AFTER the clobbers, so an aliased clobber cannot re-clobber the
    ///   result.  The builder now owns the result writeback (it used to be
    ///   the lifter's job).
    ///
    /// This emitter is **name-agnostic**: the caller stamps the user-op name
    /// separately via [`crate::Function::set_call_other_name`] on the returned
    /// node.
    ///
    /// `abi.implicit_reads`, `abi.implicit_writes`, and `output` must all
    /// name REGISTER / UNIQUE varnodes — a CallOther whose `output` is RAM
    /// errors (the reg/unique invariant is intended).
    ///
    /// When `terminate` is `true` (the `NoReturn` class), the region is
    /// closed as part of this call — no separate region-termination
    /// call is needed (and `advance_memory` must be `false`: the trap
    /// advances no memory).
    /// When `terminate` is `false` (the modeled `Call(abi)` class),
    /// the region's control advances to the CallOther's Control output
    /// and the region stays open.
    ///
    /// Inputs of the resulting node:
    /// `[ctrl, mem] ++ explicit_args ++ implicit_reads` (a CallOther is an
    /// opaque intrinsic — it has no call target and no SP anchor).
    /// Outputs: `[Control, Memory] ++ ret_val? ++ clobbers`.
    ///
    /// # Errors
    ///
    /// Returns an error when any `explicit_args` entry is not a value edge,
    /// when an `output_vns` varnode is not REGISTER / UNIQUE or is duplicated,
    /// when a varnode's byte size has no matching [`ValueType`], or when the
    /// region cannot be advanced or terminated.
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
        // Inputs: [ctrl, mem] ++ args (no call target, no SP anchor).
        let inputs = [ctrl, memory].into_iter().chain(args.iter().copied());
        let (node, outputs) =
            self.emit_call_node(NodeKind::CallOther { user_op_id }, inputs, output_vns)?;

        // `terminate` (the NoReturn class) sinks the trap's control edge into
        // an `Unreachable` terminator instead of advancing control; the memory
        // edge is then left dangling, so `advance_memory` must be false.
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

    /// Test-only convenience: build a `Call` from a calling convention, the way
    /// the lifter does in prod (derive the ret-val/clobber/arg vns from the CC,
    /// read the args + SP, emit via the dumb [`Self::build_call`], write the
    /// clobbers then ret-vals back, record the override CC, apply the post-call
    /// SP adjust).  Keeps test call sites off the dumb prod constructor.
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
            // CC arg regs are tracked full-width containers here, so a plain
            // container-resolved variable read matches what the lifter's
            // aliasing dispatch produces (no sub-register slice to insert).
            let c = crate::function::largest_container_in(self.function().all_vns(), vn);
            arg_passing.push(self.read_variable(&c)?);
        }

        let mut output_vns: SmallVec<[rsleigh::Vn; 8]> = ret_val_vars.iter().copied().collect();
        output_vns.extend(clobber_vars.iter().copied());
        // `build_call` reads SP, emits the node, and applies the post-call SP
        // adjust (`ret_stack_pop`) itself.
        let (call, output_values) =
            self.build_call(call_address, &arg_passing, &output_vns, ret_stack_pop)?;
        let (ret_val_values, clobber_values) = output_values.split_at(ret_val_vars.len());

        for (vn, new_val) in core::iter::zip(&clobber_vars, clobber_values) {
            self.write_variable(vn, *new_val)?;
        }
        for (vn, new_val) in core::iter::zip(&ret_val_vars, ret_val_values) {
            // `ret_val_vars` are already container-resolved (via
            // `call_ret_vals_for`), so a direct variable write is exact.
            self.write_variable(vn, *new_val)?;
        }

        if let Some(cc) = override_cc {
            self.function_mut().side_tables_mut().set_call_cc(call, cc.clone());
        }
        Ok(call)
    }

    /// Test-only convenience: terminate the current region with a `Return`
    /// reading exactly the calling convention's return-value registers, the
    /// way the lifter does in prod.  The ret-val regs are already
    /// container-resolved (via [`crate::Function::ret_val_regs`]), so a plain
    /// variable read is exact — no sub-register aliasing needed.
    ///
    /// # Errors
    ///
    /// Propagates [`Self::build_return`] failures, plus a
    /// non-REGISTER/UNIQUE ret-val vn or an untracked ret-val read.
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

    /// Test-only convenience: build a `CallOther` from a vn-resolved ABI, the
    /// way the lifter does in prod.  Delegates to [`Self::build_call_other`].
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
        // Args: implicit-read register values FIRST, then the explicit pcode
        // operands.
        let mut args: SmallVec<[ValueId; 4]> = SmallVec::new();
        for vn in &abi.implicit_reads {
            let c = crate::function::largest_container_in(self.function().all_vns(), vn);
            args.push(self.read_variable(&c)?);
        }
        args.extend_from_slice(explicit_args);

        // Output vns: result then implicit-write clobbers, each canonicalized to
        // its largest tracked container and deduplicated (the result wins ties).
        let result_vn =
            output.map(|vn| crate::function::largest_container_in(self.function().all_vns(), &vn));
        let mut clobber_vns: SmallVec<[rsleigh::Vn; 4]> = SmallVec::new();
        for vn in &abi.implicit_writes {
            let c = crate::function::largest_container_in(self.function().all_vns(), vn);
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
        self.function_mut().side_tables_mut().set_call_other_name(node, name);
        let (ret_val_values, clobber_values) = output_values.split_at(result_vn.iter().count());

        // Writeback: clobbers then the result — both full-container writes via
        // `write_variable` (an aliased clobber must not re-clobber the result).
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
