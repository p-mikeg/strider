//! Detects stack-passed function arguments and records them in
//! `FuncSideTables::arg_index_to_values`.  Must run as a post-pass: it needs the
//! optimized memory graph.
//!
//! A candidate is a `Load[InitialVar(sp) + K]` with `K` in a stack slot and no
//! shadowing def on its memory chain.  Ordinals start at
//! `first_stack_arg = arg_passing_regs.len()`, and the first slot with no
//! anchored load ends the gap-free prefix, as does an argument some dirty
//! load reaches past.

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::Result;
use crate::mem_analysis::{MemExpr, MemQuery, Policy, ReachingDef};
use crate::pipeline::Optimizer;

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
        // A second run over the same `Function` re-detects from scratch; the
        // register carriers the lifter recorded sit below `first_stack_arg`.
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

/// Groups qualifying loads by the byte-position slot their first byte occupies,
/// then walks slots from 0 assigning one ordinal per anchored argument.  A
/// wider-than-slot argument advances the cursor across every slot it spans but
/// the ordinal by one.
///
/// The span comes from the widest load anchored at the cursor, so two adjacent
/// arguments read by one wide load and one narrow load (a 16-byte load at slot
/// 0 plus an 8-byte load at slot 1 on x86-64) merge into ONE ordinal and shift
/// every later index down by one.  Separating them needs the callee's
/// signature; unlike [`CallStackArgCollect`](crate::CallStackArgCollect)'s
/// over-collection, the failure mode is a wrong index, not a surplus one.
///
/// A dirty load's span says nothing of where the arguments lie: it may read
/// the function's own wide store.  One reaching past the argument it starts
/// in ends the prefix after that argument.
fn detect_stack_args(
    edit: &mut crate::EditFunction<'_>,
    query: &MemQuery,
    policy: Policy,
    stack_args: strider_target::StackArgs,
    first_stack_arg: usize,
) {
    // Per start slot: the furthest slot a load anchored there reaches, and
    // the clean ones among those loads, by space.
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
        // Incoming stack args sit at fixed offsets from the ENTRY stack
        // pointer, so a load rooted at a different SP terminal is none, even
        // when its offset coincides with a convention slot.
        if !edit.function().is_entry_sp(base) {
            continue;
        }
        let Some(start_slot) = stack_args.slot_of(offset) else {
            continue;
        };
        // A pathological offset/size can overflow here; treat that as "not a
        // stack arg" rather than panicking.
        let Some(last_byte) = offset.checked_add(load_size).and_then(|e| e.checked_sub(1)) else {
            continue;
        };
        let Some(end_slot) = stack_args.slot_of(last_byte) else {
            continue;
        };
        // A load some def may have written over reads no incoming value; the
        // slot's clean loads still do.  The query narrows the load's edge as
        // it answers.
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
    // A slot with no clean load ends the prefix like a genuine gap does.
    let mut cursor = 0usize;
    let mut ordinal = first_stack_arg as u32;
    while let Some(&(reach, _)) = groups.get(&cursor) {
        let arg_loads: Vec<(NodeKind, ValueId)> = (cursor..=reach)
            .filter_map(|s| groups.get(&s))
            .flat_map(|(_, loads)| loads.iter().copied())
            .collect();
        // One argument's carriers must share a Load space.  A mismatch skips
        // registration but still consumes the ordinal.
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
