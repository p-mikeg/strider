use anyhow::anyhow;
use smallvec::SmallVec;

use super::FunctionBuilder;
use crate::IRViewer;
use crate::builder::IRBuilderExt;
use crate::error::Result;
use crate::node::{IntBinaryOp, NodeId, NodeKind, ValueId, ValueKind, VnTypeExt};

use super::require_reg_or_unique;

impl FunctionBuilder {
    /// Shared call-class node emitter.  Emits a `Call` / `CallOther`
    /// node from already-resolved ingredients.  Does **not** read
    /// variables, resolve a calling convention, or rebind variables —
    /// those are the wrapper / caller's job.
    ///
    /// - Snapshots the region's live control + memory edges.
    /// - Outputs are ALWAYS `[Control, Memory]`, then one output per
    ///   `ret_val_vns` entry (the return-value group), then one output per
    ///   `clobber_vns` entry (the havoc'd caller-saved group).  Each such
    ///   output's kind is derived purely from the varnode's byte width:
    ///   `Typed(int_for_byte_size(vn.size))` — a tracked register always
    ///   holds an int value of its byte width, so no read is needed.  The
    ///   Memory output is always present even for a memory-preserving call
    ///   ("you don't have to use it").
    /// - Inputs are `[ctrl, mem]` followed by `target` (when `Some`),
    ///   then `sp_value` (when `Some`), then `arg_values`.  Any
    ///   clobber-read inputs a node kind needs must already be present in
    ///   `arg_values` — this emitter does not auto-read them.  `sp_value`
    ///   is the stack-pointer anchor for `Call`; `CallOther` passes
    ///   `None`.
    /// - When `terminate` is `false`: advances the region's control to
    ///   the node's Control output (region stays open).
    ///   When `terminate` is `true`: marks the region terminated without
    ///   emitting a separate terminator node (used for the `NoReturn`-
    ///   class `CallOther` — the CallOther node itself is the region
    ///   exit).
    /// - Advances the region's memory to the node's Memory output IFF
    ///   `advance_memory` is set (the caller decides whether memory is
    ///   preserved).
    /// - Tags `Function::value_vn[output] = ret_val_vns[i]` for each
    ///   ret-val output, and `Function::value_vn[output] = clobber_vns[i]`
    ///   for each clobber output.
    ///
    /// Returns `(node, ret_val_values, clobber_values)` where
    /// `ret_val_values.len() == ret_val_vns.len()` and
    /// `clobber_values.len() == clobber_vns.len()`.
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion` when no region is active; an error when
    /// any `arg_values` entry is not a value edge, or when a ret-val /
    /// clobber varnode's byte size has no matching [`ValueType`] (the
    /// only failure mode of deriving the slot kind via
    /// `int_for_byte_size`).
    // Many resolved-ingredient channels plus two toggle flags is the
    // natural shape; a builder struct would add boilerplate without
    // simplifying the call sites.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn build_call_kind(
        &mut self,
        kind: NodeKind,
        target: Option<ValueId>,
        sp_value: Option<ValueId>,
        arg_values: &[ValueId],
        output_vns: &[rsleigh::Vn],
        advance_memory: bool,
        terminate: bool,
    ) -> Result<(NodeId, Vec<ValueId>)> {
        self.validate_value_inputs(arg_values)?;
        if let Some(t) = target {
            self.validate_value_inputs(std::slice::from_ref(&t))?;
        }
        if let Some(sp) = sp_value {
            self.validate_value_inputs(std::slice::from_ref(&sp))?;
        }

        // Snapshot the region's live ctrl + mem edges (without
        // terminating).
        let ctrl = self.cur_region_control()?;
        let memory = self.cur_region_memory()?;

        // Outputs: [Control, Memory] ++ one slot per output vn (ret-vals then
        // clobbers, in the caller's order).  Each slot's kind is a pure
        // function of the varnode's byte width — a tracked register always
        // holds an int value of its width, so the kind is
        // `Typed(int_for_byte_size)`.
        let mut output_kinds: SmallVec<[ValueKind; 8]> = SmallVec::new();
        output_kinds.push(ValueKind::Control);
        output_kinds.push(ValueKind::Memory);
        for vn in output_vns {
            output_kinds.push(ValueKind::Typed(vn.int_type()?));
        }

        // Inputs: [ctrl, mem] ++ target? ++ sp_value? ++ arg_values.
        let inputs = [ctrl, memory]
            .into_iter()
            .chain(target)
            .chain(sp_value)
            .chain(arg_values.iter().copied());

        let node = self.create_node(kind, inputs, output_kinds);
        let outputs: Vec<ValueId> = self.function().node_outputs(node).to_vec();

        // `terminate` and `advance_memory` are mutually exclusive: a
        // terminating (no-return) call closes the region, leaving no live
        // memory edge to advance.  Callers passing `terminate = true` must
        // pass `advance_memory = false` (the only such caller, the NoReturn
        // CallOther path in `build_call_other`, does exactly this).
        if terminate {
            // Sink the trap's control edge into an `Unreachable` terminator so
            // "every control edge reaches a terminator" holds (the memory edge
            // is intentionally left dangling — a NoReturn trap advances no
            // memory). Stamped with the current lift address via `create_node`.
            self.create_node(NodeKind::Unreachable, [outputs[0]], []);
            self.terminate_cur_region().map(|_| ())?;
        } else {
            self.advance_cur_region_ctrl(outputs[0])?;
        }
        if advance_memory {
            self.advance_cur_region_memory(outputs[1])?;
        }

        // Tag each output value with the register it represents (via
        // `value_vn`) so pattern queries can recover the varnode for each slot.
        let output_values: Vec<ValueId> = outputs[2..].to_vec();
        for (value, vn) in core::iter::zip(&output_values, output_vns) {
            self.function_mut().side_tables.value_vn.insert(*value, *vn);
        }

        Ok((node, output_values))
    }

    /// Emits a dumb `Call` node into the current region from already-resolved
    /// ingredients: the `call_address` target, the `sp_value` stack-pointer
    /// anchor, the `args` value inputs, and the combined `output_vns`
    /// (ret-vals then clobbers) — each producing one output slot tagged with
    /// its varnode on `value_vn`.  Control always advances; memory advances iff
    /// `advance_memory`.  Returns `(NodeId, output_values)` (one value per
    /// `output_vns` entry, ret-vals first) so the caller can write them back.
    ///
    /// Knows NOTHING about calling conventions: the caller (the lifter) derives
    /// the vns from a CC, reads the args + SP, and writes the outputs back.
    /// `output_vns` are validated by [`Self::validate_call_output_vns`].
    ///
    /// # Errors
    ///
    /// `NoCurrentRegion` when there is no active region; `ExpectedValue` when
    /// `call_address` is not a value edge; an error when an output varnode is
    /// not its own REGISTER/UNIQUE largest container or the list has a
    /// duplicate; `UnsupportedOutputSize` when an output varnode's byte size
    /// has no matching `ValueType`.
    pub fn build_call(
        &mut self,
        call_address: ValueId,
        sp_value: ValueId,
        args: &[ValueId],
        output_vns: &[rsleigh::Vn],
        advance_memory: bool,
    ) -> Result<(NodeId, Vec<ValueId>)> {
        self.require_value_kind(call_address)?;
        self.validate_call_output_vns(output_vns)?;
        self.build_call_kind(
            NodeKind::Call,
            Some(call_address),
            Some(sp_value),
            args,
            output_vns,
            advance_memory,
            false,
        )
    }

    /// Validates a Call / CallOther `output_vns` list: no varnode may appear in
    /// two output slots (each register / temp is clobbered or returned at most
    /// once).  Reg/container shape is intentionally NOT enforced — a real
    /// touched-varnode set includes CONST / RAM Sleigh temps (`Call`) and
    /// sub-register ABI writes (`CallOther`), both legitimately written back
    /// through the variable table.
    fn validate_call_output_vns(&self, output_vns: &[rsleigh::Vn]) -> Result<()> {
        for (i, vn) in output_vns.iter().enumerate() {
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
    /// - Reads each `abi.implicit_reads` register via [`Self::read_reg_vn`]
    ///   and appends those values after `explicit_args`, so the node's
    ///   value inputs are `explicit_args ++ implicit_read_values`.
    /// - Derives the ret-val group from `output`: when `Some(vn)`, a
    ///   single `Typed(int_for_byte_size(vn.size))` output slot tagged
    ///   with `vn`; when `None`, no ret-val slot.
    /// - Emits one clobber output per `abi.implicit_writes` register,
    ///   each typed by the register's byte width and tagged with its vn.
    /// - Advances the region's memory token IFF `abi.clobbers_memory`.
    /// - Writes the implicit-write clobbers back first (via
    ///   [`Self::write_variable`], matching the `Call` clobber path — a
    ///   full-register write is identical and this unifies clobber
    ///   handling), THEN the result to `output` via [`Self::write_reg_vn`]
    ///   AFTER the clobbers, so an aliased clobber cannot re-clobber the
    ///   result.  The builder now owns the result writeback (it used to be
    ///   the lifter's job).
    /// - Records the user-op name on the node and
    ///   stamps `name` on `Graph::call_other_names`.
    ///
    /// `abi.implicit_reads`, `abi.implicit_writes`, and `output` must all
    /// name REGISTER / UNIQUE varnodes — a CallOther whose `output` is RAM
    /// errors (the reg/unique invariant is intended).
    ///
    /// When `terminate` is `true` (the `NoReturn` class), the region is
    /// closed as part of this call — no separate region-termination
    /// call is needed.
    /// When `terminate` is `false` (the modeled `Call(abi)` class),
    /// the region's control advances to the CallOther's Control output
    /// and the region stays open.
    ///
    /// Inputs of the resulting node:
    /// `[ctrl, mem] ++ target? ++ explicit_args ++ implicit_reads`
    /// (CallOther carries no SP anchor — it has no CC stack args).
    /// Outputs: `[Control, Memory] ++ ret_val? ++ clobbers`.
    ///
    /// The result writeback to `output` now stays with the builder: it
    /// writes the result via [`Self::write_reg_vn`] after the clobbers.
    /// `output` is therefore required to name a REGISTER / UNIQUE varnode.
    /// The method still returns the result value for callers that want it.
    ///
    /// # Errors
    ///
    /// Returns an error when any `explicit_args` entry is not a value
    /// edge, when an `abi.implicit_reads` / `abi.implicit_writes` / `output`
    /// varnode is not REGISTER / UNIQUE or cannot be read/written (no
    /// tracked container, unsupported width), when `output` is `Some` but
    /// its varnode byte size has no matching [`ValueType`], or when the
    /// region cannot be advanced or terminated.
    #[allow(clippy::too_many_arguments)]
    pub fn build_call_other(
        &mut self,
        user_op_id: u64,
        name: &str,
        target: Option<ValueId>,
        args: &[ValueId],
        output_vns: &[rsleigh::Vn],
        advance_memory: bool,
        terminate: bool,
    ) -> Result<(NodeId, Vec<ValueId>)> {
        self.validate_call_output_vns(output_vns)?;
        let (node, output_values) = self.build_call_kind(
            NodeKind::CallOther { user_op_id },
            target,
            None,
            args,
            output_vns,
            advance_memory,
            terminate,
        )?;
        self.function_mut().side_tables.call_other_names[node] = Some(name.to_string());
        Ok((node, output_values))
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
        let advance_memory = !cc.preserves_memory;

        let ret_val_vars: SmallVec<[rsleigh::Vn; 4]> =
            self.function.call_ret_vals_for(cc).into_iter().collect();
        let clobber_vars: SmallVec<[rsleigh::Vn; 4]> =
            self.function.call_clobbered_for(cc).into_iter().collect();

        let arg_vns: SmallVec<[rsleigh::Vn; 4]> = cc.arg_passing_regs.iter().copied().collect();
        let mut arg_passing: SmallVec<[ValueId; 4]> = SmallVec::new();
        for vn in &arg_vns {
            arg_passing.push(self.read_reg_vn(vn)?);
        }

        let sp_vn = self.function.stack_vn();
        let sp_value = self.read_variable_optional(&sp_vn)?.ok_or_else(|| {
            anyhow!("build_call_cc: stack-pointer varnode {sp_vn:?} is not tracked")
        })?;

        let mut output_vns: SmallVec<[rsleigh::Vn; 8]> = ret_val_vars.iter().copied().collect();
        output_vns.extend(clobber_vars.iter().copied());
        let (call, output_values) =
            self.build_call(call_address, sp_value, &arg_passing, &output_vns, advance_memory)?;
        let (ret_val_values, clobber_values) = output_values.split_at(ret_val_vars.len());

        for (vn, new_val) in core::iter::zip(&clobber_vars, clobber_values) {
            self.write_variable(vn, *new_val)?;
        }
        for (vn, new_val) in core::iter::zip(&ret_val_vars, ret_val_values) {
            self.write_reg_vn(vn, *new_val)?;
        }

        if let Some(cc) = override_cc {
            self.function_mut().set_call_cc(call, cc.clone());
        }
        self.apply_post_call_sp_adjust(&sp_vn, sp_value, ret_stack_pop)?;
        Ok(call)
    }

    /// Test-only convenience: build a `CallOther` from a vn-resolved ABI, the
    /// way the lifter does in prod.  Delegates to [`Self::build_call_other`].
    #[allow(clippy::missing_errors_doc, clippy::too_many_arguments)]
    #[cfg(any(test, feature = "test-util"))]
    pub fn build_call_other_abi(
        &mut self,
        user_op_id: u64,
        name: &str,
        target: Option<ValueId>,
        explicit_args: &[ValueId],
        abi: &strider_target::BuiltCallOtherAbi,
        output: Option<rsleigh::Vn>,
        terminate: bool,
    ) -> Result<(NodeId, Option<ValueId>)> {
        for vn in &abi.implicit_reads {
            require_reg_or_unique(vn)?;
        }
        if let Some(out_vn) = output.as_ref() {
            require_reg_or_unique(out_vn)?;
        }
        // Args: implicit-read register values FIRST, then the explicit pcode
        // operands.
        let mut args: SmallVec<[ValueId; 4]> = SmallVec::new();
        for vn in &abi.implicit_reads {
            args.push(self.read_reg_vn(vn)?);
        }
        args.extend_from_slice(explicit_args);

        // Output vns: the 0-or-1 result, then one per implicit-write clobber.
        let ret_val_vns: SmallVec<[rsleigh::Vn; 1]> = output.into_iter().collect();
        let clobber_vns: &[rsleigh::Vn] = &abi.implicit_writes;
        let mut output_vns: SmallVec<[rsleigh::Vn; 8]> = ret_val_vns.iter().copied().collect();
        output_vns.extend(clobber_vns.iter().copied());

        let (node, output_values) = self.build_call_other(
            user_op_id,
            name,
            target,
            &args,
            &output_vns,
            abi.clobbers_memory,
            terminate,
        )?;
        let (ret_val_values, clobber_values) = output_values.split_at(ret_val_vns.len());

        // Writeback: clobbers first (write_variable), then the result
        // (write_reg_vn) — an aliased clobber must not re-clobber the result.
        for (vn, value) in core::iter::zip(clobber_vns, clobber_values) {
            self.write_variable(vn, *value)?;
        }
        let result = ret_val_values.first().copied();
        if let (Some(out_vn), Some(value)) = (output.as_ref(), result) {
            self.write_reg_vn(out_vn, value)?;
        }
        Ok((node, result))
    }

    /// `apply_post_call_sp_adjust` helper: model the caller-visible
    /// effect of the callee's `ret` on SP — on stack-push ISAs `ret`
    /// pops the return-address word, so the caller's post-call SP is
    /// `pre_call_SP + ret_stack_pop`.  On link-register ISAs
    /// `ret_stack_pop == 0`, so this is a no-op.  `sp_pre_call` is the
    /// single pre-call SP value [`Self::build_call`] read from the variable
    /// table.
    /// # Errors
    ///
    /// Propagates SP read / const-build / write failures.
    pub fn apply_post_call_sp_adjust(
        &mut self,
        sp: &rsleigh::Vn,
        sp_pre_call: ValueId,
        ret_stack_pop: i64,
    ) -> Result<()> {
        if ret_stack_pop == 0 {
            // Link-register ISAs (and the trivial CC) never adjust SP
            // across a call.
            return Ok(());
        }
        // Only rebind a tracked SP variable; an untracked (sentinel) SP
        // has no variable slot to write back to.
        if self.var_table.contains(sp) {
            let sp_ty = sp.int_type()?;
            let const_id = self.build_int_const(ret_stack_pop as u64, sp_ty)?;
            let adjusted =
                self.build_int_binary_operation(sp_pre_call, const_id, IntBinaryOp::Add, sp_ty)?;
            self.write_variable(sp, adjusted)?;
        }
        Ok(())
    }
}
