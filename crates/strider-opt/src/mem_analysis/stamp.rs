//! The per-access memory classes `strider-pattern`'s region filters read, and
//! the lifecycle of the memo that holds them:
//!
//! 1. Claims join the function's through [`Function::assume`], which drops the
//!    classes and the frame-escape verdict when one is new; a run starts with
//!    [`SideTables::invalidate_analyses`], which drops both.
//! 2. Everything decomposes under the function's claims
//!    ([`strider_ir::Function::assumptions`]), which only grow.
//! 3. [`SideTables::invalidate_analyses`] drops both and keeps the claims:
//!    after each pass or post-pass that changes the graph, and in
//!    [`refresh_memory_classes`].  Compaction drops the frame-escape verdict
//!    and remaps the classes.
//! 4. A converged run stamps every live access after its post-passes, and
//!    [`refresh_memory_classes`] restamps (`apply_rules_count` calls it). In
//!    between the table fills lazily.
//! 5. A raw reader (a region filter, `Function::stack_offset`) reads the table
//!    as it is, so a post-pass or an outside edit refreshes it first.
//!
//! [`Function::assume`]: strider_ir::Function::assume
//! [`SideTables::invalidate_analyses`]: strider_ir::SideTables::invalidate_analyses

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind};

use super::decompose::decompose;

/// Fills the decomposition memo for every live access under the claims the
/// function holds.  `decompose` memoizes its own verdict, so triggering it on
/// each address is enough; the stamping is a side effect.
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
/// read, under the claims the function holds
/// ([`strider_ir::Function::assumptions`]): every set
/// [`strider_ir::Function::assume`] added.  Call it after editing outside a
/// pipeline.
pub fn refresh_memory_classes(edit: &mut crate::EditFunction<'_>) {
    edit.clean();
    edit.function().side_tables().invalidate_analyses();
    stamp_all(edit);
}
