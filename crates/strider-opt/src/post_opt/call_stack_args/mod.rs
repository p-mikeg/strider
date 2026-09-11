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
        alias_cfg.options(),
        mem_value,
        base,
        call_sp_off + stack_args.offset_of(0),
        i128::MAX,
    );
    let mut args = Vec::new();
    let mut cursor = 0usize;
    loop {
        let slot_off = call_sp_off + stack_args.offset_of(cursor);
        // A slot reached by anything but a store of its own ends the prefix:
        // a covering store anchored earlier means the slot was never written
        // as a slot, and a def the scan cannot see through leaves nothing to
        // collect.
        let SlotReach::Anchored(store) = scan.reach_at(function, slot_off) else {
            break;
        };
        args.push(store.data(function));
        cursor += stack_args.slots_spanned(store.size(function));
    }
    args
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
        let alias_mode = opt_ctx.options.alias_mode;
        // The owned `Vec` lets the immutable borrow end before the mutation loop
        // takes `edit` mutably.
        let calls: Vec<NodeId> = edit.live_of_kind(|k| matches!(k, NodeKind::Call)).collect();
        let alias_cfg = MemAnalyzer::new(MemOptions::call_blocking(alias_mode));
        for call_id in calls {
            let Some(stack_args) = edit.function().get_cc(call_id).stack_args else {
                continue;
            };
            let args = collect_stack_args(edit.function(), call_id, stack_args, &alias_cfg);
            for arg_value in &args {
                edit.add_node_input(call_id, *arg_value)?;
            }
        }
        Ok(())
    }
}
