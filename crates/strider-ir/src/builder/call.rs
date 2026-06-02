use anyhow::anyhow;
use smallvec::SmallVec;

use super::FunctionBuilder;
use crate::error::Result;
use crate::node::{NodeId, NodeKind, ValueId, ValueKind, ValueType};
use crate::ops::IntBinaryOp;

/// The per-Call ABI shape resolved by
/// [`FunctionBuilder::select_call_abi`]: `(arg_vars, clobber_vars,
/// ret_stack_pop, preserves_memory)` — either the function-default
/// snapshot or the override CC's filtered view.
type CallAbiSelection = (
    SmallVec<[rsleigh::Vn; 4]>,
    SmallVec<[rsleigh::Vn; 4]>,
    i64,
    bool,
);

/// The result of [`FunctionBuilder::read_call_value_inputs`]: arg
/// input ids (in CC order) plus clobber output kinds (one per
/// `clobber_vars`).  Feeds the `build_call_kind` call in
/// [`FunctionBuilder::build_call_with_cc`].
struct CallValueInputs {
    arg_passing: SmallVec<[ValueId; 4]>,
    clobbered_kinds: SmallVec<[ValueKind; 4]>,
}

impl FunctionBuilder {
    /// Shared call-class node emitter.  Emits a `Call` / `CallOther`
    /// node from already-resolved ingredients.  Does **not** read
    /// variables, resolve a calling convention, or rebind variables —
    /// those are the wrapper / caller's job.
    ///
    /// - Snapshots the region's live control + memory edges.
    /// - Outputs are ALWAYS `[Control, Memory]`, then `result_ty` as a
    ///   single `Typed` value output when `Some`, then one output per
    ///   `clobber_kinds` entry.  The Memory output is always present
    ///   even for a memory-preserving call ("you don't have to use it").
    /// - Inputs are `[ctrl, mem]` followed by `target` (when `Some`)
    ///   then `arg_values`.  Any clobber-read inputs a node kind needs
    ///   must already be present in `arg_values` — this emitter does
    ///   not auto-read them.
    /// - When `terminate` is `false`: advances the region's control to
    ///   the node's Control output (region stays open).
    ///   When `terminate` is `true`: marks the region terminated without
    ///   emitting a separate terminator node (used for the `NoReturn`-
    ///   class `CallOther` — the CallOther node itself is the region
    ///   exit).
    /// - Advances the region's memory to the node's Memory output IFF
    ///   `advance_memory` is set (the caller decides whether memory is
    ///   preserved).
    /// - Tags `Function::value_vn[output] = clobber_vns[i]` for each
    ///   clobber output.
    ///
    /// Returns `(node, result_value, clobber_values)` where
    /// `result_value.is_some() == result_ty.is_some()` and
    /// `clobber_values.len() == clobber_kinds.len() == clobber_vns.len()`.
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion` when no region is active; an error when
    /// any `arg_values` entry is not a value edge, when `clobber_vns`
    /// and `clobber_kinds` differ in length, or when any `clobber_kinds`
    /// entry is not a value kind.
    // Eight resolved-ingredient channels plus two toggle flags is the
    // natural shape; a builder struct would add boilerplate without
    // simplifying the call sites.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn build_call_kind(
        &mut self,
        kind: NodeKind,
        target: Option<ValueId>,
        arg_values: &[ValueId],
        clobber_vns: &[rsleigh::Vn],
        clobber_kinds: &[ValueKind],
        result_ty: Option<ValueType>,
        advance_memory: bool,
        terminate: bool,
    ) -> Result<(NodeId, Option<ValueId>, Vec<ValueId>)> {
        if clobber_vns.len() != clobber_kinds.len() {
            return Err(anyhow!(
                "build_call_kind({kind:?}): clobber_vns.len() = {} but clobber_kinds.len() = {}",
                clobber_vns.len(),
                clobber_kinds.len()
            ));
        }
        self.validate_value_inputs(arg_values)?;
        if let Some(t) = target {
            self.validate_value_inputs(std::slice::from_ref(&t))?;
        }
        for (i, k) in clobber_kinds.iter().enumerate() {
            if !k.is_value() {
                return Err(anyhow!(
                    "build_call_kind({kind:?}): clobber_kinds[{i}] is not a value kind: {k:?}"
                ));
            }
        }

        // Snapshot the region's live ctrl + mem edges (without
        // terminating).
        let ctrl = self.cur_region_control()?;
        let memory = self.cur_region_memory()?;

        // Outputs: [Control, Memory] ++ result_ty? ++ clobber_kinds.
        let mut output_kinds: SmallVec<[ValueKind; 8]> = SmallVec::new();
        output_kinds.push(ValueKind::Control);
        output_kinds.push(ValueKind::Memory);
        if let Some(ty) = result_ty {
            output_kinds.push(ValueKind::Typed(ty));
        }
        output_kinds.extend(clobber_kinds.iter().copied());

        // Inputs: [ctrl, mem] ++ target? ++ arg_values.
        let inputs = [ctrl, memory]
            .into_iter()
            .chain(target)
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

        let (result_value, clobber_start) = if result_ty.is_some() {
            (Some(outputs[2]), 3usize)
        } else {
            (None, 2usize)
        };
        let clobber_values: Vec<ValueId> = outputs[clobber_start..].to_vec();

        // Tag each clobber output value with the register it clobbers
        // (via `value_vn`) so pattern queries can recover the clobber
        // varnode for each slot.
        for (value, vn) in core::iter::zip(&clobber_values, clobber_vns) {
            self.function_mut().set_clobbered_vn(*value, *vn);
        }

        Ok((node, result_value, clobber_values))
    }

    /// Emits a `Call` node into the current region using the
    /// function-default calling convention.  Equivalent to
    /// [`Self::build_call_with_cc`] with `override_cc = None`.
    ///
    /// Does **not** terminate the region — the Call sits inline in the
    /// region's control/memory chain.
    ///
    /// # Errors
    ///
    /// See [`Self::build_call_with_cc`].
    pub fn build_call(&mut self, call_address: ValueId) -> Result<()> {
        self.build_call_with_cc(call_address, None).map(|_| ())
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
    pub fn build_call_with_cc(
        &mut self,
        call_address: ValueId,
        override_cc: Option<&strider_target::BuiltCallingConvention>,
    ) -> Result<NodeId> {
        // Resolve the per-call ABI shape (arg list, clobber list,
        // ret_stack_pop, preserves_memory) from either the override CC
        // or the function-default snapshot stamped at builder construction.
        let (arg_vars, clobber_vars, ret_stack_pop, preserves_memory) =
            self.select_call_abi(override_cc);

        // Read every arg + clobber variable and verify the
        // call_address is a value edge.  This also produces the
        // arg-input id list + the clobber-kind list.
        let CallValueInputs {
            arg_passing,
            clobbered_kinds,
        } = self.read_call_value_inputs(call_address, &arg_vars, &clobber_vars)?;

        // Snapshot pre-call SP for the post-call adjust (only on
        // stack-push ISAs where `ret_stack_pop != 0`).
        let sp_pre_call = self.snapshot_pre_call_sp(ret_stack_pop)?;

        // Emit the Call node via the shared emitter.  The Call's value
        // inputs after `call_address` are exactly its args — the
        // clobbered vars are NOT inputs (they were read only to recover
        // their output-slot kinds).  Control always advances; memory
        // advances unless the CC preserves it (so subsequent loads see
        // the pre-call memory edge — the Memory output is still present
        // but left dangling).
        let (call, _value, clobber_values) = self.build_call_kind(
            NodeKind::Call,
            Some(call_address),
            &arg_passing,
            &clobber_vars,
            &clobbered_kinds,
            None,
            !preserves_memory,
            false,
        )?;

        // Post-call write-back: rebind each clobbered variable to its
        // fresh clobber output (the `value_vn` clobber tag is applied by
        // `build_call_kind`).
        for (variable, new_val) in core::iter::zip(&clobber_vars, &clobber_values) {
            self.write_variable(variable, *new_val)?;
        }

        // Record the override CC on the Call (subsuming its stack-arg
        // offsets) so per-address-CC consumers — the stack-arg collector
        // and pattern queries — can recover it.
        if let Some(cc) = override_cc {
            self.function_mut().set_call_cc(call, cc.clone());
        }

        // Apply the post-call SP adjust on stack-push ISAs.
        self.apply_post_call_sp_adjust(sp_pre_call, ret_stack_pop)?;

        Ok(call)
    }

    /// Emits a `CallOther` node into the current region from
    /// already-resolved pcode operands.
    ///
    /// This is the single IR builder for every IR-emitting `CallOther`
    /// form (the `NoReturn` trap-class and the modeled `Call(abi)`
    /// class of [`strider_target::call_other_abi::classify`]).  The
    /// `NoOp` class skips IR emission entirely (no node is produced).
    ///
    /// The lifter owns aliasing: it does the aliasing-aware `read_vn`
    /// for every argument / implicit-read (feeding them through
    /// `arg_values`) and `write_vn` for every implicit-write writeback
    /// (against the returned `clobber_values`).
    ///
    /// When `terminate` is `true` (the `NoReturn` class), the region is
    /// closed as part of this call — no separate
    /// [`Self::mark_cur_region_terminated`] call is needed.
    /// When `terminate` is `false` (the modeled `Call(abi)` class),
    /// the region's control advances to the CallOther's Control output
    /// and the region stays open.
    ///
    /// Inputs of the resulting node: `[ctrl, mem] ++ target? ++ arg_values`.
    /// Outputs: `[Control, Memory] ++ result_ty? ++ clobber_kinds`.
    ///
    /// The region's memory token does **not** advance here — the lifter
    /// calls `advance_cur_region_memory` itself when the ABI's
    /// `clobbers_memory` flag is set.  Each `clobber_vns` entry is tagged
    /// on its clobber output via `Function::value_vn` so
    /// `pattern::Match::get_vn` can recover the original Vn names.
    /// Stamps `name` on `Graph::call_other_names`.
    ///
    /// Returns `(node, result_value, clobber_values)`.
    ///
    /// # Errors
    ///
    /// Returns an error when any `arg_values` entry is not a value edge,
    /// when `clobber_vns` and `clobber_kinds` differ in length, when any
    /// `clobber_kinds` entry is not a value kind, or when the region
    /// cannot be advanced or terminated.
    #[allow(clippy::too_many_arguments)]
    pub fn build_call_other(
        &mut self,
        user_op_id: u64,
        name: &str,
        target: Option<ValueId>,
        arg_values: &[ValueId],
        clobber_vns: &[rsleigh::Vn],
        clobber_kinds: &[ValueKind],
        result_ty: Option<ValueType>,
        terminate: bool,
    ) -> Result<(NodeId, Option<ValueId>, Vec<ValueId>)> {
        // Memory advancement is the lifter's call (it advances IFF the
        // ABI clobbers memory), so the shared emitter never advances it.
        let (node, value, clobber_values) = self.build_call_kind(
            NodeKind::CallOther { user_op_id },
            target,
            arg_values,
            clobber_vns,
            clobber_kinds,
            result_ty,
            false,
            terminate,
        )?;
        self.function_mut().set_call_other_name(node, name.to_string());
        Ok((node, value, clobber_values))
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
        let function_default_preserves_memory = self.function.preserves_memory();
        let function_default_ret_stack_pop = self.function.ret_stack_pop();
        let preserves_memory =
            override_cc.map_or(function_default_preserves_memory, |cc| cc.preserves_memory);
        match override_cc {
            None => (
                self.function.arg_passing_vars().into_iter().collect(),
                self.function.call_clobbered_regs().into_iter().collect(),
                function_default_ret_stack_pop,
                preserves_memory,
            ),
            Some(cc) => {
                let arg_vars: SmallVec<[rsleigh::Vn; 4]> = cc
                    .arg_passing_regs
                    .iter()
                    .copied()
                    .filter(|v| self.var_table.contains(v))
                    .collect();
                // Clobbers go through the SAME ret-prefixed
                // `call_clobbered_for` derivation as the default branch, so
                // an override Call's clobber output slots are ordered
                // identically (ret regs first, then the rest).  The SP
                // exclusion uses the function-default `stack_vn` (SP is the
                // caller's, function-stable) which `call_clobbered_for`
                // already applies; the membership predicate is identical to
                // the former `clobbers_override_var` filter, so only the
                // slot ORDER changes.
                let clobber_vars: SmallVec<[rsleigh::Vn; 4]> =
                    self.function.call_clobbered_for(cc).into_iter().collect();
                (arg_vars, clobber_vars, cc.ret_stack_pop, preserves_memory)
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
        call_address: ValueId,
        arg_vars: &[rsleigh::Vn],
        clobber_vars: &[rsleigh::Vn],
    ) -> Result<CallValueInputs> {
        let arg_passing: SmallVec<[ValueId; 4]> = arg_vars
            .iter()
            .map(|var| self.read_variable(var))
            .collect::<Result<_>>()?;
        self.validate_value_inputs(&arg_passing)?;

        let mut clobbered_kinds: SmallVec<[ValueKind; 4]> = SmallVec::new();
        for var in clobber_vars {
            let value = self.read_variable(var)?;
            let k = self.function().value_kind(value);
            if !k.is_value() {
                return Err(anyhow!("output {value:?} is not a value edge (got {k:?})"));
            }
            clobbered_kinds.push(k);
        }

        let addr_kind = self.function().value_kind(call_address);
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
    /// Returns `None` on link-register ISAs (`ret_stack_pop == 0`, which
    /// also covers the trivial CC) or when the SP value is unavailable.
    fn snapshot_pre_call_sp(
        &mut self,
        ret_stack_pop: i64,
    ) -> Result<Option<(rsleigh::Vn, ValueId)>> {
        if ret_stack_pop == 0 {
            // Link-register ISAs (and the trivial CC) never adjust SP
            // across a call.
            return Ok(None);
        }
        let sp = self.function.stack_vn();
        Ok(self.read_variable_optional(&sp)?.map(|value| (sp, value)))
    }

    /// `apply_post_call_sp_adjust` helper: model the caller-visible
    /// effect of the callee's `ret` on SP — on stack-push ISAs `ret`
    /// pops the return-address word, so the caller's post-call SP is
    /// `pre_call_SP + ret_stack_pop`.  On link-register ISAs
    /// `ret_stack_pop == 0` and the `snapshot_pre_call_sp` snapshot
    /// is `None`, so this is a no-op.
    fn apply_post_call_sp_adjust(
        &mut self,
        sp_pre_call: Option<(rsleigh::Vn, ValueId)>,
        ret_stack_pop: i64,
    ) -> Result<()> {
        if let Some((sp, pre)) = sp_pre_call {
            let sp_ty = ValueType::int_for_byte_size(sp.size)?;
            let const_id = self.build_int_const(ret_stack_pop as u64, sp_ty)?;
            let adjusted =
                self.build_int_binary_operation(pre, const_id, IntBinaryOp::Add, sp_ty)?;
            self.write_variable(&sp, adjusted)?;
        }
        Ok(())
    }
}
