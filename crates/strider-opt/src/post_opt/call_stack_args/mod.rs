//! Stack-argument collection post-pass. The shared SP-decomposition and
//! memory-SSA machinery lives in [`crate::sp_expr`] / [`crate::sp_expr::mem_ssa`].
//!
//! `CallStackArgCollect` — post-pass that, for each `Call` node, walks the
//! shared memory-SSA chain to find the `Store` supplying each positional
//! stack-arg slot and appends the stored data values as additional Call
//! inputs.

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::error::Result;
use crate::pipeline::PostOptimizer;
use crate::sp_expr::{SpAliasCfg, SpExpr};

#[cfg(test)]
mod tests;

/// Collects the stack-passed arguments for one `Call` by walking the shared
/// memory-SSA chain slot-by-slot from the call-time stack pointer.
///
/// The convention's stack-arg offsets are relative to the **call-time SP**, so
/// the origin is the `Call`'s own SP input decomposed to an entry-SP-relative
/// `{ base, offset }`.  Starting at slot 0, each slot is probed with
/// [`SpAliasCfg::reaching_store`] (the `MemPhi`-sound memory-SSA walker): if a `Store`
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
    alias_cfg: &SpAliasCfg,
) -> Vec<ValueId> {
    // Call inputs: [control, memory, target, sp, ...args]; slots 1 (memory)
    // and 3 (sp) are guaranteed by the validated Call structural invariant.
    let inputs = function.node_inputs(call_id);
    let mem_value = function
        .memory_input_of(call_id)
        .expect("Call carries a memory input (slot 1)");
    let sp_value = inputs[3];

    // Origin: the call-time SP, decomposed to an entry-SP-relative offset so a
    // slot's absolute (entry-relative) probe offset is `call_sp_off +
    // offset_of(slot)`.  A non-decomposable SP input (e.g. a phi-SP) yields no
    // args.  (`alias_cfg` is call-blocking: a `Call` on the chain clobbers the
    // outgoing-args frame, distinct SP bases stay conservative.)
    let Some(SpExpr {
        base,
        offset: call_sp_off,
    }) = alias_cfg.decompose(function, sp_value)
    else {
        return Vec::new();
    };

    let mut args = Vec::new();
    let mut cursor = 0usize;
    loop {
        let slot_off = call_sp_off + stack_args.offset_of(cursor);
        // Probe a single byte at the slot start; the store reports its own
        // width back so a wider-than-slot argument is discovered, not forced
        // into a fixed range.
        let Some(store) = alias_cfg.reaching_store(function, mem_value, base, slot_off, 1) else {
            break;
        };
        // Only a store anchored exactly at the slot start supplies this
        // argument; a covering store anchored earlier (a wider preceding
        // argument the cursor should already have passed) means the slot was
        // not itself written — end the prefix.
        if store.store_offset != slot_off {
            break;
        }
        args.push(store.data(function));
        // A store wider than one slot is one argument spanning several slots;
        // advance the cursor past every slot it covers.
        cursor += stack_args.slots_spanned(store.size(function));
    }
    args
}

/// Walks backward from each `Call`'s memory input (via the shared memory-SSA
/// walker) to reconstruct stack-passed arguments and appends them as extra
/// `Call` inputs in positional order.  Intended to run *once*, as an
/// [`OptimizerPipeline::add_post_pass`][crate::OptimizerPipeline::add_post_pass]
/// after the fixed-point loop has converged.
///
/// The positional stack-arg formula is derived on-demand from the function's
/// own calling convention (`Function::default_cc`), the stack-pointer varnode
/// likewise, and the alias precision from the per-run [`AliasMode`][crate::AliasMode] — the
/// pass carries no configuration of its own.  A per-`Call` CC override (e.g. a
/// varargs site) wins over the convention default.
#[derive(Clone)]
pub struct CallStackArgCollect;

impl PostOptimizer for CallStackArgCollect {
    fn apply(
        &self,
        ctx: &mut crate::EditFunction<'_>,
        opt_ctx: &mut crate::OptCtx<'_>,
    ) -> Result<()> {
        let alias_mode = opt_ctx.options.alias_mode;
        // Each call is processed independently below (no cross-call data
        // dependency, detection order irrelevant), so iterate the cached live
        // set directly — no graph walk — like the sibling post-passes. The
        // owned `Vec` lets the immutable borrow end before the per-call
        // mutation loop takes `ctx` mutably.
        let calls: Vec<NodeId> = ctx.live_of_kind(|k| matches!(k, NodeKind::Call)).collect();
        // Build the SP-alias context once for the whole pass and reuse it across
        // every call site (decompositions route through the function's cache).
        let alias_cfg = SpAliasCfg::call_blocking(alias_mode);
        for call_id in calls {
            // The call's effective stack-arg layout: a per-call CC override (a
            // varargs site) if recorded, else the function-default CC.  `None`
            // means the convention passes no stack args, so skip.
            let Some(stack_args) = ctx.function().get_cc(call_id).stack_args else {
                continue;
            };
            // Append each discovered stack-arg value as an extra Call input
            // (positional order); the loop is a no-op when the call passes none.
            // Single-shot post-pass, so we don't track a changed/unchanged result.
            let args = collect_stack_args(ctx.function(), call_id, stack_args, &alias_cfg);
            for arg_value in &args {
                ctx.add_node_input(call_id, *arg_value)?;
            }
        }
        Ok(())
    }
}
