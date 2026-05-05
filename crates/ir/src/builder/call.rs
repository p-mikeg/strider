use anyhow::anyhow;
use smallvec::SmallVec;

use super::FunctionBuilder;
use crate::error::Result;
use crate::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};
use crate::ops::IntBinaryOp;

/// Outcome of [`FunctionBuilder::build_call_other`].
#[derive(Debug)]
#[must_use]
pub enum CallOtherOutcome {
    /// Classification was [`target::user_ops::UserOpClass::NoOp`].
    /// No IR node emitted; control / memory unchanged.
    NoOp,

    /// Classification was [`target::user_ops::UserOpClass::NoReturn`].
    /// A `NodeKind::CallOther` node was emitted with control + memory
    /// inputs only (no clobber outputs, no value output); its outputs
    /// dangle.  The cfg has already terminated the region on this
    /// CallOther (see `RegionTerminator::NoReturn`), so the per-region
    /// IR walk has nothing left to process.
    NoReturn,

    /// Classification was [`target::user_ops::UserOpClass::Call`]
    /// (v1-compat shim: ignores the ABI and uses the conservative
    /// every-tracked-variable-except-SP clobber set).  Removed in
    /// Task 6 once all callers migrate to `build_call_other_modeled`.
    Built {
        node: crate::node::NodeId,
        value: Option<crate::node::NodeOutputId>,
    },
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
    /// [`crate::Graph::call_clobbered_overrides`] so pattern queries
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
        override_cc: Option<&target::BuiltCallingConvention>,
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

        let inputs = [ctrl, memory, call_address].into_iter().chain(arg_passing);
        let outputs = [NodeOutputKind::Control, NodeOutputKind::Memory]
            .into_iter()
            .chain(clobbered_kinds);
        let call = self.create_node(NodeKind::Call, inputs, outputs);
        let call_outputs: Vec<_> = self.graph().node_outputs(call).into_iter().collect();

        self.advance_cur_region_ctrl(call_outputs[0])?;
        self.advance_cur_region_memory(call_outputs[1])?;
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

    /// Internal: emit a CallOther node with the conservative clobber
    /// set (every tracked variable except SP).  Used by the v1
    /// `build_call_other` for any classified call shape.
    pub(crate) fn build_call_other_opaque(
        &mut self,
        user_op_id: u64,
        args: &[NodeOutputId],
        output_ty: Option<NodeOutputType>,
    ) -> Result<(NodeId, Option<NodeOutputId>)> {
        let stack_ptr_vn = self.stack_ptr_vn;
        let clobber_vars: SmallVec<[rsleigh::Vn; 8]> = self
            .variables
            .values()
            .copied()
            .filter(|v| Some(*v) != stack_ptr_vn)
            .collect();
        self.build_call_other_with_clobbers(user_op_id, args, output_ty, &clobber_vars)
    }

    /// Emit a CallOther node intended as a region terminator (Linux
    /// `BUG_ON`-class trap).  Has only ctrl + memory inputs and
    /// ctrl + memory outputs — no clobbers, no value, no args.  The
    /// outputs are expected to dangle: the cfg has already terminated
    /// the region with `RegionTerminator::NoReturn`, so no successor
    /// will read them.  Stamps `name` on `Graph::call_other_names`.
    pub fn build_call_other_terminal(
        &mut self,
        user_op_id: u64,
        name: &str,
    ) -> Result<NodeId> {
        let ctrl = self.cur_region_control()?;
        let memory = self.cur_region_memory()?;
        let mut output_kinds: SmallVec<[NodeOutputKind; 4]> = SmallVec::new();
        output_kinds.push(NodeOutputKind::Control);
        output_kinds.push(NodeOutputKind::Memory);
        let inputs = [ctrl, memory];
        let node = self.create_node(
            NodeKind::CallOther { user_op_id },
            inputs,
            output_kinds,
        );
        self.body_mut()
            .graph
            .set_call_other_name(node, name.to_string());
        // Intentionally DO NOT call advance_cur_region_ctrl /
        // advance_cur_region_memory — outputs dangle.
        Ok(node)
    }

    /// Emit a CallOther with the precise per-op ABI shape.
    ///
    /// Inputs of the resulting node:
    ///   `[ctrl_in, mem_in, *args, *implicit_read_values]`
    ///
    /// Outputs of the resulting node:
    ///   `[ctrl_out, mem_out, value?, *clobber_per_implicit_write]`
    ///
    /// This method advances the region's control token to the new
    /// `ctrl_out` but **does not** advance the memory token — the
    /// strider layer is responsible for calling
    /// `advance_cur_region_memory(mem_out)` IFF the ABI's
    /// `memory_edge` is true.  Similarly the strider layer rebinds
    /// each `implicit_writes_vns` Vn to its corresponding
    /// `clobber_outputs` slot via `write_variable`.
    ///
    /// Stamps `name` on `Graph::call_other_names`.  Records a per-
    /// CallOther override on `Graph::call_clobbered_overrides` so that
    /// `pattern::Match::get_vn` recovers the correct varnode for each
    /// clobber slot directly from `implicit_writes_vns`.
    ///
    /// Returns `(node, value_output, clobber_outputs)`.
    /// `value_output.is_some() == output_ty.is_some()`.
    /// `clobber_outputs.len() == implicit_writes_vns.len()`.
    ///
    /// # Errors
    ///
    /// Returns an error when any `args` entry is not a value edge,
    /// when an implicit-read or implicit-write Vn is not a tracked
    /// variable, or when the resulting node fails to advance the
    /// active region's control token.
    pub fn build_call_other_modeled(
        &mut self,
        user_op_id: u64,
        name: &str,
        args: &[NodeOutputId],
        output_ty: Option<NodeOutputType>,
        implicit_reads_vns: &[rsleigh::Vn],
        implicit_writes_vns: &[rsleigh::Vn],
    ) -> Result<(NodeId, Option<NodeOutputId>, Vec<NodeOutputId>)> {
        let ctrl = self.cur_region_control()?;
        let memory = self.cur_region_memory()?;

        self.validate_value_inputs(args)?;

        // Read each implicit-read register through the variable
        // machinery — gives the current SSA value for that register
        // (including any aliasing fixups).  Width must be a value edge.
        let mut implicit_read_values: SmallVec<[NodeOutputId; 8]> = SmallVec::new();
        for vn in implicit_reads_vns {
            let out = self.read_variable(vn)?;
            let k = self.graph().output_kind(out);
            if !k.is_value() {
                return Err(anyhow!(
                    "implicit_read for user-op {name:?}: output {out:?} \
                     is not a value edge (got {k:?})"
                ));
            }
            implicit_read_values.push(out);
        }

        // Read each implicit-write register's *kind* so we can declare
        // the correct output slot type.  The value itself is irrelevant
        // here — we just need the kind.
        let mut implicit_write_kinds: SmallVec<[NodeOutputKind; 8]> = SmallVec::new();
        for vn in implicit_writes_vns {
            let out = self.read_variable(vn)?;
            let k = self.graph().output_kind(out);
            if !k.is_value() {
                return Err(anyhow!(
                    "implicit_write for user-op {name:?}: output {out:?} \
                     is not a value edge (got {k:?})"
                ));
            }
            implicit_write_kinds.push(k);
        }

        let mut output_kinds: SmallVec<[NodeOutputKind; 8]> = SmallVec::new();
        output_kinds.push(NodeOutputKind::Control);
        output_kinds.push(NodeOutputKind::Memory);
        if let Some(ty) = output_ty {
            output_kinds.push(NodeOutputKind::OutputType(ty));
        }
        output_kinds.extend(implicit_write_kinds);

        let inputs = [ctrl, memory]
            .into_iter()
            .chain(args.iter().copied())
            .chain(implicit_read_values.iter().copied());

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

    /// Classified CallOther construction.  Looks up `name` in
    /// [`target::user_ops::classify`] and dispatches to the matching
    /// builder shape.
    ///
    /// # Errors
    /// Returns [`crate::error::UnknownUserOpError`] (via `anyhow`) if
    /// `name` has no entry in the classification table.
    pub fn build_call_other(
        &mut self,
        name: &str,
        user_op_id: u64,
        args: &[NodeOutputId],
        output_ty: Option<NodeOutputType>,
    ) -> Result<CallOtherOutcome> {
        use target::user_ops::{UserOpClass, classify};
        let class = classify(name).ok_or_else(|| crate::error::UnknownUserOpError {
            name: name.to_string(),
        })?;
        match class {
            UserOpClass::NoOp => Ok(CallOtherOutcome::NoOp),
            UserOpClass::NoReturn => {
                let _node = self.build_call_other_terminal(user_op_id, name)?;
                Ok(CallOtherOutcome::NoReturn)
            }
            // v1-compat shim: any Call(abi) classification falls back to the
            // conservative-clobber Opaque path.  v2's strider routes through
            // the new build_call_other_modeled and bypasses this method
            // entirely.  This shim only keeps v1 callers (legacy tests still
            // calling build_call_other) compiling until Tasks 6-10 migrate
            // them out.
            UserOpClass::Call(_) => {
                let (node, value) =
                    self.build_call_other_opaque(user_op_id, args, output_ty)?;
                self.body_mut()
                    .graph
                    .set_call_other_name(node, name.to_string());
                Ok(CallOtherOutcome::Built { node, value })
            }
        }
    }

    /// Internal helper for [`Self::build_call_other_opaque`]: emits a
    /// CallOther with an explicit `clobber_vars` slice.  All callers
    /// now go through the classified [`Self::build_call_other`] entry
    /// point — this helper is `pub(crate)` and used only by
    /// [`Self::build_call_other_opaque`].
    ///
    /// # Errors
    ///
    /// Same set as [`Self::build_call_other`].
    pub(crate) fn build_call_other_with_clobbers(
        &mut self,
        user_op_id: u64,
        args: &[NodeOutputId],
        output_ty: Option<NodeOutputType>,
        clobber_vars: &[rsleigh::Vn],
    ) -> Result<(NodeId, Option<NodeOutputId>)> {
        let ctrl = self.cur_region_control()?;
        let memory = self.cur_region_memory()?;

        self.validate_value_inputs(args)?;

        let clobber_vars: SmallVec<[rsleigh::Vn; 8]> = clobber_vars.iter().copied().collect();

        // Read each clobbered variable to validate it has a kind we
        // can express.  Same defensive check as `build_call`.
        let mut clobber_kinds: SmallVec<[NodeOutputKind; 8]> = SmallVec::new();
        for var in &clobber_vars {
            let out = self.read_variable(var)?;
            let k = self.graph().output_kind(out);
            if !k.is_value() {
                return Err(anyhow!("output {out:?} is not a value edge (got {k:?})"));
            }
            clobber_kinds.push(k);
        }

        let mut output_kinds: SmallVec<[NodeOutputKind; 8]> = SmallVec::new();
        output_kinds.push(NodeOutputKind::Control);
        output_kinds.push(NodeOutputKind::Memory);
        if let Some(ty) = output_ty {
            output_kinds.push(NodeOutputKind::OutputType(ty));
        }
        output_kinds.extend(clobber_kinds);

        let inputs = [ctrl, memory].into_iter().chain(args.iter().copied());
        let node = self.create_node(NodeKind::CallOther { user_op_id }, inputs, output_kinds);
        let outputs: SmallVec<[NodeOutputId; 8]> =
            self.graph().node_outputs(node).into_iter().collect();
        self.advance_cur_region_ctrl(outputs[0])?;
        self.advance_cur_region_memory(outputs[1])?;

        // Optional value output sits at slot 2 when present; clobber
        // outputs follow at slot 2 (value-less) or slot 3 (with value).
        let (value_output, clobber_start_slot) = if output_ty.is_some() {
            (Some(outputs[2]), 3usize)
        } else {
            (None, 2usize)
        };

        // Rebind each clobbered variable to its CallOther output.
        for (var, out) in core::iter::zip(
            clobber_vars.iter(),
            outputs.iter().skip(clobber_start_slot),
        ) {
            self.write_variable(var, *out)?;
        }

        Ok((node, value_output))
    }
}
