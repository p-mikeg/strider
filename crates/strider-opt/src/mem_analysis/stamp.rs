//! The per-access memory classes the pattern region filters (`stack_only` /
//! `heap_only` / `non_stack`) read. They decompose under the function's
//! claims and fill lazily; [`Function::invalidate_analyses`] drops them after
//! each graph-changing pass and `run` stamps every live access on exit. After
//! an edit outside a pipeline, call [`refresh_memory_classes`].
//!
//! [`Function::invalidate_analyses`]: strider_ir::Function::invalidate_analyses

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind};

use super::decompose::decompose;

/// Fills the decomposition memo for every live access.
pub(crate) fn stamp_all(edit: &crate::EditFunction<'_>) {
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

/// Re-derives the memory classes after an edit outside a pipeline.
pub fn refresh_memory_classes(edit: &mut crate::EditFunction<'_>) {
    edit.clean();
    edit.function().invalidate_analyses();
    stamp_all(edit);
}

#[cfg(test)]
#[path = "stamp_tests.rs"]
mod tests;
