//! Stamps `Function::stack_offsets` with the slot `(base, K)` for every Store
//! / Load whose address decomposes to a single SP-derived `base + K` terminal.
//! Everything else (Phi-of-offsets, non-SP-rooted addresses) records nothing.
//!
//! `K` is only comparable against another access sharing the same `base`.

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind};

use crate::error::Result;
use crate::pipeline::PostOptimizer;
use crate::sp_analysis::decompose;

/// The stack-pointer varnode is read from the function's own calling
/// convention at apply time, so the pass carries no convention state.
#[derive(Clone)]
pub struct StackOffsetDetect;

impl PostOptimizer for StackOffsetDetect {
    fn apply(
        &self,
        edit: &mut crate::EditFunction<'_>,
        _ctx: &mut crate::OptCtx<'_>,
    ) -> Result<()> {
        // `decompose` memoizes its own verdict, so triggering it on each
        // address is enough: both positive and negative results land in the
        // cache as a side effect.
        let candidates: Vec<NodeId> = edit
            .live_of_kind(|k| matches!(k, NodeKind::Store(_) | NodeKind::Load(_)))
            .collect();
        let function = edit.function();
        for node in candidates {
            // Slot 1 on both Store and Load; skip a malformed node.
            if let Some(addr) = function.node_inputs(node).get(1).copied() {
                decompose(function, addr);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
