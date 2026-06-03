//! `StackOffsetDetect` — stamps `Function::stack_offsets` with the stack
//! slot `(base, K)` for every Store / Load whose address decomposes to a
//! single `base + K` terminal, where `base` is the SP-derived terminal node
//! (`InitialVar(sp)` or an alignment-masked `sp & mask`).
//!
//! `function.stack_offset(node)` returns `Some((base, K))` for every Store /
//! Load whose address is unambiguously `base + K`, and `None` for everything
//! else (Phi-of-offsets, non-SP-rooted addresses).  The offset `K` is only
//! comparable against another access sharing the same `base`.

use strider_ir::Function;
use strider_ir::node::{NodeId, NodeKind};

use crate::error::Result;
use crate::pipeline::{OptimizationResult, Optimizer};
use crate::sp_expr::{SpExpr, SpExprMemo, decompose_sp};

/// Detects SP-relative Store / Load addresses and records each one's
/// concrete offset in the `Function::stack_offsets` side-table.
///
/// The stack-pointer varnode is read from the function's own calling
/// convention (`Function::default_cc`) at apply time — the function is the
/// single source of truth, so the pass carries no convention state.
#[derive(Clone, Default)]
pub struct StackOffsetDetect;

impl StackOffsetDetect {
    /// Creates the pass.  Carries no state; the stack pointer is read from
    /// the function under analysis.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Optimizer for StackOffsetDetect {
    fn apply(
        &self,
        rctx: &mut crate::RewriteCtx<'_>,
        _ctx: &crate::OptCtx<'_>,
    ) -> Result<OptimizationResult> {
        let mut memo = SpExprMemo::default();
        let stack_vn = rctx.function_ref().default_cc().stack_vn;

        // Snapshot the Store/Load nodes in global reverse-post-order.  The
        // reachable SET is identical to `walk()`; only the ORDER is
        // canonicalised.  The owned `Vec` lets the immutable RPO borrow end
        // before the per-node loop re-borrows `rctx` (immutably to decompose,
        // mutably to stamp).
        let candidates: Vec<NodeId> = rctx
            .function_ref()
            .rpo_filter(|k| matches!(k, NodeKind::Store(_) | NodeKind::Load(_)))
            .collect();

        let mut changed = false;
        for node in candidates {
            let function: &Function = rctx.function_ref();
            // Skip nodes whose offset is already known — keeps the
            // pass idempotent inside the fixed-point loop.
            if function.stack_offset(node).is_some() {
                continue;
            }
            // `node` came from the `Store`/`Load`-seeded RPO filter, so it has
            // ≥2 inputs (validated arity for both shapes, [mem, addr, data] /
            // [mem, addr]); the address is slot 1 in either.
            let addr = function
                .graph()
                .nth_input(node, 1)
                .expect("Store/Load carries an address in input slot 1");
            // `decompose_sp` returns a `Terminal` only for genuinely
            // SP-rooted addresses: `InitialVar(sp)` OR an alignment-masked
            // `sp & mask` (the And arm guards against `And(rax, mask)` and the
            // like).  So any base it yields is a real stack base.  The offset
            // is only comparable against another access that shares the same
            // base (different SP bases, e.g. entry-SP vs an aligned SP, differ
            // by the caller-dependent `sp mod align`).
            let Some(SpExpr { base, offset }) = decompose_sp(function, addr, stack_vn, &mut memo)
            else {
                continue;
            };
            // The immutable `function` borrow ends here, freeing `rctx` for
            // the stamping mutation.
            rctx.set_stack_offset(node, base, offset);
            changed = true;
        }

        Ok(if changed {
            OptimizationResult::Changed
        } else {
            OptimizationResult::NoChange
        })
    }
}

#[cfg(test)]
mod tests;
