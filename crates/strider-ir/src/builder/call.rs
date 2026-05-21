use anyhow::anyhow;
use smallvec::SmallVec;

use super::FunctionBuilder;
use crate::error::Result;
use crate::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};
use crate::ops::IntBinaryOp;

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
    /// `cc.arg_passing_regs()` (filtered through the function's tracked-
    /// variable set) become the args; `cc.callee_saved_regs()` define a
    /// fresh `is_clobbered = !callee_saved.contains(v) && Some(*v) !=
    /// stack_ptr` filter that produces this Call's clobber list;
    /// `cc.ret_stack_pop()` drives the post-call SP-add.  The per-Call
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
        override_cc: Option<&crate::FunctionBuilderCC>,
    ) -> Result<NodeId> {
        let ctrl = self.cur_region_control()?;
        let memory = self.cur_region_memory()?;

        // Pick the per-call arg-passing list, clobber list, and
        // ret_stack_pop based on whether an override was supplied.
        let (arg_vars, clobber_vars, ret_stack_pop): (
            SmallVec<[rsleigh::Vn; 4]>,
            SmallVec<[rsleigh::Vn; 4]>,
            i64,
        ) = match override_cc {
            None => (
                self.arg_passing_vars.iter().copied().collect(),
                self.call_clobbered_variables.iter().copied().collect(),
                self.ret_stack_pop,
            ),
            Some(cc) => {
                // Filter override args through the function's tracked
                // variables.  Override args that the function never
                // reads are silently dropped — they would otherwise
                // produce a `VariableNotFound` error from `read_variable`.
                let arg_vars: SmallVec<[rsleigh::Vn; 4]> = cc
                    .arg_passing_regs
                    .iter()
                    .copied()
                    .filter(|v| self.variable_to_id.contains_key(v))
                    .collect();
                // Per-call clobber list: every tracked variable that
                // is NOT in `callee_saved_regs` and NOT the SP.
                let callee_saved = &cc.callee_saved_regs;
                let stack_ptr_vn = self.stack_ptr_vn;
                let clobber_vars: SmallVec<[rsleigh::Vn; 4]> = self
                    .variables
                    .values()
                    .copied()
                    .filter(|v| !callee_saved.contains(v) && Some(*v) != stack_ptr_vn)
                    .collect();
                (arg_vars, clobber_vars, cc.ret_stack_pop)
            }
        };

        let arg_passing: SmallVec<[NodeOutputId; 4]> = arg_vars
            .iter()
            .map(|var| self.read_variable(var))
            .collect::<Result<_>>()?;
        self.validate_value_inputs(&arg_passing)?;

        let mut clobbered_kinds: SmallVec<[NodeOutputKind; 4]> = SmallVec::new();
        for var in &clobber_vars {
            let out = self.read_variable(var)?;
            let k = self.graph().output_kind(out);
            if !k.is_value() {
                return Err(anyhow!("output {out:?} is not a value edge (got {k:?})"));
            }
            clobbered_kinds.push(k);
        }

        let addr_kind = self.graph().output_kind(call_address);
        if !addr_kind.is_value() {
            return Err(anyhow!(
                "output {call_address:?} is not a value edge (got {addr_kind:?})"
            ));
        }

        // Snapshot pre-call SP for the post-call adjust.
        let sp_pre_call = match self.stack_ptr_vn {
            Some(sp) if ret_stack_pop != 0 => {
                self.read_variable_optional(&sp)?.map(|out| (sp, out))
            }
            _ => None,
        };

        // Per-call effective `no_memory_clobber`: the override CC, if any,
        // takes precedence; otherwise fall back to the function-default.
        let no_memory_clobber =
            override_cc.map_or(self.no_memory_clobber, |cc| cc.no_memory_clobber);

        let inputs = [ctrl, memory, call_address].into_iter().chain(arg_passing);
        // The Call node's signature always includes a Memory output
        // (validator local-typing enforces `[Control, Memory, *clobbers]`).
        // When the CC
        // declares no_memory_clobber, we keep the Memory output but leave it
        // dangling — the region's memory chain is NOT advanced, so subsequent
        // loads see the pre-call memory edge.  LoadReadOnly / StackLoadForward
        // can therefore forward through the call.
        let outputs = [NodeOutputKind::Control, NodeOutputKind::Memory]
            .into_iter()
            .chain(clobbered_kinds);
        let call = self.create_node(NodeKind::Call, inputs, outputs);
        let call_outputs: Vec<_> = self.graph().node_outputs(call).into_iter().collect();

        self.advance_cur_region_ctrl(call_outputs[0])?;
        if !no_memory_clobber {
            self.advance_cur_region_memory(call_outputs[1])?;
        }
        for (variable, new_val) in core::iter::zip(&clobber_vars, call_outputs.iter().skip(2)) {
            self.write_variable(variable, *new_val)?;
        }

        // Record the per-Call override clobber list when an override was used.
        if override_cc.is_some() {
            let list: Vec<rsleigh::Vn> = clobber_vars.into_iter().collect();
            self.body_mut().graph.set_call_clobbered_override(call, list);
        }

        // Model the caller-visible effect of the callee's `ret` on SP: on
        // stack-push ISAs `ret` pops the return-address word, so the
        // caller's post-call SP is `pre_call_SP + ret_stack_pop`.  On
        // link-register ISAs `ret_stack_pop == 0` and we skip this entirely.
        if let Some((sp, pre)) = sp_pre_call {
            let sp_ty: NodeOutputType = sp.size.try_into()?;
            let const_id = self.build_int_const(ret_stack_pop as u64, sp_ty)?;
            let adjusted =
                self.build_int_binary_operation(pre, const_id, IntBinaryOp::Add, sp_ty)?;
            self.write_variable(&sp, adjusted)?;
        }
        Ok(call)
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
        self.body_mut()
            .graph
            .set_call_other_name(node, name.to_string());
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
    /// `memory_edge` is true.  Similarly the strider layer is
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
            self.graph().node_outputs(node).into_iter().collect();

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
        let body = self.body_mut();
        body.graph.set_call_other_name(node, name.to_string());
        if !writes_vec.is_empty() {
            body.graph.set_call_clobbered_override(node, writes_vec);
        }

        Ok((node, value_output, clobber_outputs))
    }

}
