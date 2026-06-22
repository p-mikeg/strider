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
use crate::pipeline::PostOptimizer;
use crate::sp_expr::{SpDecomposer, SpExpr};

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
        ctx: &mut crate::OptCtx<'_>,
    ) -> Result<()> {
        let memo = &mut ctx.sp_memo;

        // Snapshot the live Store/Load nodes.  Each access is decomposed and
        // stamped INDEPENDENTLY into the `stack_offsets` side-table (a pure
        // side-table write, no graph-structure change), so processing order
        // does not affect the outcome — iterate the cached live set directly
        // (`live_of_kind`, no graph walk).  The owned `Vec` lets the immutable
        // borrow end before the per-node loop re-borrows `edit` (immutably to
        // decompose, mutably to stamp).
        let candidates: Vec<NodeId> = edit
            .live_of_kind(|k| matches!(k, NodeKind::Store(_) | NodeKind::Load(_)))
            .collect();

        for node in candidates {
            let function: &Function = edit.function();
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
            let Some(SpExpr { base, offset }) =
                SpDecomposer::new(function, memo).decompose(addr)
            else {
                continue;
            };
            // The immutable `function` borrow ends here, freeing `edit` for
            // the stamping mutation.
            edit.function_mut().set_stack_offset(node, base, offset);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests;
