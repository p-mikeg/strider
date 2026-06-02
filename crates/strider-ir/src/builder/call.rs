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

/// The result of [`FunctionBuilder::read_call_value_inputs`]: arg
/// input ids (in CC order), ret-val output kinds (one per `ret_val_vars`),
/// and clobber output kinds (one per `clobber_vars`).
/// Feeds the `build_call_kind` call in
/// [`FunctionBuilder::build_call_with_cc`].
struct CallValueInputs {
    arg_passing: SmallVec<[ValueId; 4]>,
    ret_val_kinds: SmallVec<[ValueKind; 4]>,
    clobbered_kinds: SmallVec<[ValueKind; 4]>,
}

impl FunctionBuilder {
    /// Shared call-class node emitter.  Emits a `Call` / `CallOther`
    /// node from already-resolved ingredients.  Does **not** read
    /// variables, resolve a calling convention, or rebind variables —
    /// those are the wrapper / caller's job.
    ///
    /// - Snapshots the region's live control + memory edges.
    /// - Outputs are ALWAYS `[Control, Memory]`, then one output per
    ///   `ret_val_kinds` entry (the return-value group), then one output per
    ///   `clobber_kinds` entry (the havoc'd caller-saved group).  The Memory
    ///   output is always present even for a memory-preserving call ("you
    ///   don't have to use it").
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
    /// `ret_val_values.len() == ret_val_kinds.len() == ret_val_vns.len()`
    /// and `clobber_values.len() == clobber_kinds.len() == clobber_vns.len()`.
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion` when no region is active; an error when
    /// any `arg_values` entry is not a value edge, when `ret_val_vns`
    /// and `ret_val_kinds` differ in length, when `clobber_vns`
    /// and `clobber_kinds` differ in length, or when any `ret_val_kinds` /
    /// `clobber_kinds` entry is not a value kind.
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
        ret_val_kinds: &[ValueKind],
        clobber_vns: &[rsleigh::Vn],
        clobber_kinds: &[ValueKind],
        advance_memory: bool,
        terminate: bool,
    ) -> Result<(NodeId, Vec<ValueId>, Vec<ValueId>)> {
        if ret_val_vns.len() != ret_val_kinds.len() {
            return Err(anyhow!(
                "build_call_kind({kind:?}): ret_val_vns.len() = {} but ret_val_kinds.len() = {}",
                ret_val_vns.len(),
                ret_val_kinds.len()
            ));
        }
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
        if let Some(sp) = sp_value {
            self.validate_value_inputs(std::slice::from_ref(&sp))?;
        }
        for (i, k) in ret_val_kinds.iter().enumerate() {
            if !k.is_value() {
                return Err(anyhow!(
                    "build_call_kind({kind:?}): ret_val_kinds[{i}] is not a value kind: {k:?}"
                ));
            }
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

        // Outputs: [Control, Memory] ++ ret_val_kinds ++ clobber_kinds.
        let mut output_kinds: SmallVec<[ValueKind; 8]> = SmallVec::new();
        output_kinds.push(ValueKind::Control);
        output_kinds.push(ValueKind::Memory);
        output_kinds.extend(ret_val_kinds.iter().copied());
        output_kinds.extend(clobber_kinds.iter().copied());

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
        // Resolve the per-call ABI shape (arg list, ret-val list, clobber
        // list, ret_stack_pop, preserves_memory) from either the override CC
        // or the function-default snapshot stamped at builder construction.
        let (arg_vars, ret_val_vars, clobber_vars, ret_stack_pop, preserves_memory) =
            self.select_call_abi(override_cc);

        // Read every arg + ret-val + clobber variable and verify the
        // call_address is a value edge.  This also produces the
        // arg-input id list + the ret-val-kind list + the clobber-kind list.
        let CallValueInputs {
            arg_passing,
            ret_val_kinds,
            clobbered_kinds,
        } = self.read_call_value_inputs(call_address, &arg_vars, &ret_val_vars, &clobber_vars)?;

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
            &ret_val_kinds,
            &clobber_vars,
            &clobbered_kinds,
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
    /// Inputs of the resulting node: `[ctrl, mem] ++ target? ++ arg_values`
    /// (CallOther carries no SP anchor — it has no CC stack args).
    /// Outputs: `[Control, Memory] ++ ret_val_kinds ++ clobber_kinds`.
    ///
    /// `output` specifies the pcode result destination varnode (the
    /// CallOther's single return value, if any).  When `Some(vn)`, a
    /// `Typed` ret-val output slot is emitted and its `value_vn` is
    /// tagged with `vn`; when `None`, no ret-val slots are emitted.
    ///
    /// The region's memory token does **not** advance here — the lifter
    /// calls `advance_cur_region_memory` itself when the ABI's
    /// `clobbers_memory` flag is set.  Each `clobber_vns` entry is tagged
    /// on its clobber output via `Function::value_vn` so
    /// `pattern::Match::get_vn` can recover the original Vn names.
    /// Stamps `name` on `Graph::call_other_names`.
    ///
    /// Returns `(node, ret_val_values, clobber_values)` where
    /// `ret_val_values` has 0 or 1 element (0 when `output` is `None`,
    /// 1 when `output` is `Some`).
    ///
    /// # Errors
    ///
    /// Returns an error when any `arg_values` entry is not a value edge,
    /// when `clobber_vns` and `clobber_kinds` differ in length, when any
    /// `clobber_kinds` entry is not a value kind, when `output` is `Some`
    /// but its varnode byte size has no matching [`ValueType`], or when
    /// the region cannot be advanced or terminated.
    #[allow(clippy::too_many_arguments)]
    pub fn build_call_other(
        &mut self,
        user_op_id: u64,
        name: &str,
        target: Option<ValueId>,
        arg_values: &[ValueId],
        clobber_vns: &[rsleigh::Vn],
        clobber_kinds: &[ValueKind],
        output: Option<rsleigh::Vn>,
        terminate: bool,
    ) -> Result<(NodeId, Vec<ValueId>, Vec<ValueId>)> {
        // Derive the ret-val group from the output varnode (if any).
        let (ret_val_vns, ret_val_kinds): (SmallVec<[rsleigh::Vn; 1]>, SmallVec<[ValueKind; 1]>) =
            if let Some(out_vn) = output {
                let ty = ValueType::int_for_byte_size(out_vn.size)?;
                (
                    smallvec::smallvec![out_vn],
                    smallvec::smallvec![ValueKind::Typed(ty)],
                )
            } else {
                (SmallVec::new(), SmallVec::new())
            };

        // Memory advancement is the lifter's call (it advances IFF the
        // ABI clobbers memory), so the shared emitter never advances it.
        let (node, ret_val_values, clobber_values) = self.build_call_kind(
            NodeKind::CallOther { user_op_id },
            target,
            None,
            arg_values,
            &ret_val_vns,
            &ret_val_kinds,
            clobber_vns,
            clobber_kinds,
            false,
            terminate,
        )?;
        self.function_mut().set_call_other_name(node, name.to_string());
        Ok((node, ret_val_values, clobber_values))
    }

    /// `select_call_abi` helper for [`Self::build_call_with_cc`]:
    /// resolve the per-call ABI shape from the override CC or the
    /// function-default snapshot.  Override args are filtered through
    /// the function's tracked-variable set so reads against unread
    /// vars don't fail with `VariableNotFound`; the ret-val and clobber
    /// lists are derived via `call_ret_vals_for` / `call_clobbered_for`.
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
                self.function.call_ret_val_regs().into_iter().collect(),
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
                // Ret-val and clobber lists both derived from the override CC
                // against the function's tracked-variable set.
                let ret_val_vars: SmallVec<[rsleigh::Vn; 4]> =
                    self.function.call_ret_vals_for(cc).into_iter().collect();
                let clobber_vars: SmallVec<[rsleigh::Vn; 4]> =
                    self.function.call_clobbered_for(cc).into_iter().collect();
                (arg_vars, ret_val_vars, clobber_vars, cc.ret_stack_pop, preserves_memory)
            }
        }
    }

    /// `read_call_value_inputs` helper: read every arg / ret-val / clobber
    /// variable and assert the call address is a value edge.  Returns
    /// the arg-input id list (in CC order), the ret-val-output-kind list
    /// (one entry per `ret_val_vars` entry), and the clobber-output-kind
    /// list (one entry per `clobber_vars` entry), all in the same order as
    /// their respective input slices.
    fn read_call_value_inputs(
        &mut self,
        call_address: ValueId,
        arg_vars: &[rsleigh::Vn],
        ret_val_vars: &[rsleigh::Vn],
        clobber_vars: &[rsleigh::Vn],
    ) -> Result<CallValueInputs> {
        let arg_passing: SmallVec<[ValueId; 4]> = arg_vars
            .iter()
            .map(|var| self.read_variable(var))
            .collect::<Result<_>>()?;
        self.validate_value_inputs(&arg_passing)?;

        let mut ret_val_kinds: SmallVec<[ValueKind; 4]> = SmallVec::new();
        for var in ret_val_vars {
            let value = self.read_variable(var)?;
            let k = self.function().value_kind(value);
            if !k.is_value() {
                return Err(anyhow!("ret-val output {value:?} is not a value edge (got {k:?})"));
            }
            ret_val_kinds.push(k);
        }

        let mut clobbered_kinds: SmallVec<[ValueKind; 4]> = SmallVec::new();
        for var in clobber_vars {
            let value = self.read_variable(var)?;
            let k = self.function().value_kind(value);
            if !k.is_value() {
                return Err(anyhow!("clobber output {value:?} is not a value edge (got {k:?})"));
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
            ret_val_kinds,
            clobbered_kinds,
        })
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
