//! `StackOffsetDetect` — stamps `Function::stack_offsets` with the
//! concrete SP-relative offset for every Store / Load whose address
//! decomposes to a single `sp + K` terminal.
//!
//! `function.stack_offset(node)` returns `Some(K)` for every Store /
//! Load whose address is unambiguously `sp + K`, and `None` for
//! everything else (Phi-of-offsets, non-SP-rooted addresses).

use strider_ir::Function;
use strider_ir::node::{NodeId, NodeKind};

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
        _entry: NodeId,
    ) -> Result<OptimizationResult> {
        let mut memo = SpExprMemo::default();
        let mut to_stamp: Vec<(NodeId, i64)> = Vec::new();

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
            if let Some(SpExpr::Terminal { offset, .. }) =
                decompose_sp(function, addr, self.stack_vn, &mut memo)
            {
                to_stamp.push((node, offset));
            }
        }

        if to_stamp.is_empty() {
            return Ok(OptimizationResult::NoChange);
        }
        for (node, offset) in to_stamp {
            function.set_stack_offset(node, offset);
        }
        Ok(OptimizationResult::Changed)
    }
}

#[cfg(test)]
mod tests;
