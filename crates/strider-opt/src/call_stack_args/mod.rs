//! Stack-argument collection post-pass. The shared SP-decomposition and
//! memory-SSA machinery lives in [`crate::sp_expr`] / [`crate::memory_ssa`].
//!
//! `CallStackArgCollect` — post-pass that, for each `Call` node, walks the
//! shared memory-SSA chain to find the `Store` supplying each positional
//! stack-arg slot and appends the stored data values as additional Call
//! inputs.

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::error::Result;
use crate::pipeline::{OptimizationResult, Optimizer};
use crate::sp_expr::{SpExpr, SpExprMemo, decompose_sp, reaching_sp_store};

#[cfg(test)]
mod tests;

/// Collects the stack-passed arguments for one `Call` by walking the shared
/// memory-SSA chain slot-by-slot from the call-time stack pointer.
///
/// The convention's stack-arg offsets are relative to the **call-time SP**, so
/// the origin is the `Call`'s own SP input decomposed to an entry-SP-relative
/// `{ base, offset }`.  Starting at slot 0, each slot is probed with
/// [`reaching_sp_store`] (the `MemPhi`-sound memory-SSA walker): if a `Store`
/// is anchored exactly at the slot's byte offset its data value is the
/// argument.  A store wider than one slot (e.g. an 8-byte `double` on a
/// 4-byte-stride ABI) is **one** argument occupying several slots: the cursor
/// advances by its slot span (`ceil(size / increment)`) but appends a single
/// Call input.  The walk yields the contiguous prefix `0..k`, stopping at the
/// first slot with no anchored store (a true gap, a `Call`, a disagreeing
/// `MemPhi`, an opaque producer, or a store rooted at a different SP base).
///
/// # Over-collection is intentional
///
/// Argument pushes are indistinguishable from incidental in-window stack
/// writes (a prologue buffer zero-init, a `push ebx` save) once lowered to
/// memory — both are SP-relative stores at contiguous slots reaching the call.
/// This pass therefore collects **every** plausible stack-arg store; a caller
/// reasoning about a specific function disambiguates.  The alias precision
/// (`AliasMode`) still governs which intervening stores are proven disjoint
/// (steppable) versus clobbering, and a `Call` on the chain still terminates
/// collection (the callee may overwrite the frame).
fn collect_stack_args(
    function: &strider_ir::Function,
    call_id: NodeId,
    stack_args: strider_target::StackArgs,
    stack_vn: rsleigh::Vn,
    sp_memo: &mut SpExprMemo,
    alias_mode: crate::AliasMode,
) -> Vec<ValueId> {
    // Call inputs: [control, memory, target, sp, ...args]; slots 1 (memory)
    // and 3 (sp) are guaranteed by the validated Call structural invariant.
    let inputs = function.node_inputs(call_id);
    let mem_value = inputs[1];
    let sp_value = inputs[3];
    let mem_start = function.producer(mem_value);

    // Origin: the call-time SP, decomposed to an entry-SP-relative offset so a
    // slot's absolute (entry-relative) probe offset is `call_sp_off +
    // offset_of(slot)`.  A non-decomposable SP input (e.g. a phi-SP) yields no
    // args.
    let Some(SpExpr {
        base,
        offset: call_sp_off,
    }) = decompose_sp(function, sp_value, stack_vn, sp_memo)
    else {
        return Vec::new();
    };

    let mut args = Vec::new();
    let mut cursor = 0usize;
    loop {
        let slot_off = call_sp_off + stack_args.offset_of(cursor);
        let Some(store) = reaching_sp_store(
            function,
            mem_start,
            base,
            slot_off,
            // Probe a single byte at the slot start; the store reports its own
            // width back so a wider-than-slot argument is discovered, not
            // forced into a fixed range.
            1,
            sp_memo,
            alias_mode,
            // A Call on the chain clobbers the outgoing-args frame.
            true,
            // Stay conservative on distinct SP bases.
            false,
        ) else {
            break;
        };
        // Only a store anchored exactly at the slot start supplies this
        // argument; a covering store anchored earlier (a wider preceding
        // argument the cursor should already have passed) means the slot was
        // not itself written — end the prefix.
        if store.store_offset != slot_off {
            break;
        }
        args.push(store.data);
        // A store wider than one slot is one argument spanning several slots:
        // `ceil(size / increment)` (both positive; `i64::div_ceil` is still
        // unstable, so compute it directly).
        let span = (store.size.max(1) + stack_args.increment - 1) / stack_args.increment;
        cursor += span.max(1) as usize;
    }
    args
}

/// Collects stack-passed arguments for one Call node and appends the
/// discovered data values as additional Call inputs (in positional order).
fn try_collect_stack_args(
    ctx: &mut crate::EditFunction<'_>,
    call_id: NodeId,
    stack_args: strider_target::StackArgs,
    stack_vn: rsleigh::Vn,
    sp_memo: &mut SpExprMemo,
    alias_mode: crate::AliasMode,
) -> Result<OptimizationResult> {
    let args = collect_stack_args(ctx.function(), call_id, stack_args, stack_vn, sp_memo, alias_mode);
    if args.is_empty() {
        return Ok(OptimizationResult::NoChange);
    }
    for data in &args {
        ctx.add_node_input(call_id, *data)?;
    }
    Ok(OptimizationResult::Changed)
}

/// Walks backward from each `Call`'s memory input (via the shared memory-SSA
/// walker) to reconstruct stack-passed arguments and appends them as extra
/// `Call` inputs in positional order.  Intended to run *once*, as an
/// [`OptimizerPipeline::add_post_pass`][crate::OptimizerPipeline::add_post_pass]
/// after the fixed-point loop has converged.
///
/// The positional stack-arg formula is derived on-demand from the function's
/// own calling convention (`Function::default_cc`), the stack-pointer varnode
/// likewise, and the alias precision from [`crate::OptCtx::alias_mode`] — the
/// pass carries no configuration of its own.  A per-`Call` CC override (e.g. a
/// varargs site) wins over the convention default.
#[derive(Clone, Default)]
pub struct CallStackArgCollect;

impl CallStackArgCollect {
    /// Creates the pass.  The stack-arg layout, stack pointer, and alias
    /// precision all come from the function / shared [`crate::OptCtx`] at
    /// apply time.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Optimizer for CallStackArgCollect {
    fn apply(
        &self,
        ctx: &mut crate::EditFunction<'_>,
        opt_ctx: &mut crate::OptCtx<'_>,
    ) -> Result<OptimizationResult> {
        let alias_mode = opt_ctx.options.alias_mode;
        // SSoT: derive the default stack-arg formula on-demand from the
        // function's own CC.  `None` means the convention passes no arguments
        // on the stack.
        let default_stack_args = ctx.function().default_cc().positional_arg_layout().stack;
        // Collect the reachable `Call` nodes via a plain pre-order walk.
        // Each call is processed independently below (no cross-call data
        // dependency), so the owned `Vec` just lets the immutable walk borrow
        // end before the per-call mutation loop takes `ctx` mutably.
        let calls: Vec<NodeId> = ctx.walk_kind(|k| matches!(k, NodeKind::Call)).collect();
        let mut result = OptimizationResult::NoChange;
        let stack_vn = ctx.function().default_cc().stack_vn;
        for call_id in calls {
            // Per-call override (e.g. a varargs call site) wins over the
            // convention default; when both are absent the call passes no
            // stack args and is skipped.
            let override_stack_args = ctx.function().call_stack_args_override(call_id);
            let Some(stack_args) = override_stack_args.or(default_stack_args) else {
                continue;
            };
            result |= try_collect_stack_args(
                ctx,
                call_id,
                stack_args,
                stack_vn,
                &mut opt_ctx.sp_memo,
                alias_mode,
            )?;
        }
        Ok(result)
    }
}
