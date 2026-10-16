//! The per-access memory classes `strider-pattern`'s region filters read.

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind};

use super::decompose::decompose;

/// Fills the decomposition memo for every live access under the recorded
/// allocator set.  `decompose` memoizes its own verdict, so triggering it on
/// each address is enough; the stamping is a side effect.
pub(crate) fn stamp_all(edit: &mut crate::EditFunction<'_>) {
    let candidates: Vec<NodeId> = edit
        .live_of_kind(|k| matches!(k, NodeKind::Store(_) | NodeKind::Load(_)))
        .collect();
    let function = edit.function();
    let allocators = function.side_tables().memory_allocators();
    for node in candidates {
        // Slot 1 on both Store and Load; skip a malformed node.
        if let Some(addr) = function.node_inputs(node).get(1).copied() {
            decompose(function, addr, allocators);
        }
    }
}

/// Re-derives the per-access memory classes `stack_only()` / `heap_only()`
/// read, under the allocator set the function was last optimized with.  Call
/// it after editing outside a pipeline.
pub fn refresh_memory_classes(edit: &mut crate::EditFunction<'_>) {
    edit.clean();
    edit.function().side_tables().invalidate_analyses();
    stamp_all(edit);
}
