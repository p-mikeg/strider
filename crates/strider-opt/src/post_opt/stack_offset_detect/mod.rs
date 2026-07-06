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
        // `decompose_readonly` walk (not a whole-graph sweep); the other
        // consumers call `decompose_readonly` directly.
        //
        // BOTH verdicts are committed: an SP-rooted address caches its
        // `(base, offset)`, a non-SP one caches the negative `NotStack`.  Since
        // `decompose_readonly` is read-only (its memory-SSA / range-scoped
        // callers hold `&Function`), this fill pass is the one place with `&mut`
        // to populate the cache, so caching the negatives here lets those later
        // read-only queries short-circuit instead of re-walking the spine.
        let candidates: Vec<NodeId> = edit
            .live_of_kind(|k| matches!(k, NodeKind::Store(_) | NodeKind::Load(_)))
            .collect();
        for node in candidates {
            // Address is input slot 1 of both Store/Load; skip a malformed node.
            let Some(addr) = edit.function().node_inputs(node).get(1).copied() else {
                continue;
            };
            let decomposed = decompose_readonly(edit.function(), addr);
            let tables = edit.function_mut().side_tables_mut();
            match decomposed {
                Some(SpExpr { base, offset }) => tables.set_stack_slot(addr, base, offset),
                None => tables.set_stack_slot_not(addr),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
