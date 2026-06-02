use anyhow::anyhow;
use smallvec::SmallVec;

use super::FunctionBuilder;
use crate::error::Result;
use crate::node::{NodeId, NodeKind, ValueId, ValueKind, ValueType};
use crate::ops::IntBinaryOp;

/// The per-Call ABI shape resolved by
/// [`FunctionBuilder::select_call_abi`]: `(arg_vars, ret_val_vars,
/// clobber_vars, ret_stack_pop, preserves_memory)` — either the
/// function-default snapshot or the override CC's filtered view.
type CallAbiSelection = (
    SmallVec<[rsleigh::Vn; 4]>,
    SmallVec<[rsleigh::Vn; 4]>,
    SmallVec<[rsleigh::Vn; 4]>,
    i64,
    bool,
);

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
        ret_val_vns: &[rsleigh::Vn],
        clobber_vns: &[rsleigh::Vn],
        advance_memory: bool,
        terminate: bool,
    ) -> Result<(NodeId, Vec<ValueId>, Vec<ValueId>)> {
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

        // Outputs: [Control, Memory] ++ ret-val slots ++ clobber slots.
        // Each ret-val / clobber slot's kind is a pure function of the
        // varnode's byte width — a tracked register always holds an int
        // value of its width, so the kind is `Typed(int_for_byte_size)`.
        let mut output_kinds: SmallVec<[ValueKind; 8]> = SmallVec::new();
        output_kinds.push(ValueKind::Control);
        output_kinds.push(ValueKind::Memory);
        for vn in ret_val_vns.iter().chain(clobber_vns) {
            output_kinds.push(ValueKind::Typed(ValueType::int_for_byte_size(vn.size)?));
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
            self.mark_cur_region_terminated()?;
        } else {
            self.advance_cur_region_ctrl(outputs[0])?;
        }
        if advance_memory {
            self.advance_cur_region_memory(outputs[1])?;
        }

        let ret_val_start = 2usize;
        let clobber_start = ret_val_start + ret_val_vns.len();
        let ret_val_values: Vec<ValueId> = outputs[ret_val_start..clobber_start].to_vec();
        let clobber_values: Vec<ValueId> = outputs[clobber_start..].to_vec();

        // Tag each ret-val output value with the register it returns via
        // `value_vn` so pattern queries can recover the ret-val varnode.
        for (value, vn) in core::iter::zip(&ret_val_values, ret_val_vns) {
            self.function_mut().set_clobbered_vn(*value, *vn);
        }
        // Tag each clobber output value with the register it clobbers
        // (via `value_vn`) so pattern queries can recover the clobber
        // varnode for each slot.
        for (value, vn) in core::iter::zip(&clobber_values, clobber_vns) {
            self.function_mut().set_clobbered_vn(*value, *vn);
        }

        Ok((node, ret_val_values, clobber_values))
    }

    /// Emits a `Call` node into the current region.
    ///
    /// When `override_cc` is `None`, the Call is built with the
    /// function-default arg-passing / clobber / ret-stack-pop set
    /// from `FunctionBuilder::new`.  When `override_cc` is `Some(cc)`,
    /// `cc` fully replaces the function-default for this single Call:
    /// `cc.arg_passing_regs` (filtered through the function's tracked-
    /// variable set) become the args; `cc.callee_saved_regs` define a
    /// fresh `is_clobbered = !callee_saved.contains(v) && Some(*v) !=
    /// stack_ptr` filter that produces this Call's clobber list;
    /// `cc.ret_stack_pop` drives the post-call SP-add.  Each clobber
    /// output value is tagged with the register it clobbers on
    /// `Function::value_vn` so pattern queries can recover the right
    /// varnode for each clobber slot.
    ///
    /// Does **not** terminate the region — the Call sits inline in the
    /// region's control/memory chain.  Like [`Self::build_call_other`],
    /// the node itself is emitted by the shared [`Self::build_call_kind`]
    /// low-level builder.
    ///
    /// Returns the freshly-created Call's [`NodeId`].
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion`
    /// when there is no active region to advance, `ExpectedValue`
    /// when `call_address` or any read clobbered/arg-passing variable is not
    /// a value edge, `VariableNotFound` when an arg-passing or
    /// clobbered varnode is not tracked, and `UnsupportedOutputSize`
    /// when the stack-pointer varnode's byte size has no matching
    /// [`ValueType`] (only applicable on stack-push ISAs).
    pub fn build_call(
        &mut self,
        call_address: ValueId,
        override_cc: Option<&strider_target::BuiltCallingConvention>,
    ) -> Result<NodeId> {
        // Resolve the per-call ABI shape (arg list, ret-val list, clobber
        // list, ret_stack_pop, preserves_memory) from either the override CC
        // or the function-default snapshot stamped at builder construction.
        let (arg_vars, ret_val_vars, clobber_vars, ret_stack_pop, preserves_memory) =
            self.select_call_abi(override_cc);

        // Read each arg variable (the args are the Call's real value
        // inputs) and verify the call_address is a value edge.  The
        // ret-val / clobber vars are NOT read here — they are outputs, and
        // their slot kinds are derived from each vn's byte width inside
        // `build_call_kind`.
        let arg_passing: SmallVec<[ValueId; 4]> = arg_vars
            .iter()
            .map(|var| self.read_variable(var))
            .collect::<Result<_>>()?;
        self.validate_value_inputs(&arg_passing)?;
        let addr_kind = self.function().value_kind(call_address);
        if !addr_kind.is_value() {
            return Err(anyhow!(
                "output {call_address:?} is not a value edge (got {addr_kind:?})"
            ));
        }

        // Read the stack pointer ONCE: it is both the Call's SP input
        // anchor (always wired, ahead of the args) and — on stack-push
        // ISAs (`ret_stack_pop != 0`) — the base for the post-call SP
        // adjust.  Reading it here, before `build_call_kind`, lets a
        // single SP value feed both uses instead of reading SP twice.
        let (sp_vn, sp_value) = self.read_or_init_stack_vn()?;

        // Emit the Call node via the shared emitter.  The Call's value
        // inputs after `call_address` are the SP anchor, then exactly its
        // args — the ret-val and clobbered vars are NOT inputs (they were
        // read only to recover their output-slot kinds).  Control always
        // advances; memory advances unless the CC preserves it (so
        // subsequent loads see the pre-call memory edge — the Memory
        // output is still present but left dangling).
        let (call, ret_val_values, clobber_values) = self.build_call_kind(
            NodeKind::Call,
            Some(call_address),
            Some(sp_value),
            &arg_passing,
            &ret_val_vars,
            &clobber_vars,
            !preserves_memory,
            false,
        )?;

        // Post-call write-back: rebind each ret-val variable to its fresh
        // ret-val output, then each clobbered variable to its clobber
        // output.  The `value_vn` tags are applied by `build_call_kind`.
        for (variable, new_val) in core::iter::zip(&ret_val_vars, &ret_val_values) {
            self.write_variable(variable, *new_val)?;
        }
        for (variable, new_val) in core::iter::zip(&clobber_vars, &clobber_values) {
            self.write_variable(variable, *new_val)?;
        }

        // Record the override CC on the Call (subsuming its stack-arg
        // offsets) so per-address-CC consumers — the stack-arg collector
        // and pattern queries — can recover it.
        if let Some(cc) = override_cc {
            self.function_mut()
                .set_call_descriptor(call, crate::CallDescriptor::Call(cc.clone()));
        }

        // Apply the post-call SP adjust on stack-push ISAs, reusing the
        // single SP value read above.  On link-register ISAs
        // (`ret_stack_pop == 0`) this is a no-op.
        self.apply_post_call_sp_adjust(&sp_vn, sp_value, ret_stack_pop)?;

        Ok(call)
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
    /// - Writes each implicit-write clobber output back to its register
    ///   via [`Self::write_reg_vn`].
    /// - Records `CallDescriptor::CallOther(abi.clone())` on the node and
    ///   stamps `name` on `Graph::call_other_names`.
    ///
    /// When `terminate` is `true` (the `NoReturn` class), the region is
    /// closed as part of this call — no separate
    /// [`Self::mark_cur_region_terminated`] call is needed.
    /// When `terminate` is `false` (the modeled `Call(abi)` class),
    /// the region's control advances to the CallOther's Control output
    /// and the region stays open.
    ///
    /// Inputs of the resulting node:
    /// `[ctrl, mem] ++ target? ++ explicit_args ++ implicit_reads`
    /// (CallOther carries no SP anchor — it has no CC stack args).
    /// Outputs: `[Control, Memory] ++ ret_val? ++ clobbers`.
    ///
    /// The result writeback to `output` stays with the caller: `output`
    /// can name any space (register / unique / RAM) and only the lifter's
    /// full `write_vn` handles all of them.  This method therefore returns
    /// the result value (the `Some` ret-val output, if any) and leaves the
    /// `write_vn(output_vn, result)` to the lifter.
    ///
    /// # Errors
    ///
    /// Returns an error when any `explicit_args` entry is not a value
    /// edge, when an `abi.implicit_reads` / `abi.implicit_writes` register
    /// cannot be read/written (no tracked container, unsupported width),
    /// when `output` is `Some` but its varnode byte size has no matching
    /// [`ValueType`], or when the region cannot be advanced or terminated.
    #[allow(clippy::too_many_arguments)]
    pub fn build_call_other(
        &mut self,
        user_op_id: u64,
        name: &str,
        target: Option<ValueId>,
        explicit_args: &[ValueId],
        abi: &strider_target::BuiltCallOtherAbi,
        output: Option<rsleigh::Vn>,
        terminate: bool,
    ) -> Result<(NodeId, Option<ValueId>)> {
        // Read each implicit-read register and append after the explicit
        // pcode operands, preserving the layout
        // `[ctrl, mem] ++ explicit_args ++ implicit_reads`.
        let mut arg_values: SmallVec<[ValueId; 4]> = explicit_args.iter().copied().collect();
        for vn in &abi.implicit_reads {
            let value = self.read_reg_vn(vn)?;
            arg_values.push(value);
        }

        // Derive the ret-val group from the output varnode (if any): a
        // 0-or-1-element vn list.  Both the ret-val and clobber slot kinds
        // are derived from each vn's byte width inside `build_call_kind`.
        let ret_val_vns: SmallVec<[rsleigh::Vn; 1]> = output.into_iter().collect();
        let clobber_vns: &[rsleigh::Vn] = &abi.implicit_writes;

        let (node, ret_val_values, clobber_values) = self.build_call_kind(
            NodeKind::CallOther { user_op_id },
            target,
            None,
            &arg_values,
            &ret_val_vns,
            clobber_vns,
            abi.clobbers_memory,
            terminate,
        )?;

        // Write each implicit-write clobber output back to its register.
        // Implicit writes are registers, so write_reg_vn is the right
        // aliasing-aware path.
        for (vn, value) in core::iter::zip(clobber_vns, &clobber_values) {
            self.write_reg_vn(vn, *value)?;
        }

        // Record the vn-resolved footprint + the user-op name on the node.
        self.function_mut()
            .set_call_descriptor(node, crate::CallDescriptor::CallOther(abi.clone()));
        self.function_mut().set_call_other_name(node, name.to_string());

        Ok((node, ret_val_values.into_iter().next()))
    }

    /// `select_call_abi` helper for [`Self::build_call`]:
    /// resolve the per-call ABI shape for a single Call.  The effective CC
    /// is the override when present, else the function default — and every
    /// derived list is computed from that one CC via the same `_for(cc)`
    /// accessors, so there is a single source of truth for ret-vals,
    /// clobbers, `ret_stack_pop`, and `preserves_memory`.
    ///
    /// The arg list is the lone branch: the function default maps each arg
    /// register to its tracked container (via `arg_passing_vars`'s
    /// `upgrade_vn`), while an override keeps only arg registers that are
    /// themselves tracked (so reads against untracked vars don't fail with
    /// `VariableNotFound`).
    fn select_call_abi(
        &self,
        override_cc: Option<&strider_target::BuiltCallingConvention>,
    ) -> CallAbiSelection {
        let cc = override_cc.unwrap_or_else(|| self.function.default_cc());
        let arg_vars: SmallVec<[rsleigh::Vn; 4]> = match override_cc {
            None => self.function.arg_passing_vars().into_iter().collect(),
            Some(cc) => cc
                .arg_passing_regs
                .iter()
                .copied()
                .filter(|v| self.var_table.contains(v))
                .collect(),
        };
        (
            arg_vars,
            self.function.call_ret_vals_for(cc).into_iter().collect(),
            self.function.call_clobbered_for(cc).into_iter().collect(),
            cc.ret_stack_pop,
            cc.preserves_memory,
        )
    }

    /// `read_or_init_stack_vn` helper: produce the current stack-pointer
    /// value at the call site.  This single value feeds both the Call's
    /// SP input anchor and (on stack-push ISAs) the post-call SP adjust.
    ///
    /// When the stack-pointer varnode is a tracked variable, its current
    /// SSA value is returned.  When it is NOT tracked but is a real
    /// (sized) register, a fresh `InitialVar(stack_vn)` is minted.  When
    /// it is the trivial-CC sentinel (a zero-size CONST varnode, used by
    /// synthetic fixtures with no real stack pointer), a default
    /// `IntConst(0):I64` anchor is minted so the Call still carries a
    /// well-typed SP input.  Returns `(stack_vn, sp_value)`.
    fn read_or_init_stack_vn(&mut self) -> Result<(rsleigh::Vn, ValueId)> {
        let sp = self.function.stack_vn();
        if let Some(value) = self.read_variable_optional(&sp)? {
            return Ok((sp, value));
        }
        // Untracked SP.  A real register has a supported byte size — mint
        // a fresh `InitialVar(sp)` of the matching width.  The trivial-CC
        // sentinel (size 0) has no width: anchor it with a default
        // `IntConst(0):I64` so the Call's SP input slot stays well-typed.
        let value = match ValueType::int_for_byte_size(sp.size) {
            Ok(sp_ty) => self.build_single_output_pure(NodeKind::InitialVar(sp), [], sp_ty),
            Err(_) => self.build_int_const(0u64, ValueType::I64)?,
        };
        Ok((sp, value))
    }

    /// `apply_post_call_sp_adjust` helper: model the caller-visible
    /// effect of the callee's `ret` on SP — on stack-push ISAs `ret`
    /// pops the return-address word, so the caller's post-call SP is
    /// `pre_call_SP + ret_stack_pop`.  On link-register ISAs
    /// `ret_stack_pop == 0`, so this is a no-op.  `sp_pre_call` is the
    /// single SP value read by [`Self::read_or_init_stack_vn`].
    fn apply_post_call_sp_adjust(
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
            let sp_ty = ValueType::int_for_byte_size(sp.size)?;
            let const_id = self.build_int_const(ret_stack_pop as u64, sp_ty)?;
            let adjusted =
                self.build_int_binary_operation(sp_pre_call, const_id, IntBinaryOp::Add, sp_ty)?;
            self.write_variable(sp, adjusted)?;
        }
        Ok(())
    }
}
