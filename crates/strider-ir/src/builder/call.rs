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
            self.terminate_cur_region().map(|_| ())?;
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
            self.function_mut().side_tables.value_vn.insert(*value, *vn);
        }
        // Tag each clobber output value with the register it clobbers
        // (via `value_vn`) so pattern queries can recover the clobber
        // varnode for each slot.
        for (value, vn) in core::iter::zip(&clobber_values, clobber_vns) {
            self.function_mut().side_tables.value_vn.insert(*value, *vn);
        }

        Ok((node, ret_val_values, clobber_values))
    }

    /// Emits a `Call` node into the current region.
    ///
    /// When `override_cc` is `None`, the Call is built with the
    /// function-default arg-passing / clobber / ret-stack-pop set from
    /// `FunctionBuilder::new`; the ret-val + clobber lists are derived on
    /// demand from the function-default CC via [`crate::Function::call_ret_vals_for`]
    /// / [`crate::Function::call_clobbered_for`].  When
    /// `override_cc` is `Some(cc)`, `cc` fully replaces the function-default
    /// for this single Call: the RAW `cc.arg_passing_regs` are read at the
    /// call site via the aliasing-aware [`Self::read_reg_vn`] (which resolves
    /// each declared register to its tracked container and errors when none
    /// exists); `cc.callee_saved_regs` define a fresh `is_clobbered =
    /// !callee_saved.contains(v) && v != stack_ptr` filter that produces this
    /// Call's clobber list; `cc.ret_stack_pop` drives the post-call SP-add.
    /// Each ret-val / clobber output value is tagged with the register it
    /// represents on `Function::value_vn` so pattern queries can recover the
    /// right varnode for each slot.
    ///
    /// The stack pointer is read through the variable table at the call
    /// site; a Call requires a pre-seeded SP and **errors** when it is not
    /// tracked (no SP anchor is minted).  Write-back order is clobbers (via
    /// [`Self::write_variable`]) then ret-vals (via [`Self::write_reg_vn`]),
    /// so an aliased clobber cannot re-clobber the return value.
    ///
    /// Does **not** terminate the region — the Call sits inline in the
    /// region's control/memory chain.  Like [`Self::build_call_other`],
    /// the node itself is emitted by the shared `build_call_kind`
    /// low-level builder.
    ///
    /// Returns the freshly-created Call's [`NodeId`].
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion` when there is no active region to advance,
    /// `ExpectedValue` when `call_address` is not a value edge, an error when
    /// any arg-passing / ret-val varnode is not REGISTER / UNIQUE or has no
    /// enclosing tracked container, an error when the stack pointer is not
    /// tracked, and `UnsupportedOutputSize` when the stack-pointer varnode's
    /// byte size has no matching [`ValueType`] (only applicable on stack-push
    /// ISAs).
    pub fn build_call(
        &mut self,
        call_address: ValueId,
        override_cc: Option<&strider_target::BuiltCallingConvention>,
    ) -> Result<NodeId> {
        // The effective CC is the override when present, else the
        // function-default snapshot stamped at builder construction.  Every
        // derived list — ret-vals, clobbers, args, `ret_stack_pop`,
        // `preserves_memory` — comes from this one CC, so there is a single
        // source of truth.
        let cc = override_cc.unwrap_or_else(|| self.function.default_cc());
        let ret_stack_pop = cc.ret_stack_pop;
        let preserves_memory = cc.preserves_memory;

        // Ret-val + clobber vns, derived from the effective CC over the
        // function's tracked varnodes.  The hashed-membership derivations
        // are O(V) in the tracked-varnode count (bounded by the register
        // file), run once per Call site — Calls are sparse relative to the
        // node count, so this stays linear in graph size overall.
        let ret_val_vars: SmallVec<[rsleigh::Vn; 4]> =
            self.function.call_ret_vals_for(cc).into_iter().collect();
        let clobber_vars: SmallVec<[rsleigh::Vn; 4]> =
            self.function.call_clobbered_for(cc).into_iter().collect();

        // Ret-val vns must be REGISTER / UNIQUE: they are written back via
        // the aliasing-aware `write_reg_vn`, so a RAM / CONST ret-val has
        // no container to slice.  Gate them explicitly before emitting.
        for vn in &ret_val_vars {
            require_reg_or_unique(vn)?;
        }

        // Args: the RAW `cc.arg_passing_regs`, read through the aliasing-
        // aware `read_reg_vn`.  No `upgrade_vn` pre-mapping — `read_reg_vn`
        // resolves each declared register to its tracked container (slicing
        // a sub-register down to the arg's width) and errors when a CC arg
        // register has no tracked footprint (the intended "CC reg must
        // exist" invariant).  Snapshot the vns first so the read loop is
        // free to borrow `self` mutably.  Each arg is also gated through
        // `require_reg_or_unique` so a malformed CC arg surfaces cleanly.
        let arg_vns: SmallVec<[rsleigh::Vn; 4]> = cc.arg_passing_regs.iter().copied().collect();
        let mut arg_passing: SmallVec<[ValueId; 4]> = SmallVec::new();
        for vn in &arg_vns {
            require_reg_or_unique(vn)?;
            arg_passing.push(self.read_reg_vn(vn)?);
        }
        self.validate_value_inputs(&arg_passing)?;
        self.require_value_kind(call_address)?;

        // Read the stack pointer ONCE: it is both the Call's SP input
        // anchor (always wired, ahead of the args) and — on stack-push
        // ISAs (`ret_stack_pop != 0`) — the base for the post-call SP
        // adjust.  A Call requires a real, pre-seeded SP: every CC register
        // (SP included) already exists as a tracked `InitialVar` from
        // `build_entry`, so a missing SP is a genuine error, never minted.
        let sp_vn = self.function.stack_vn();
        let sp_value = self.read_variable_optional(&sp_vn)?.ok_or_else(|| {
            anyhow!(
                "build_call: stack-pointer varnode {sp_vn:?} is not tracked; \
                 a Call requires a pre-seeded SP (CC registers are tracked at \
                 build_entry — no SP anchor is minted at the call site)"
            )
        })?;

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

        // Post-call write-back, in this ORDER: first the clobbers, then the
        // ret-vals.  The clobber set legitimately includes CONST / RAM
        // temporaries Sleigh leaves in the touched-varnode set, so clobbers
        // go through `write_variable`.  Ret-vals are REGISTER / UNIQUE (gated
        // above), so they go through the aliasing-aware `write_reg_vn`.
        // Writing the ret-vals AFTER the clobbers guarantees a clobber that
        // aliases a ret-val register cannot re-clobber the return value.  The
        // `value_vn` tags are applied by `build_call_kind`.
        for (vn, new_val) in core::iter::zip(&clobber_vars, &clobber_values) {
            self.write_variable(vn, *new_val)?;
        }
        for (vn, new_val) in core::iter::zip(&ret_val_vars, &ret_val_values) {
            self.write_reg_vn(vn, *new_val)?;
        }

        // Record the override CC on the Call (subsuming its stack-arg
        // offsets) so per-address-CC consumers — the stack-arg collector
        // and pattern queries — can recover it.
        if let Some(cc) = override_cc {
            self.function_mut()
                .side_tables.call_descriptor.insert(call, crate::CallDescriptor::Call(cc.clone()));
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
    /// - Writes the implicit-write clobbers back first (via
    ///   [`Self::write_variable`], matching the `Call` clobber path — a
    ///   full-register write is identical and this unifies clobber
    ///   handling), THEN the result to `output` via [`Self::write_reg_vn`]
    ///   AFTER the clobbers, so an aliased clobber cannot re-clobber the
    ///   result.  The builder now owns the result writeback (it used to be
    ///   the lifter's job).
    /// - Records `CallDescriptor::CallOther(abi.clone())` on the node and
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
        explicit_args: &[ValueId],
        abi: &strider_target::BuiltCallOtherAbi,
        output: Option<rsleigh::Vn>,
        terminate: bool,
    ) -> Result<(NodeId, Option<ValueId>)> {
        // Gate the implicit reads / writes and the output through the
        // reg/unique invariant before doing anything: every footprint
        // register (and the result destination) must be REGISTER / UNIQUE,
        // because they all flow through the aliasing-aware read/write path.
        for vn in &abi.implicit_reads {
            require_reg_or_unique(vn)?;
        }
        for vn in &abi.implicit_writes {
            require_reg_or_unique(vn)?;
        }
        if let Some(out_vn) = output.as_ref() {
            require_reg_or_unique(out_vn)?;
        }

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

        // Writeback ORDER (matching `build_call`): the implicit-write
        // clobbers first via `write_variable` (a full-register write is
        // identical to `write_reg_vn` here, and this unifies clobber
        // handling with `Call`), THEN the result to `output` via
        // `write_reg_vn` AFTER the clobbers — so an aliased clobber cannot
        // re-clobber the result.
        for (vn, value) in core::iter::zip(clobber_vns, &clobber_values) {
            self.write_variable(vn, *value)?;
        }
        let result = ret_val_values.into_iter().next();
        if let (Some(out_vn), Some(value)) = (output.as_ref(), result) {
            self.write_reg_vn(out_vn, value)?;
        }

        // Record the vn-resolved footprint + the user-op name on the node.
        self.function_mut()
            .side_tables.call_descriptor.insert(node, crate::CallDescriptor::CallOther(abi.clone()));
        self.function_mut()
            .side_tables.call_other_names[node] = Some(name.to_string());

        Ok((node, result))
    }

    /// `apply_post_call_sp_adjust` helper: model the caller-visible
    /// effect of the callee's `ret` on SP — on stack-push ISAs `ret`
    /// pops the return-address word, so the caller's post-call SP is
    /// `pre_call_SP + ret_stack_pop`.  On link-register ISAs
    /// `ret_stack_pop == 0`, so this is a no-op.  `sp_pre_call` is the
    /// single pre-call SP value [`Self::build_call`] read from the variable
    /// table.
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
            let sp_ty = sp.int_type()?;
            let const_id = self.build_int_const(ret_stack_pop as u64, sp_ty)?;
            let adjusted =
                self.build_int_binary_operation(sp_pre_call, const_id, IntBinaryOp::Add, sp_ty)?;
            self.write_variable(sp, adjusted)?;
        }
        Ok(())
    }
}
