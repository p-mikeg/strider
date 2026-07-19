//! For each `Call`, walks the memory-SSA chain to find the `Store` supplying
//! each positional stack-arg slot and appends the stored data as extra Call
//! inputs.  The SP-decomposition and walk machinery lives in
//! [`crate::sp_analysis`].

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::error::Result;
use crate::pipeline::PostOptimizer;
use crate::sp_analysis::{SpAnalyzer, SpExpr, SpOptions};

#[cfg(test)]
mod tests;

/// Stack-arg offsets are relative to the CALL-TIME SP, so the origin is the
/// `Call`'s own SP input decomposed to an entry-SP-relative `{ base, offset }`.
/// A store wider than one slot is ONE argument spanning several slots.  The
/// result is the contiguous prefix `0..k`, stopping at the first slot with no
/// anchored store.
///
/// Over-collection is intentional: once lowered to memory, argument pushes are
/// indistinguishable from incidental in-window stack writes (a prologue
/// zero-init, a `push ebx` save), so every plausible store is collected and the
/// caller disambiguates.  Alias precision still decides which intervening
/// stores are steppable, and a `Call` on the chain still ends collection since
/// the callee may overwrite the frame.
fn collect_stack_args(
    function: &strider_ir::Function,
    call_id: NodeId,
    stack_args: strider_target::StackArgs,
    alias_cfg: &SpAnalyzer,
) -> Vec<ValueId> {
    // Call inputs are [control, memory, target, sp, ...args]; slots 1 and 3
    // are guaranteed by the validated Call structural invariant.
    let inputs = function.node_inputs(call_id);
    let mem_value = function
        .memory_input_of(call_id)
        .expect("Call carries a memory input (slot 1)");
    let sp_value = inputs[3];

    // A slot's entry-relative probe offset is `call_sp_off + offset_of(slot)`.
    // A non-decomposable SP input, say a phi-SP, yields no args.
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
        // Probing a single byte lets the store report its own width back, so a
        // wider-than-slot argument is discovered rather than forced into a
        // fixed range.
        let Some(store) = alias_cfg.reaching_store(function, mem_value, base, slot_off, 1) else {
            break;
        };
        // A covering store anchored EARLIER is a wider preceding argument the
        // cursor should already have passed, meaning this slot was never
        // written itself, so the prefix ends here.
        if store.store_offset != slot_off {
            break;
        }
        args.push(store.data(function));
        cursor += stack_args.slots_spanned(store.size(function));
    }
    args
}

/// Runs ONCE after the fixed-point loop converges.
///
/// The stack-arg formula, the stack-pointer varnode, and the alias precision
/// are all derived on demand from the function and the per-run options, so the
/// pass carries no configuration.  A per-`Call` CC override, e.g. a varargs
/// site, wins over the convention default.
#[derive(Clone)]
pub struct CallStackArgCollect;

impl PostOptimizer for CallStackArgCollect {
    fn apply(
        &self,
        edit: &mut crate::EditFunction<'_>,
        opt_ctx: &mut crate::OptCtx<'_>,
    ) -> Result<()> {
        let alias_mode = opt_ctx.options.alias_mode;
        // Calls are independent of each other, so the cached live set is enough
        // and no graph walk is needed.  The owned `Vec` lets the immutable
        // borrow end before the mutation loop takes `edit` mutably.
        let calls: Vec<NodeId> = edit.live_of_kind(|k| matches!(k, NodeKind::Call)).collect();
        let alias_cfg = SpAnalyzer::new(SpOptions::call_blocking(alias_mode));
        for call_id in calls {
            // `None` means the convention passes no stack args.
            let Some(stack_args) = edit.function().get_cc(call_id).stack_args else {
                continue;
            };
            // Single-shot post-pass, so no changed/unchanged result to track.
            let args = collect_stack_args(edit.function(), call_id, stack_args, &alias_cfg);
            for arg_value in &args {
                edit.add_node_input(call_id, *arg_value)?;
            }
        }
        Ok(())
    }
}
