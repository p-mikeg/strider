//! For each `Call`, walks the memory-SSA chain to find the `Store` supplying
//! each positional stack-arg slot and appends the stored data as extra Call
//! inputs.

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::error::Result;
use crate::mem_analysis::{ArgStoreScan, MemAnalyzer, MemExpr, MemOptions, SlotReach};
use crate::pipeline::PostOptimizer;

#[cfg(test)]
mod tests;

/// The contiguous prefix of stack-arg slots `0..k`, stopping at the first slot
/// with no anchored store.  Offsets are relative to the CALL-TIME SP, and a
/// store wider than one slot is ONE argument spanning several slots.
///
/// The prefix over-collects: once lowered to memory, argument pushes are
/// indistinguishable from incidental in-window stack writes, so every plausible
/// store is collected.  A `Call` on the chain ends collection, since the callee
/// may overwrite the frame.
///
/// The window is open above: `k` is discovered, not given, so the scan probes
/// every slot from the first one upward.
fn collect_stack_args(
    function: &strider_ir::Function,
    call_id: NodeId,
    stack_args: strider_target::StackArgs,
    alias_cfg: &MemAnalyzer,
) -> Vec<ValueId> {
    // Call inputs are [control, memory, target, sp, ...args].
    let inputs = function.node_inputs(call_id);
    let mem_value = function
        .memory_input_of(call_id)
        .expect("Call carries a memory input (slot 1)");
    let sp_value = inputs[3];

    // A slot's entry-relative probe offset is `call_sp_off + offset_of(slot)`.
    // The probes below are stack-rooted, so a base in any other region is not
    // a coordinate system they share.
    let Some(MemExpr {
        base,
        offset: call_sp_off,
        kind: crate::mem_analysis::MemKind::Stack,
    }) = alias_cfg.decompose(function, sp_value)
    else {
        return Vec::new();
    };

    let mut scan = ArgStoreScan::new(
        alias_cfg.options().clone(),
        mem_value,
        base,
        call_sp_off + stack_args.offset_of(0),
        i128::MAX,
    );
    let mut args = Vec::new();
    let mut cursor = 0usize;
    loop {
        let slot_off = call_sp_off + stack_args.offset_of(cursor);
        // A slot reached by anything but a whole store of its own ends the
        // prefix: a covering store anchored earlier means the slot was never
        // written as a slot, a def the scan cannot see through leaves nothing
        // to collect, and an anchor a nearer store patched in part holds no
        // one value to collect.
        let SlotReach::Anchored { store, whole: true } = scan.reach_at(function, slot_off) else {
            break;
        };
        args.push(store.data(function));
        cursor += stack_args.slots_spanned(store.size(function));
    }
    args
}

/// The inputs a `Call` carries before any collection: `[control, memory,
/// target, sp]` plus one per register-passed argument, integer then float, the
/// layout the lifter builds.  Re-derived per run rather than remembered, so a
/// second run over the same `Function` replaces the first run's tail instead of
/// appending a duplicate of it.
fn register_arity(
    function: &strider_ir::Function,
    cc: &strider_target::BuiltCallingConvention,
) -> usize {
    const FIXED_INPUTS: usize = 4;
    let tracked = function.all_vns();
    // A `None` slot means the function was not lifted against this convention;
    // the lifter passes the prefix before the first one, so nothing after it is
    // an input either.
    let float_args = cc
        .float_arg_slots(tracked, |v| vn_container::largest_container_in(tracked, v))
        .into_iter()
        .take_while(Option::is_some)
        .count();
    FIXED_INPUTS + cc.arg_passing_regs.len() + float_args
}

/// Wires positional stack args into each `Call`.  A per-`Call` CC override
/// wins over the convention default.
#[derive(Clone)]
pub struct CallStackArgCollect;

impl PostOptimizer for CallStackArgCollect {
    fn apply(
        &self,
        edit: &mut crate::EditFunction<'_>,
        opt_ctx: &mut crate::OptCtx<'_>,
    ) -> Result<()> {
        let assumptions = &opt_ctx.options.assumptions;
        // The owned `Vec` lets the immutable borrow end before the mutation loop
        // takes `edit` mutably.
        let calls: Vec<NodeId> = edit.live_of_kind(|k| matches!(k, NodeKind::Call)).collect();
        let alias_cfg = MemAnalyzer::new(MemOptions::call_blocking(
            assumptions.stack_global_disjoint,
            &assumptions.noalias_allocators,
        ));
        // `float_arg_slots` scans the tracked set per float register, so the
        // unoverridden calls share one scan.
        let default_arity = register_arity(edit.function(), edit.function().default_cc());
        for call_id in calls {
            let (stack_args, arity) = {
                let function = edit.function();
                let cc = function.get_cc(call_id);
                let Some(stack_args) = cc.stack_args else {
                    continue;
                };
                // `get_cc` hands back the default itself when the call carries
                // no override.
                let arity = if std::ptr::eq(cc, function.default_cc()) {
                    default_arity
                } else {
                    register_arity(function, cc)
                };
                (stack_args, arity)
            };
            let args = collect_stack_args(edit.function(), call_id, stack_args, &alias_cfg);
            edit.truncate_node_inputs(call_id, arity);
            for arg_value in &args {
                edit.add_node_input(call_id, *arg_value)?;
            }
        }
        Ok(())
    }
}
