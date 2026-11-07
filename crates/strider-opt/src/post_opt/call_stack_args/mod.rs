use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueKind};

use crate::Result;
use crate::mem_analysis::{ArgPrefix, MemQuery, merged_slots};
use crate::pipeline::Optimizer;

#[cfg(test)]
mod tests;

/// The contiguous prefix of stack-arg slots the call's [`ArgPrefix`] anchors,
/// stopping at the first slot no whole store of its own reaches. A store
/// wider than one slot is one argument.
///
/// A slot holding the entry value of a register the function's own convention
/// preserves (a prologue save) ends it, as does a `Call` on the chain. Slots
/// past the prefix that every arm of a non-loop `MemPhi` stores whole continue
/// it as a `Phi` of the stored values, and past those a slot stored whole
/// before the branch continues it as its value.
fn collect_stack_args(
    edit: &mut crate::EditFunction<'_>,
    call_id: NodeId,
    increment: i128,
    query: &MemQuery,
    saved: &[ValueId],
) -> Vec<ValueId> {
    let function = edit.function();
    let Some(prefix) = ArgPrefix::scan(function, query, call_id) else {
        return Vec::new();
    };
    let big_endian = function.byte_order().data == strider_target::Endianness::Big;
    // An anchor a nearer store patched in part holds no one value to collect,
    // and a store inside the slot is its argument only right-justified on
    // big-endian.
    let mut args: Vec<ValueId> = prefix
        .slots()
        .iter()
        .map_while(|slot| {
            let data = slot.store.data(function);
            (slot.whole
                && !saved.contains(&data)
                && (slot.span == slot.store.size(function)
                    || (big_endian && slot.span == increment)))
                .then_some(data)
        })
        .collect();
    if args.len() < prefix.slots().len() {
        return args;
    }
    for (mem_phi, stores) in merged_slots(function, query, call_id, &prefix) {
        let function = edit.function();
        let data: Vec<ValueId> = stores.iter().map(|hit| hit.data(function)).collect();
        let Some(mem_phi) = mem_phi else {
            if saved.contains(&data[0]) {
                break;
            }
            args.push(data[0]);
            continue;
        };
        let region = function.phi_region(mem_phi);
        // A loop header's arms are a local's iterations, not one call's paths.
        let header = {
            let doms = edit.control_dominator_tree();
            function
                .node_inputs(region)
                .into_iter()
                .any(|edge| doms.dominates(region, function.producer(edge)))
        };
        if header || data.iter().any(|v| saved.contains(v)) {
            break;
        }
        let ty = function.value_type(data[0]).expect("store data is typed");
        let token = function.node_inputs(mem_phi)[NodeKind::PHI_TOKEN_SLOT];
        let phi = edit.create_node(
            NodeKind::Phi,
            std::iter::once(token).chain(data),
            [ValueKind::Typed(ty)],
        );
        args.push(edit.function().node_outputs(phi)[0]);
    }
    args
}

/// The entry values of the registers the function's own convention preserves.
fn callee_saved_entry_values(function: &strider_ir::Function) -> Vec<ValueId> {
    let tracked = function.all_vns();
    function
        .default_cc()
        .callee_saved_regs
        .iter()
        .filter_map(|r| function.initial_var_value(&vn_container::largest_container_in(tracked, r)))
        .collect()
}

/// The inputs a `Call` carries before any collection: the head plus one per
/// register-passed argument, integer then float. A second run truncates to it.
fn register_arity(
    function: &strider_ir::Function,
    cc: &strider_target::BuiltCallingConvention,
) -> usize {
    let tracked = function.all_vns();
    let float_args = cc
        .float_arg_prefix(tracked, |v| vn_container::largest_container_in(tracked, v))
        .len();
    NodeKind::Call { cc: None }.input_head_len() + cc.arg_passing_regs.len() + float_args
}

/// Wires positional stack args into each `Call`. A per-`Call` CC override
/// wins over the convention default; a CC without `stack_args` collects none.
/// Collection stops at the first unwritten slot, so arguments past a gap (an
/// alignment hole, a ppc64 slot whose FP argument travels in an FPR) are
/// missed. Other writes to the slots past the last argument are collected
/// too: a local, an alignment push (x86-64 clang's `push rax`, Thumb gcc's
/// `push {r3, lr}`), or o32's `gp` save just above the outgoing arguments
/// (`sp+16` when there are none), which the IR cannot tell from an argument
/// without the call-time `gp`.
pub struct CallStackArgCollect;

impl Optimizer for CallStackArgCollect {
    fn is_post_pass(&self) -> bool {
        true
    }

    fn apply(
        &self,
        edit: &mut crate::EditFunction<'_>,
        _opt_ctx: &crate::OptCtx<'_>,
    ) -> Result<()> {
        let calls: Vec<NodeId> = edit
            .live_of_kind(|k| matches!(k, NodeKind::Call { .. }))
            .collect();
        let query = MemQuery::default();
        // Shared by every call without an override.
        let default_arity = register_arity(edit.function(), edit.function().default_cc());
        let saved = callee_saved_entry_values(edit.function());
        for call_id in calls {
            let (stack_args, arity) = {
                let function = edit.function();
                let cc = function.get_cc(call_id);
                let Some(stack_args) = cc.stack_args else {
                    continue;
                };
                let arity = if std::ptr::eq(cc, function.default_cc()) {
                    default_arity
                } else {
                    register_arity(function, cc)
                };
                (stack_args, arity)
            };
            let args = collect_stack_args(edit, call_id, stack_args.increment, &query, &saved);
            edit.truncate_node_inputs(call_id, arity);
            for arg_value in &args {
                edit.add_node_input(call_id, *arg_value);
            }
        }
        Ok(())
    }
}
