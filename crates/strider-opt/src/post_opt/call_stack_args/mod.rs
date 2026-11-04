use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::Result;
use crate::mem_analysis::{ArgPrefix, MemQuery};
use crate::pipeline::Optimizer;

#[cfg(test)]
mod tests;

/// The contiguous prefix of stack-arg slots the call's [`ArgPrefix`] anchors,
/// stopping at the first slot no whole store of its own reaches. Offsets are
/// relative to the call-time SP; a store wider than one slot is one argument.
///
/// Over-collects: argument pushes are indistinguishable from incidental stack
/// writes. A `Call` on the chain ends collection.
fn collect_stack_args(
    function: &strider_ir::Function,
    call_id: NodeId,
    increment: i128,
    query: &MemQuery,
) -> Vec<ValueId> {
    let Some(prefix) = ArgPrefix::scan(function, query, call_id) else {
        return Vec::new();
    };
    let big_endian = function.byte_order().data == strider_target::Endianness::Big;
    // An anchor a nearer store patched in part holds no one value to collect,
    // and a store inside the slot is its argument only right-justified on
    // big-endian.
    prefix
        .slots()
        .iter()
        .map_while(|slot| {
            (slot.whole
                && (slot.span == slot.store.size(function)
                    || (big_endian && slot.span == increment)))
                .then(|| slot.store.data(function))
        })
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

/// Wires positional stack args into each `Call`.  A per-`Call` CC override
/// wins over the convention default.
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
            let args = collect_stack_args(edit.function(), call_id, stack_args.increment, &query);
            edit.truncate_node_inputs(call_id, arity);
            for arg_value in &args {
                edit.add_node_input(call_id, *arg_value);
            }
        }
        Ok(())
    }
}
