//! The per-access memory classes `strider-pattern`'s region filters read, and
//! the lifecycle of the memo that holds them:
//!
//! 1. A run starts with [`SideTables::reset_analyses`], the one writer of the
//!    recorded allocator set, which drops the classes and the frame-escape
//!    verdict.
//! 2. Everything the run decomposes, it decomposes under that set.
//! 3. [`SideTables::invalidate_analyses`] drops both and keeps the set: after
//!    each changing in-loop pass, in [`refresh_memory_classes`], and in
//!    strider-py's in-loop pass run as a post-pass when it changes the graph.
//!    After any other post-pass only the frame-escape verdict is dropped;
//!    compaction drops it too and remaps the classes.
//! 4. A converged run stamps every live access after its post-passes, and
//!    [`refresh_memory_classes`] restamps (`apply_rules_count` calls it). In
//!    between the table fills lazily.
//! 5. A reader outside the run (`classify_sites`, `classify_target`,
//!    `ReturnTargets::new`, `run_post`, or a built-in pass's `apply`)
//!    must be handed the recorded set; another debug-panics.
//! 6. A raw reader (a region filter, `Function::stack_offset`) reads the table
//!    as it is, so a post-pass or an outside edit refreshes it first.
//!
//! [`SideTables::reset_analyses`]: strider_ir::SideTables::reset_analyses
//! [`SideTables::invalidate_analyses`]: strider_ir::SideTables::invalidate_analyses

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
/// read, under the recorded allocator set
/// ([`strider_ir::SideTables::memory_allocators`]): the one the function was
/// last optimized with, empty if it never was.  Call it after editing outside
/// a pipeline.
pub fn refresh_memory_classes(edit: &mut crate::EditFunction<'_>) {
    edit.clean();
    edit.function().side_tables().invalidate_analyses();
    stamp_all(edit);
}
