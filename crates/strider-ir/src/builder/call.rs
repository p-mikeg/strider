use anyhow::anyhow;
use smallvec::SmallVec;

use super::FunctionBuilder;
use crate::error::Result;
use crate::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};
use crate::ops::IntBinaryOp;

/// The per-Call ABI shape resolved by
/// [`FunctionBuilder::select_call_abi`] — either the function-default
/// snapshot or the override CC's filtered view.  Threaded through the
/// rest of [`FunctionBuilder::build_call_with_cc`]'s phases.
struct CallAbiSelection {
    arg_vars: SmallVec<[rsleigh::Vn; 4]>,
    clobber_vars: SmallVec<[rsleigh::Vn; 4]>,
    ret_stack_pop: i64,
    no_memory_clobber: bool,
}

/// The result of [`FunctionBuilder::read_call_value_inputs`]: arg
/// input ids (in CC order) plus clobber output kinds (one per
/// `clobber_vars`).  Feeds the `create_node` call in
/// [`FunctionBuilder::emit_call_node`].
struct CallValueInputs {
    arg_passing: SmallVec<[NodeOutputId; 4]>,
    clobbered_kinds: SmallVec<[NodeOutputKind; 4]>,
}

impl FunctionBuilder {
    /// Terminates the current region with a `Call` node, using the
    /// function-default calling convention.  Equivalent to
    /// [`Self::build_call_with_cc`] with `override_cc = None`.
    ///
    /// # Errors
    ///
    /// See [`Self::build_call_with_cc`].
    pub fn build_call(&mut self, call_address: NodeOutputId) -> Result<()> {
        self.build_call_with_cc(call_address, None).map(|_| ())
    }

    /// Terminates the current region with a `Call` node.
    ///
    /// When `override_cc` is `None`, the Call is built with the
    /// function-default arg-passing / clobber / ret-stack-pop set
    /// from `FunctionBuilder::new`.  When `override_cc` is `Some(cc)`,
    /// `cc` fully replaces the function-default for this single Call:
    /// `cc.arg_passing_regs` (filtered through the function's tracked-
    /// variable set) become the args; `cc.callee_saved_regs` define a
    /// fresh `is_clobbered = !callee_saved.contains(v) && Some(*v) !=
    /// stack_ptr` filter that produces this Call's clobber list;
    /// `cc.ret_stack_pop` drives the post-call SP-add.  The per-Call
    /// clobber list is recorded on
    /// `Graph::call_clobbered_overrides` so pattern queries
    /// can recover the right varnode for each clobber slot.
    ///
    /// Returns the freshly-created Call's [`NodeId`].
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion` / `RegionTerminated`
    /// when there is no active region to advance, `ExpectedValue`
    /// when `call_address` or any read clobbered/arg-passing variable is not
    /// a value edge, `VariableNotFound` when an arg-passing or
    /// clobbered varnode is not tracked, and `UnsupportedOutputSize`
    /// when the stack-pointer varnode's byte size has no matching
    /// [`NodeOutputType`] (only applicable on stack-push ISAs).
    pub fn build_call_with_cc(
        &mut self,
        call_address: NodeOutputId,
        override_cc: Option<&strider_target::BuiltCallingConvention>,
    ) -> Result<NodeId> {
        // Resolve the per-call ABI shape (arg list, clobber list,
        // ret_stack_pop) from either the override CC or the
        // function-default snapshot stamped at builder construction.
        let CallAbiSelection {
            arg_vars,
            clobber_vars,
            ret_stack_pop,
            no_memory_clobber,
        } = self.select_call_abi(override_cc);

        // Read every arg + clobber variable and verify the
        // call_address is a value edge.  This also produces the
        // arg-input id list + the clobber-kind list that feed the
        // `emit_call_node` create_node call below.
        let CallValueInputs {
            arg_passing,
            clobbered_kinds,
        } = self.read_call_value_inputs(call_address, &arg_vars, &clobber_vars)?;

        // Snapshot pre-call SP for the post-call adjust (only on
        // stack-push ISAs where `ret_stack_pop != 0`).
        let sp_pre_call = self.snapshot_pre_call_sp(ret_stack_pop)?;

        // Create the Call node, advance ctrl (+memory unless
        // no_memory_clobber), write per-clobber variables, and stamp
        // the per-call override on the side-table.
        let call = self.emit_call_node(
            call_address,
            arg_passing,
            clobbered_kinds,
            &clobber_vars,
            override_cc.is_some(),
            no_memory_clobber,
        )?;

        // Apply the post-call SP adjust on stack-push ISAs.
        self.apply_post_call_sp_adjust(sp_pre_call, ret_stack_pop)?;

        Ok(call)
    }

    /// `select_call_abi` helper for [`Self::build_call_with_cc`]:
    /// resolve the per-call ABI shape from the override CC or the
    /// function-default snapshot.  Override args are filtered through
    /// the function's tracked-variable set so reads against unread
    /// vars don't fail with `VariableNotFound`; override clobbers
    /// cover every tracked variable that is neither callee-saved nor
    /// the SP.
    fn select_call_abi(
        &self,
        override_cc: Option<&strider_target::BuiltCallingConvention>,
    ) -> CallAbiSelection {
        let cc_meta = &self.function.cc_metadata;
        let function_default_no_memory_clobber = self.function.no_memory_clobber();
        let function_default_ret_stack_pop = self.function.ret_stack_pop();
        let function_stack_vn = self.function.stack_vn();
        let no_memory_clobber =
            override_cc.map_or(function_default_no_memory_clobber, |cc| cc.no_memory_clobber);
        match override_cc {
            None => CallAbiSelection {
                arg_vars: cc_meta.arg_passing_vars.iter().copied().collect(),
                clobber_vars: cc_meta.call_clobbered.iter().copied().collect(),
                ret_stack_pop: function_default_ret_stack_pop,
                no_memory_clobber,
            },
            Some(cc) => {
                let arg_vars: SmallVec<[rsleigh::Vn; 4]> = cc
                    .arg_passing_regs
                    .iter()
                    .copied()
                    .filter(|v| cc_meta.variable_to_id.contains_key(v))
                    .collect();
                // SP is a function-stable register; an override only
                // sees it via the function-default's `stack_vn`.
                // When the FunctionBuilder was built without a CC
                // (the `new_raw` path), `stack_vn` is None — in
                // that case no variable can equal "the function's SP"
                // so the comparison degenerates to "not callee-saved",
                // which the helper short-circuits via a sentinel
                // unreachable Vn.
                let function_sp = function_stack_vn.unwrap_or(rsleigh::Vn {
                    addr_off: u64::MAX,
                    addr_space: rsleigh::VnSpace::CONST,
                    size: 0,
                });
                let clobber_vars: SmallVec<[rsleigh::Vn; 4]> = cc_meta
                    .variables
                    .values()
                    .copied()
                    .filter(|v| cc.clobbers_override_var(v, function_sp))
                    .collect();
                CallAbiSelection {
                    arg_vars,
                    clobber_vars,
                    ret_stack_pop: cc.ret_stack_pop,
                    no_memory_clobber,
                }
            }
        }
    }

    /// `read_call_value_inputs` helper: read every arg / clobber
    /// variable and assert the call address is a value edge.  Returns
    /// the arg-input id list (in CC order) plus the
    /// clobber-output-kind list (one entry per `clobber_vars` entry,
    /// in the same order).
    fn read_call_value_inputs(
        &mut self,
        call_address: NodeOutputId,
        arg_vars: &[rsleigh::Vn],
        clobber_vars: &[rsleigh::Vn],
    ) -> Result<CallValueInputs> {
        let arg_passing: SmallVec<[NodeOutputId; 4]> = arg_vars
            .iter()
            .map(|var| self.read_variable(var))
            .collect::<Result<_>>()?;
        self.validate_value_inputs(&arg_passing)?;

        let mut clobbered_kinds: SmallVec<[NodeOutputKind; 4]> = SmallVec::new();
        for var in clobber_vars {
            let out = self.read_variable(var)?;
            let k = self.function().output_kind(out);
            if !k.is_value() {
                return Err(anyhow!("output {out:?} is not a value edge (got {k:?})"));
            }
            clobbered_kinds.push(k);
        }

        let addr_kind = self.function().output_kind(call_address);
        if !addr_kind.is_value() {
            return Err(anyhow!(
                "output {call_address:?} is not a value edge (got {addr_kind:?})"
            ));
        }

        Ok(CallValueInputs {
            arg_passing,
            clobbered_kinds,
        })
    }

    /// `snapshot_pre_call_sp` helper: snapshot the pre-call SP value
    /// so the post-call SP adjust (`apply_post_call_sp_adjust`) can
    /// wire `pre + ret_stack_pop` through `IntBinaryOp::Add`.
    /// Returns `None` on link-register ISAs (`ret_stack_pop == 0`)
    /// or when the function doesn't track the SP.
    fn snapshot_pre_call_sp(
        &mut self,
        ret_stack_pop: i64,
    ) -> Result<Option<(rsleigh::Vn, NodeOutputId)>> {
        match self.function.stack_vn() {
            Some(sp) if ret_stack_pop != 0 => {
                Ok(self.read_variable_optional(&sp)?.map(|out| (sp, out)))
            }
            _ => Ok(None),
        }
    }

    /// `emit_call_node` helper: create the Call node, advance the
    /// region's control (+memory unless `no_memory_clobber`) edges,
    /// write each clobber variable, and stamp the per-call override
    /// clobber list on the graph side-table when this Call carries
    /// one.
    fn emit_call_node(
        &mut self,
        call_address: NodeOutputId,
        arg_passing: SmallVec<[NodeOutputId; 4]>,
        clobbered_kinds: SmallVec<[NodeOutputKind; 4]>,
        clobber_vars: &[rsleigh::Vn],
        is_override: bool,
        no_memory_clobber: bool,
    ) -> Result<NodeId> {
        let ctrl = self.cur_region_control()?;
        let memory = self.cur_region_memory()?;

        let inputs = [ctrl, memory, call_address].into_iter().chain(arg_passing);
        // The Call node's signature always includes a Memory output
        // (validator local-typing enforces `[Control, Memory,
        // *clobbers]`).  When the CC declares `no_memory_clobber`, we
        // keep the Memory output but leave it dangling — the region's
        // memory chain is NOT advanced, so subsequent loads see the
        // pre-call memory edge.  LoadReadOnly / LoadForward can
        // therefore forward through the call.
        let outputs = [NodeOutputKind::Control, NodeOutputKind::Memory]
            .into_iter()
            .chain(clobbered_kinds);
        let call = self.create_node(NodeKind::Call, inputs, outputs);
        let call_outputs: Vec<_> = self.function().node_outputs(call).to_vec();

        self.advance_cur_region_ctrl(call_outputs[0])?;
        if !no_memory_clobber {
            self.advance_cur_region_memory(call_outputs[1])?;
        }
        for (variable, new_val) in core::iter::zip(clobber_vars, call_outputs.iter().skip(2)) {
            self.write_variable(variable, *new_val)?;
        }

        // Record the per-Call override clobber list when an override
        // was used.
        if is_override {
            let list: Vec<rsleigh::Vn> = clobber_vars.to_vec();
            self.function_mut().set_call_clobbered_override(call, list);
        }
        Ok(call)
    }

    /// `apply_post_call_sp_adjust` helper: model the caller-visible
    /// effect of the callee's `ret` on SP — on stack-push ISAs `ret`
    /// pops the return-address word, so the caller's post-call SP is
    /// `pre_call_SP + ret_stack_pop`.  On link-register ISAs
    /// `ret_stack_pop == 0` and the `snapshot_pre_call_sp` snapshot
    /// is `None`, so this is a no-op.
    fn apply_post_call_sp_adjust(
        &mut self,
        sp_pre_call: Option<(rsleigh::Vn, NodeOutputId)>,
        ret_stack_pop: i64,
    ) -> Result<()> {
        if let Some((sp, pre)) = sp_pre_call {
            let sp_ty = NodeOutputType::int_for_byte_size(sp.size)?;
            let const_id = self.build_int_const(ret_stack_pop as u64, sp_ty)?;
            let adjusted =
                self.build_int_binary_operation(pre, const_id, IntBinaryOp::Add, sp_ty)?;
            self.write_variable(&sp, adjusted)?;
        }
        Ok(())
    }

    /// Emit a CallOther node intended as a region terminator (Linux
    /// `BUG_ON`-class trap).  Has only ctrl + memory inputs and
    /// ctrl + memory outputs — no clobbers, no value, no args.  The
    /// outputs are expected to dangle: the cfg has already terminated
    /// the region with `RegionTerminator::NoReturn`, so no successor
    /// will read them.  Stamps `name` on `Graph::call_other_names`.
    ///
    /// # Dispatch via `strider_target::CallOtherClass`
    ///
    /// This is the IR builder for the `NoReturn` arm of
    /// [`strider_target::call_other_abi::classify`] — the strider lift driver
    /// chooses between this and [`Self::build_call_other_modeled`]
    /// based on the `CallOtherClass` returned for the user-op name.
    ///
    /// There is intentionally **no** `build_call_other_noop` sibling:
    /// the `NoOp` arm of `CallOtherClass` skips IR emission entirely
    /// (the lifter discards the pcode op without producing any node).
    /// Only `NoReturn` (this function) and `Call(abi)`
    /// ([`Self::build_call_other_modeled`]) emit a `CallOther` node.
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion` when no region is active.
    pub fn build_call_other_terminal(
        &mut self,
        user_op_id: u64,
        name: &str,
    ) -> Result<NodeId> {
        // Snapshot the region's ctrl/mem edges and mark the region
        // terminated, mirroring `build_return` / `build_branch` /
        // `build_indirect_branch`.  Subsequent `build_*` calls into this
        // region will now correctly fail with `RegionTerminated` instead
        // of silently producing IR after a NoReturn terminator.
        let res = self.terminate_cur_region()?;
        self.require_terminator_kinds(&res)?;
        let mut output_kinds: SmallVec<[NodeOutputKind; 4]> = SmallVec::new();
        output_kinds.push(NodeOutputKind::Control);
        output_kinds.push(NodeOutputKind::Memory);
        let inputs = [res.control, res.memory];
        let node = self.create_node(
            NodeKind::CallOther { user_op_id },
            inputs,
            output_kinds,
        );
        self.function_mut().set_call_other_name(node, name.to_string());
        // Outputs intentionally dangle — no link_region.  The cfg layer
        // already terminates the region with `RegionTerminator::NoReturn`.
        Ok(node)
    }

    /// Emit a CallOther with the precise per-op ABI shape.
    ///
    /// # Dispatch via `strider_target::CallOtherClass`
    ///
    /// This is the IR builder for the `Call(abi)` arm of
    /// [`strider_target::call_other_abi::classify`] — the strider lift driver
    /// chooses between this and [`Self::build_call_other_terminal`]
    /// based on the `CallOtherClass` returned for the user-op name.
    /// `NoOp` skips IR emission entirely (no sibling builder), so this
    /// pair (`_terminal` for `NoReturn`, `_modeled` for `Call(abi)`)
    /// covers every IR-emitting case.  See `strider_target::CallOtherAbi` for
    /// the implicit-channel description that drives this builder's
    /// `implicit_reads` / `implicit_writes_vns` / `implicit_write_kinds`
    /// inputs.
    ///
    /// Inputs of the resulting node:
    ///   `[ctrl_in, mem_in, *args, *implicit_reads]`
    ///
    /// Outputs of the resulting node:
    ///   `[ctrl_out, mem_out, value?, *clobber_per_implicit_write]`
    ///
    /// This method advances the region's control token to the new
    /// `ctrl_out` but **does not** advance the memory token — the
    /// strider layer is responsible for calling
    /// `advance_cur_region_memory(mem_out)` IFF the ABI's
    /// `mem_clobbers` set is non-empty.  Similarly the strider layer is
    /// responsible for rebinding each implicit-write Vn to its
    /// corresponding clobber slot via the aliasing-aware
    /// `strider_lift::pcode_lift::ValueLifter::write_vn`.
    ///
    /// Both `implicit_reads` and `implicit_write_kinds` are slices of
    /// pre-resolved values: the strider caller does the
    /// aliasing-aware `read_vn` for reads (so EAX → RAX-extract works)
    /// and resolves the slot kind for each write (typically by looking
    /// at the Vn's size).  `implicit_writes_vns` is recorded in the
    /// per-CallOther clobber override side-table so
    /// `pattern::Match::get_vn` can recover the original Vn names.
    ///
    /// Stamps `name` on `Graph::call_other_names`.
    ///
    /// Returns `(node, value_output, clobber_outputs)`.
    /// `value_output.is_some() == output_ty.is_some()`.
    /// `clobber_outputs.len() == implicit_write_kinds.len() == implicit_writes_vns.len()`.
    ///
    /// # Errors
    ///
    /// Returns an error when any `args` or `implicit_reads` entry is
    /// not a value edge, when `implicit_write_kinds` and
    /// `implicit_writes_vns` differ in length, when any
    /// implicit-write kind is not a value kind, or when the resulting
    /// node fails to advance the active region's control token.
    // Eight-arg signature is the natural shape: id + name + the four
    // pcode-explicit channels (args, output_ty) + the three implicit
    // ABI channels (reads, writes_vns, write_kinds).  Splitting into a
    // builder type would add boilerplate without simplifying the call
    // site.
    #[allow(clippy::too_many_arguments)]
    pub fn build_call_other_modeled(
        &mut self,
        user_op_id: u64,
        name: &str,
        args: &[NodeOutputId],
        output_ty: Option<NodeOutputType>,
        implicit_reads: &[NodeOutputId],
        implicit_writes_vns: &[rsleigh::Vn],
        implicit_write_kinds: &[NodeOutputKind],
    ) -> Result<(NodeId, Option<NodeOutputId>, Vec<NodeOutputId>)> {
        if implicit_writes_vns.len() != implicit_write_kinds.len() {
            return Err(anyhow!(
                "build_call_other_modeled({name:?}): implicit_writes_vns.len() = {} \
                 but implicit_write_kinds.len() = {}",
                implicit_writes_vns.len(),
                implicit_write_kinds.len()
            ));
        }
        let ctrl = self.cur_region_control()?;
        let memory = self.cur_region_memory()?;

        self.validate_value_inputs(args)?;
        self.validate_value_inputs(implicit_reads)?;

        for (i, k) in implicit_write_kinds.iter().enumerate() {
            if !k.is_value() {
                return Err(anyhow!(
                    "implicit_write_kinds[{i}] for user-op {name:?} is not a value kind: {k:?}"
                ));
            }
        }

        let mut output_kinds: SmallVec<[NodeOutputKind; 8]> = SmallVec::new();
        output_kinds.push(NodeOutputKind::Control);
        output_kinds.push(NodeOutputKind::Memory);
        if let Some(ty) = output_ty {
            output_kinds.push(NodeOutputKind::OutputType(ty));
        }
        output_kinds.extend(implicit_write_kinds.iter().copied());

        let inputs = [ctrl, memory]
            .into_iter()
            .chain(args.iter().copied())
            .chain(implicit_reads.iter().copied());

        let node = self.create_node(
            NodeKind::CallOther { user_op_id },
            inputs,
            output_kinds,
        );
        let outputs: SmallVec<[NodeOutputId; 8]> =
            self.function().node_outputs(node).iter().copied().collect();

        // Advance ctrl only.  Memory is the strider layer's call.
        self.advance_cur_region_ctrl(outputs[0])?;

        let (value_output, clobber_start_slot) = if output_ty.is_some() {
            (Some(outputs[2]), 3usize)
        } else {
            (None, 2usize)
        };

        let clobber_outputs: Vec<NodeOutputId> = outputs[clobber_start_slot..].to_vec();

        // Stamp the user-op name + per-CallOther clobber override.
        let writes_vec: Vec<rsleigh::Vn> = implicit_writes_vns.to_vec();
        let function = self.function_mut();
        function.set_call_other_name(node, name.to_string());
        if !writes_vec.is_empty() {
            function.set_call_clobbered_override(node, writes_vec);
        }

        Ok((node, value_output, clobber_outputs))
    }

}
