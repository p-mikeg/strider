//! Stamps the side-table memory class with the slot `(base, K)` for every Store
//! / Load whose address decomposes to a single `base + K` terminal.  `base` is
//! SP-derived, or a listed allocator's return when `noalias_allocators` is
//! non-empty.
//!
//! `K` is only comparable against another access sharing the same `base`.

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind};

use crate::error::Result;
use crate::mem_analysis::decompose;
use crate::pipeline::PostOptimizer;

/// Annotates `Store` / `Load` offsets against their stack or heap base in the
/// side-table memory class.
#[derive(Clone)]
pub struct StackOffsetDetect;

impl PostOptimizer for StackOffsetDetect {
    fn apply(
        &self,
        edit: &mut crate::EditFunction<'_>,
        _ctx: &mut crate::OptCtx<'_>,
    ) -> Result<()> {
        stamp_all(edit);
        Ok(())
    }
}

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

#[cfg(test)]
mod tests;
