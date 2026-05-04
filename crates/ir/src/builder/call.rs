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

    /// Emits a `CallOther` (user-defined op) node and advances the control
    /// and memory chain of the active region.
    ///
    /// `args` are additional arguments to the intrinsic (may be empty).
    /// `output_ty` is `Some` when the source instruction has an output varnode
    /// and `None` when the intrinsic produces no value (e.g. `syscall` without
    /// an explicit return).  Memory is always treated as clobbered.
    ///
    /// Returns `(node_id, value_output)` where `value_output` is the
    /// optional value-typed output of the call (matching `output_ty`), and
    /// `node_id` is the freshly created [`NodeKind::CallOther`] — useful for
    /// callers that need to record the user-op name in
    /// [`crate::Graph::set_call_other_name`].
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion` / `RegionTerminated`
    /// when there is no active region, or `ExpectedValue` when
    /// any element of `args` is not a value edge.
    pub fn build_call_other(
        &mut self,
        user_op_id: u64,
        args: &[NodeOutputId],
        output_ty: Option<NodeOutputType>,
    ) -> Result<(NodeId, Option<NodeOutputId>)> {
        // Default: conservative clobber set = every tracked variable except SP.
        let stack_ptr_vn = self.stack_ptr_vn;
        let clobber_vars: SmallVec<[rsleigh::Vn; 8]> = self
            .variables
            .values()
            .copied()
            .filter(|v| Some(*v) != stack_ptr_vn)
            .collect();
        self.build_call_other_with_clobbers(user_op_id, args, output_ty, &clobber_vars)
    }

    /// Variant of [`Self::build_call_other`] that takes an explicit
    /// `clobber_vars` slice instead of computing the conservative
    /// "every-tracked-variable-except-SP" default.  Used by callers
    /// that know the user-op's true clobber semantics — e.g. lifters
    /// emitting `setISAMode` (a known no-op in the IR's value model)
    /// pass an empty slice so no variables get rebound through the
    /// CallOther.
    ///
    /// # Errors
    ///
    /// Same set as [`Self::build_call_other`].
    pub fn build_call_other_with_clobbers(
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
