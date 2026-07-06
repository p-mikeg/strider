//! `StackOffsetDetect` — stamps `Function::stack_offsets` with the stack
//! slot `(base, K)` for every Store / Load whose address decomposes to a
//! single `base + K` terminal, where `base` is the SP-derived terminal node
//! (`InitialVar(sp)` or an alignment-masked `sp & mask`).
//!
//! `function.stack_offset(node)` returns `Some((base, K))` for every Store /
//! Load whose address is unambiguously `base + K`, and `None` for everything
//! else (Phi-of-offsets, non-SP-rooted addresses).  The offset `K` is only
//! comparable against another access sharing the same `base`.

use strider_ir::node::{NodeId, NodeKind};
use strider_ir::IRViewer;

use crate::error::Result;
use crate::pipeline::PostOptimizer;
use crate::sp_expr::{decompose_readonly, SpExpr};

/// Detects SP-relative Store / Load addresses and records each one's
/// concrete offset in the `Function::stack_offsets` side-table.
///
/// The stack-pointer varnode is read from the function's own calling
/// convention (`Function::default_cc`) at apply time — the function is the
/// single source of truth, so the pass carries no convention state.
#[derive(Clone)]
pub struct StackOffsetDetect;

impl PostOptimizer for StackOffsetDetect {
    fn apply(&self, edit: &mut crate::EditFunction<'_>, _ctx: &mut crate::OptCtx<'_>) -> Result<()> {
        // Fills the `stack_offsets` cache for the STORE/LOAD ADDRESSES only — the
        // sparse set the user-facing per-node `Function::stack_offset` reads back
        // on the frozen, post-convergence graph.  Each address is an O(spine)
        // `decompose_readonly` walk (not a whole-graph sweep): the other
        // consumers (the memory-SSA walk, the indirect-branch classifier /
        // evaluator) call `decompose_readonly` directly, which is already cheap,
        // so there is no need to eagerly decompose every value.
        let candidates: Vec<NodeId> = edit
            .live_of_kind(|k| matches!(k, NodeKind::Store(_) | NodeKind::Load(_)))
            .collect();
        for node in candidates {
            // Address is input slot 1 of both Store/Load; skip a malformed node.
            let Some(addr) = edit.function().node_inputs(node).get(1).copied() else {
                continue;
            };
            if let Some(SpExpr { base, offset }) = decompose_readonly(edit.function(), addr) {
                edit.function_mut()
                    .side_tables_mut()
                    .set_stack_slot(addr, base, offset);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
