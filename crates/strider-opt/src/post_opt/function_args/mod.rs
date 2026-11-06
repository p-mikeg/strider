use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::Result;
use crate::mem_analysis::{MemExpr, MemQuery, Policy, ReachingDef};
use crate::pipeline::Optimizer;

/// Registers the function's stack-passed arguments as argument values.
///
/// A candidate is an unshadowed `Load[InitialVar(sp) + K]` with `K` in a stack
/// slot. Ordinals start at `arg_passing_regs.len()` whatever the argument's
/// class, so an integer-arg query can bind a stack-passed float. Only slots
/// up to the first one no clean load reads are arguments.
pub struct FunctionArgDetect;

impl Optimizer for FunctionArgDetect {
    fn is_post_pass(&self) -> bool {
        true
    }

    fn apply(
        &self,
        edit: &mut crate::EditFunction<'_>,
        _opt_ctx: &crate::OptCtx<'_>,
    ) -> Result<()> {
        let cc = edit.function().default_cc();
        let first_stack_arg = cc.arg_passing_regs.len();
        let Some(stack_args) = cc.stack_args else {
            return Ok(());
        };
        // Re-detect from scratch; the lifter's register carriers sit below
        // `first_stack_arg`.
        edit.clear_arg_values_from(first_stack_arg as u32);
        let policy = Policy::incoming_args(edit.function());
        detect_stack_args(
            edit,
            &MemQuery::default(),
            policy,
            stack_args,
            first_stack_arg,
        );
        Ok(())
    }
}

/// Walks slots from 0 assigning one ordinal per anchored argument, spanning
/// the widest load anchored at the cursor, so adjacent arguments read by one
/// wide and one narrow load merge into one ordinal. The first slot with no
/// anchored load ends the prefix, as does a dirty load reaching past its
/// argument (after that argument).
fn detect_stack_args(
    edit: &mut crate::EditFunction<'_>,
    query: &MemQuery,
    policy: Policy,
    stack_args: strider_target::StackArgs,
    first_stack_arg: usize,
) {
    // Per start slot: the furthest slot reached, and the clean loads with
    // their kind.
    let mut groups: rustc_hash::FxHashMap<usize, (usize, Vec<(NodeKind, ValueId)>)> =
        rustc_hash::FxHashMap::default();
    let mut dirty_reach: rustc_hash::FxHashMap<usize, usize> = rustc_hash::FxHashMap::default();
    let loads: Vec<NodeId> = edit
        .live_of_kind(|k| matches!(k, NodeKind::Load(_)))
        .collect();
    for node_id in loads {
        let addr = edit.load_addr(node_id);
        let (load_value, load_ty) = edit
            .single_value_output(node_id)
            .expect("Load has 1 typed output per node signature");
        let load_size = load_ty.byte_size() as i128;
        let Some(MemExpr { base, offset, .. }) =
            crate::mem_analysis::decompose(edit.function(), addr)
        else {
            continue;
        };
        if !edit.function().is_entry_sp(base) {
            continue;
        }
        let Some(start_slot) = stack_args.slot_of(offset) else {
            continue;
        };
        let Some(last_byte) = offset.checked_add(load_size).and_then(|e| e.checked_sub(1)) else {
            continue;
        };
        let Some(end_slot) = stack_args.slot_of(last_byte) else {
            continue;
        };
        // A load some def may have overwritten reads no incoming value. The
        // query narrows the load's edge as it answers.
        if !matches!(
            query.reach_and_narrow(edit, node_id, policy),
            ReachingDef::Clean(_)
        ) {
            let reach = dirty_reach.entry(start_slot).or_insert(start_slot);
            *reach = (*reach).max(end_slot);
            continue;
        }
        let (reach, loads) = groups.entry(start_slot).or_insert((start_slot, Vec::new()));
        *reach = (*reach).max(end_slot);
        loads.push((*edit.node_kind(node_id), load_value));
    }
    let mut cursor = 0usize;
    let mut ordinal = first_stack_arg as u32;
    while let Some(&(reach, _)) = groups.get(&cursor) {
        let arg_loads: Vec<(NodeKind, ValueId)> = (cursor..=reach)
            .filter_map(|s| groups.get(&s))
            .flat_map(|(_, loads)| loads.iter().copied())
            .collect();
        // A Load-space mismatch skips registration but consumes the ordinal.
        if arg_loads.iter().all(|&(kind, _)| kind == arg_loads[0].0) {
            for (_, load_value) in arg_loads {
                edit.register_arg_value(ordinal, load_value);
            }
        }
        if (cursor..=reach).any(|s| dirty_reach.get(&s).is_some_and(|&d| d > reach)) {
            break;
        }
        cursor = reach + 1;
        ordinal += 1;
    }
}

#[cfg(test)]
mod tests;
