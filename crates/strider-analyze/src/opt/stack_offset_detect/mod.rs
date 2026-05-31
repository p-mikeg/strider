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
use strider_ir::node::{NodeId, NodeKind, NodeOutputId};

use crate::opt::error::Result;
use crate::opt::pipeline::{OptimizationResult, Optimizer};
use crate::opt::sp_expr::{SpExpr, SpExprMemo, decompose_sp};

/// Detects SP-relative Store / Load addresses and records each one's
/// concrete offset in the `Function::stack_offsets` side-table.
#[derive(Clone)]
pub struct StackOffsetDetect {
    /// Stack-pointer varnode used by `decompose_sp` to recognise
    /// SP-relative addresses.
    stack_vn: rsleigh::Vn,
}

impl StackOffsetDetect {
    /// Convenience constructor for tests.
    #[must_use]
    pub const fn new(stack_vn: rsleigh::Vn) -> Self {
        Self { stack_vn }
    }

    /// Production constructor — takes the stack-pointer varnode from
    /// the supplied calling convention.
    #[must_use]
    pub const fn from_convention(cc: &strider_target::BuiltCallingConvention) -> Self {
        Self {
            stack_vn: cc.stack_vn,
        }
    }
}

impl Optimizer for StackOffsetDetect {
    fn optimize(
        &self,
        function: &mut Function,
    ) -> Result<OptimizationResult> {
        let mut memo = SpExprMemo::default();
        let mut to_stamp: Vec<(NodeId, NodeOutputId, i64)> = Vec::new();

        for node in function.walk() {
            // Skip nodes whose offset is already known — keeps the
            // pass idempotent inside the fixed-point loop.
            if function.stack_offset(node).is_some() {
                continue;
            }
            let addr = match *function.node_kind(node) {
                NodeKind::Store(_) => function.node_inputs_exact::<3>(node)?[1],
                NodeKind::Load(_) => function.node_inputs_exact::<2>(node)?[1],
                _ => continue,
            };
            // `decompose_sp` returns a `Terminal` only for genuinely
            // SP-rooted addresses: `InitialVar(sp)` OR an alignment-masked
            // `sp & mask` (the And arm guards against `And(rax, mask)` and the
            // like).  So any base it yields is a real stack base.  Record
            // `(base, offset)` — the offset is only comparable against another
            // access that shares the same base (different SP bases, e.g.
            // entry-SP vs an aligned SP, differ by the caller-dependent
            // `sp mod align`).
            if let Some(SpExpr::Terminal { base, offset }) =
                decompose_sp(function, addr, self.stack_vn, &mut memo)
            {
                to_stamp.push((node, base, offset));
            }
        }

        if to_stamp.is_empty() {
            return Ok(OptimizationResult::NoChange);
        }
        for (node, base, offset) in to_stamp {
            function.set_stack_offset(node, base, offset);
        }
        Ok(OptimizationResult::Changed)
    }
}

#[cfg(test)]
mod tests;
