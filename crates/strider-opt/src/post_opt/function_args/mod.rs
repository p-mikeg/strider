//! Detects stack-passed function arguments and records them in
//! `SideTables::arg_index_to_values`.  Must run as a post-pass: it needs the
//! optimized memory graph.
//!
//! A candidate is a `Load[InitialVar(sp) + K]` with `K` in a stack slot and no
//! shadowing def on its memory chain.  Ordinals start at
//! `first_stack_arg = arg_passing_regs.len()`, and the first slot with no
//! anchored load ends the gap-free prefix.

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind};

use crate::error::Result;
use crate::mem_analysis::{MemAnalyzer, MemExpr, MemOptions};
use crate::mem_ssa::narrow_load_to;
use crate::pipeline::PostOptimizer;

#[derive(Clone)]
pub struct FunctionArgDetect;

impl PostOptimizer for FunctionArgDetect {
    fn apply(
        &self,
        edit: &mut crate::EditFunction<'_>,
        opt_ctx: &mut crate::OptCtx<'_>,
    ) -> Result<()> {
        let cc = edit.function().default_cc();
        let first_stack_arg = cc.arg_passing_regs.len();
        let maybe_stack_args = cc.stack_args;
        let Some(stack_args) = maybe_stack_args else {
            return Ok(());
        };
        let stack_global_disjoint = opt_ctx.options.assumptions.stack_global_disjoint;
        let alias_cfg = MemAnalyzer::new(MemOptions::incoming_args(
            stack_global_disjoint,
            &opt_ctx.options,
        ));
        // Narrowing rewires the graph, so it may only ever use what a
        // call-blocking walk proves; `alias_cfg` carries the relaxations and
        // decides detection alone.
        let narrow_cfg = MemAnalyzer::new(MemOptions::call_blocking(stack_global_disjoint));
        detect_stack_args(edit, &alias_cfg, &narrow_cfg, stack_args, first_stack_arg);
        Ok(())
    }
}

/// Groups qualifying loads by the byte-position slot their first byte occupies,
/// then walks slots from 0 assigning one ordinal per anchored argument.  A
/// wider-than-slot argument advances the cursor across every slot it spans but
/// the ordinal by one.
fn detect_stack_args(
    edit: &mut crate::EditFunction<'_>,
    alias_cfg: &MemAnalyzer,
    narrow_cfg: &MemAnalyzer,
    stack_args: strider_target::StackArgs,
    first_stack_arg: usize,
) {
    // Incoming stack args sit at fixed offsets from the ENTRY stack pointer, so
    // pinning `InitialVar(sp)` up front rejects a load rooted at a different SP
    // terminal even when its offset coincides with a convention slot.
    let Some(sp_node) = edit.function().initial_sp() else {
        return;
    };
    // `initial_sp` does not filter liveness; no live load is rooted at a dead SP.
    if !edit.is_live(sp_node) {
        return;
    }
    let [initial_sp] = edit
        .node_outputs_exact::<1>(sp_node)
        .expect("InitialVar has 1 output per node signature");
    // `span` records the furthest slot any load anchored at a start slot
    // reaches.
    let mut groups: rustc_hash::FxHashMap<usize, Vec<NodeId>> = rustc_hash::FxHashMap::default();
    let mut span: rustc_hash::FxHashMap<usize, usize> = rustc_hash::FxHashMap::default();
    let mut disqualified: rustc_hash::FxHashSet<usize> = rustc_hash::FxHashSet::default();
    let loads: Vec<NodeId> = edit
        .live_of_kind(|k| matches!(k, NodeKind::Load(_)))
        .collect();
    for node_id in loads {
        let addr = edit.load_addr(node_id);
        let [load_value] = edit
            .node_outputs_exact::<1>(node_id)
            .expect("Load has 1 output per node signature");
        let Some(load_ty) = edit.value_type_opt(load_value) else {
            continue;
        };
        let load_size = load_ty.byte_size() as i128;
        let Some(MemExpr { base, offset, .. }) = alias_cfg.decompose(edit.function(), addr) else {
            continue;
        };
        if base != initial_sp {
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
        if disqualified.contains(&start_slot) {
            continue;
        }
        let dirty = mem_chain_is_dirty(edit, alias_cfg, narrow_cfg, node_id);
        if dirty {
            disqualified.insert(start_slot);
            groups.remove(&start_slot);
            span.remove(&start_slot);
            continue;
        }
        groups.entry(start_slot).or_default().push(node_id);
        let reach = span.entry(start_slot).or_insert(start_slot);
        *reach = (*reach).max(end_slot);
    }

    // A disqualified slot is absent from `groups`, so it ends the prefix like a
    // genuine gap does.
    let mut cursor = 0usize;
    let mut ordinal = first_stack_arg;
    while groups.contains_key(&cursor) {
        let arg_span = span[&cursor] - cursor + 1;
        let index = ordinal as u32;
        let mut arg_loads: Vec<NodeId> = Vec::new();
        for s in cursor..cursor + arg_span {
            if let Some(loads) = groups.get(&s) {
                arg_loads.extend_from_slice(loads);
            }
        }
        // One argument's carriers must share a Load space.  A mismatch skips
        // registration but still consumes the ordinal.
        let first_load = *arg_loads
            .first()
            .expect("a present span entry always has >=1 anchored load");
        let NodeKind::Load(space) = *edit.node_kind(first_load) else {
            unreachable!("group members are seeded from Load nodes");
        };
        if arg_loads
            .iter()
            .all(|&l| matches!(*edit.node_kind(l), NodeKind::Load(s) if s == space))
        {
            for load in arg_loads {
                let [load_value] = edit
                    .node_outputs_exact::<1>(load)
                    .expect("Load has 1 output per node signature");
                edit.register_arg_value(index, load_value);
            }
        }
        cursor += arg_span;
        ordinal += 1;
    }
}

/// `true` when any path may overwrite bytes in the load's range, i.e. the
/// nearest clobber is anything but the clean `InitialMemory` root.
///
/// Narrows to `narrow_cfg`'s clobber, never `alias_cfg`'s: the rewire outlives
/// this pass, and a later run with the relaxations off would inherit an edge
/// that only holds with them on.
fn mem_chain_is_dirty(
    edit: &mut crate::EditFunction<'_>,
    alias_cfg: &MemAnalyzer,
    narrow_cfg: &MemAnalyzer,
    load: NodeId,
) -> bool {
    let mem_token = edit
        .memory_input_of(load)
        .expect("a Load has a memory input (slot 0)");
    let clobber = alias_cfg.nearest_clobber(edit.function(), load, mem_token);
    let sound = narrow_cfg.nearest_clobber(edit.function(), load, mem_token);
    narrow_load_to(edit, load, sound);
    !matches!(edit.node_kind(clobber), NodeKind::InitialMemory)
}

#[cfg(test)]
mod tests;
