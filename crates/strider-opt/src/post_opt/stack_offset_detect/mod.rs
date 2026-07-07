//! `StackOffsetDetect` — stamps `Function::stack_offsets` with the stack
//! slot `(base, K)` for every Store / Load whose address decomposes to a
//! single `base + K` terminal, where `base` is the SP-derived terminal node
//! (`InitialVar(sp)` or an alignment-masked `sp & mask`).
//!
//! `function.stack_offset(node)` returns `Some((base, K))` for every Store /
//! Load whose address is unambiguously `base + K`, and `None` for everything
//! else (Phi-of-offsets, non-SP-rooted addresses).  The offset `K` is only
//! comparable against another access sharing the same `base`.

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind};

use crate::error::Result;
use crate::pipeline::PostOptimizer;
use crate::sp_expr::decompose_readonly;

/// Detects SP-relative Store / Load addresses and records each one's
/// concrete offset in the `Function::stack_offsets` side-table.
///
/// The stack-pointer varnode is read from the function's own calling
/// convention (`Function::default_cc`) at apply time — the function is the
/// single source of truth, so the pass carries no convention state.
#[derive(Clone)]
pub struct StackOffsetDetect;

impl PostOptimizer for StackOffsetDetect {
    fn apply(
        &self,
        edit: &mut crate::EditFunction<'_>,
        _ctx: &mut crate::OptCtx<'_>,
    ) -> Result<()> {
        // Ensures the `stack_offsets` cache is populated for every STORE/LOAD
        // ADDRESS on the frozen, post-convergence graph — the sparse set the
        // user-facing per-node `Function::stack_offset` reads back.  `decompose`
        // now memoizes each verdict itself (into the RefCell-backed cache), so
        // this pass just has to *trigger* a decompose on each address; the
        // positive and negative verdicts land in the cache as a side effect.
        let candidates: Vec<NodeId> = edit
            .live_of_kind(|k| matches!(k, NodeKind::Store(_) | NodeKind::Load(_)))
            .collect();
        let function = edit.function();
        for node in candidates {
            // Address is input slot 1 of both Store/Load; skip a malformed node.
            if let Some(addr) = function.node_inputs(node).get(1).copied() {
                decompose_readonly(function, addr);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
