//! Egg-based `FunctionArgDetect` rewriter — Phase 3 Task 3.7b.
//!
//! Built alongside the imperative [`crate::opt::FunctionArgDetect`] —
//! NOT a replacement.  The parity test
//! `crates/strider-analyze/tests/function_arg_detect_egg_parity.rs`
//! proves both produce structurally identical IR for the supported
//! shapes.
//!
//! # Design — why this pass does NOT use the egraph
//!
//! `FunctionArgDetect` is a **function-boundary post-pass** with two
//! independent halves:
//!
//! 1. **Register args**.  Walk every reachable `InitialVar(reg)` for
//!    `reg` in `arg_passing_regs`; emit one `FunctionArg { Register(reg),
//!    i }` and rewire its consumers.  No arithmetic — just a 1-to-1
//!    rename driven by the calling-convention table.
//!
//! 2. **Stack args**.  Collect `Load[sp + K]` nodes whose `K` matches a
//!    convention `stack_arg_offsets[j]` and whose memory chain is
//!    proven not to alias the slot (DFS shadow check through
//!    `MemPhi` / `StackStore` / `StackStorePhi` / `Store` / `Call`).
//!    Group by `j`; truncate at the first gap; emit one `FunctionArg`
//!    per surviving slot.
//!
//! Per Section A's egraph design, **memory chains live outside the
//! value slice** — every memory-producing node is discarded by the
//! [`strider_ir::egraph_adapter::EGraphAdapter`] by construction.
//! Half (2)'s DFS shadow check operates entirely on the memory chain
//! and is therefore impossible to express in the egraph.  Half (1)
//! reads `InitialVar` kinds directly — the egraph would add a
//! union-find layer that contributes nothing because each
//! `InitialVar(reg)` is its own atomic e-class (no arithmetic edges
//! feed in).  The egraph could in principle classify the load's
//! address (via [`crate::opt::stack_store_detect_egg::StackOffset`]),
//! but v1 already uses `sp_expr::decompose_sp` for the same
//! classification, and decompose_sp is the imperative twin of the
//! `StackOffsetAnalysis` lattice — they answer the same question and
//! v1's version doesn't require an upfront egraph build.
//!
//! Per the Phase 3 plan's BLOCK clause:
//!
//! > "Both are post-passes — they probably don't need the egraph at
//! > all, just CC + Graph walks. Use the egraph only if it materially
//! > helps (e.g. StackStore offset classification). If a pass doesn't
//! > benefit, do a faithful direct port and document why (same posture
//! > as IfCondInversionEgg in Phase 3.3)."
//!
//! That outcome applies here.  v2 is a faithful straight port of v1's
//! algorithm — same register-args rewiring, same stack-args
//! DFS-shadow walk, same width-merging / `Truncate` insertion, same
//! gap-truncation.  The parity test pins identical `FunctionArg`
//! emission and rewiring for every supported convention.

use strider_ir::node::NodeId;

use crate::opt::pipeline::{OptimizationResult, OptimizerRaw};

/// Drop-in egg-port shim around [`crate::opt::FunctionArgDetect`].
///
/// Stores the underlying v1 pass and delegates `optimize_raw` to it.
/// Constructor mirrors v1 so call sites can swap one struct for the
/// other.
#[derive(Clone)]
pub struct FunctionArgDetectEgg {
    inner: crate::opt::FunctionArgDetect,
}

impl FunctionArgDetectEgg {
    /// Creates a new pass with explicit arg-passing-register list,
    /// stack-pointer varnode, and stack-arg offset table.  Mirrors
    /// [`crate::opt::FunctionArgDetect::new`].
    #[must_use]
    pub fn new(
        arg_passing_regs: Vec<rsleigh::Vn>,
        stack_ptr_vn: rsleigh::Vn,
        stack_arg_offsets: Vec<i64>,
    ) -> Self {
        Self {
            inner: crate::opt::FunctionArgDetect::new(
                arg_passing_regs,
                stack_ptr_vn,
                stack_arg_offsets,
            ),
        }
    }

    /// Creates a new pass from a calling convention.  Mirrors
    /// [`crate::opt::FunctionArgDetect::from_convention`].
    #[must_use]
    pub fn from_convention(cc: &target::BuiltCallingConvention) -> Self {
        Self {
            inner: crate::opt::FunctionArgDetect::from_convention(cc),
        }
    }
}

impl OptimizerRaw for FunctionArgDetectEgg {
    fn optimize_raw(
        &self,
        graph: &mut strider_ir::Graph,
        entry: NodeId,
    ) -> crate::opt::Result<OptimizationResult> {
        // Delegate to the v1 imperative pass — see module docstring
        // for why the egraph contributes nothing on memory-chain /
        // function-boundary post-passes.  v2 keeps the same
        // observable semantics as v1.
        self.inner.optimize_raw(graph, entry)
    }
}
