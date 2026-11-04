//! The per-access memory classes region filters read. They decompose under
//! the function's claims and fill lazily; [`Function::invalidate_analyses`]
//! drops them after each graph-changing pass and `run` stamps every live
//! access on exit. An outside edit calls [`refresh_memory_classes`] first.
//!
//! [`Function::invalidate_analyses`]: strider_ir::Function::invalidate_analyses

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind};

use super::decompose::decompose;

/// Fills the decomposition memo for every live access.
pub(crate) fn stamp_all(edit: &mut crate::EditFunction<'_>) {
    let candidates: Vec<NodeId> = edit
        .live_of_kind(|k| matches!(k, NodeKind::Store(_) | NodeKind::Load(_)))
        .collect();
    let function = edit.function();
    for node in candidates {
        if let Some(addr) = function.access_address(node) {
            decompose(function, addr);
        }
    }
}

/// Re-derives the per-access memory classes `stack_only()` / `heap_only()`
/// read. Call it after editing outside a pipeline.
pub fn refresh_memory_classes(edit: &mut crate::EditFunction<'_>) {
    edit.clean();
    edit.function().invalidate_analyses();
    stamp_all(edit);
}

#[cfg(test)]
#[path = "stamp_tests.rs"]
mod tests;
