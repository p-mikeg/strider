//! `StackOffsetDetect` — stamps `Function::stack_offsets` with the stack
//! slot `(base, K)` for every Store / Load whose address decomposes to a
//! single `base + K` terminal, where `base` is the SP-derived terminal node
//! (`InitialVar(sp)` or an alignment-masked `sp & mask`).
//!
//! `function.stack_offset(node)` returns `Some((base, K))` for every Store /
//! Load whose address is unambiguously `base + K`, and `None` for everything
//! else (Phi-of-offsets, non-SP-rooted addresses).  The offset `K` is only
//! comparable against another access sharing the same `base`.

use crate::error::Result;
use crate::pipeline::PostOptimizer;
use crate::sp_expr::decompose_fill_all;

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
        // This is the FILL pass for the `stack_offsets` decomposition cache: on
        // the frozen, post-convergence graph it decomposes EVERY value in one
        // O(graph) defs-before-uses sweep and commits the verdicts (positives
        // and negatives).  After it, the value-keyed cache is populated for the
        // per-node user-facing `Function::stack_offset` and every read-only
        // consumer (the memory-SSA walk, the indirect-branch classifier /
        // evaluator) — each an O(1) cache hit rather than a per-query cone walk.
        decompose_fill_all(edit.function_mut());
        Ok(())
    }
}

#[cfg(test)]
mod tests;
