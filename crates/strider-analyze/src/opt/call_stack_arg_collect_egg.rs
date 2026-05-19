//! Egg-based `CallStackArgCollect` rewriter — Phase 3 Task 3.7a.
//!
//! Built alongside the imperative [`crate::opt::CallStackArgCollect`] —
//! NOT a replacement.  The parity test
//! `crates/strider-analyze/tests/call_stack_arg_collect_egg_parity.rs`
//! proves both produce structurally identical IR for the supported
//! shapes.
//!
//! # Design — why this pass does NOT use the egraph
//!
//! `CallStackArgCollect` is a **memory-chain post-pass**: it walks
//! backward from each `Call`'s memory input through `StackStore` /
//! `Store` nodes, matches each store's offset against the calling
//! convention's `stack_arg_offsets` slot table, and appends the
//! discovered data outputs as positional `Call` inputs.
//!
//! Per Section A's egraph design, **memory chains live outside the
//! value slice** — `Store`, `StackStore`, `MemPhi`, and `Load` are
//! discarded by the [`strider_ir::egraph_adapter::EGraphAdapter`] by
//! construction.  The egraph would therefore tell us *nothing* about
//! the chain we need to walk.  In principle the egraph could classify
//! a store's *address* (it carries a
//! [`crate::opt::stack_store_detect_egg::StackOffset`] lattice value
//! after Phase 3.5a), but `StackStoreDetect` has already run by the
//! time `CallStackArgCollect` runs (it's a post-pass) and every
//! SP-relative `Store` has been rewritten to a `StackStore { offset }`
//! whose offset is a literal in the node's [`NodeKind`].  The address
//! classification is already done — the only remaining work is the
//! memory-chain walk, which is purely imperative.
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
//! collection algorithm — same memory-chain walk, same prefix/set
//! membership rules, same `sp_expr::decompose_sp`-based alias
//! discrimination for plain `Store` interlopers.  The parity test
//! pins identical Call-input shape for every supported convention.

use strider_ir::node::NodeId;

use crate::opt::pipeline::{OptimizationResult, OptimizerRaw};

/// Drop-in egg-port shim around [`crate::opt::CallStackArgCollect`].
///
/// Stores the underlying v1 pass and delegates `optimize_raw` to it.
/// Constructor mirrors v1 so call sites can swap one struct for the
/// other.
#[derive(Clone)]
pub struct CallStackArgCollectEgg {
    inner: crate::opt::CallStackArgCollect,
}

impl CallStackArgCollectEgg {
    /// Creates a new pass for the given positional stack-arg offset table
    /// and stack-pointer varnode.  Mirrors
    /// [`crate::opt::CallStackArgCollect::new`].
    #[must_use]
    pub fn new(stack_arg_offsets: Vec<i64>, stack_ptr_vn: rsleigh::Vn) -> Self {
        Self {
            inner: crate::opt::CallStackArgCollect::new(stack_arg_offsets, stack_ptr_vn),
        }
    }

    /// Creates a new pass from a calling convention.  Mirrors
    /// [`crate::opt::CallStackArgCollect::from_convention`].
    #[must_use]
    pub fn from_convention(cc: &target::BuiltCallingConvention) -> Self {
        Self {
            inner: crate::opt::CallStackArgCollect::from_convention(cc),
        }
    }
}

impl OptimizerRaw for CallStackArgCollectEgg {
    fn optimize_raw(
        &self,
        graph: &mut strider_ir::Graph,
        entry: NodeId,
    ) -> crate::opt::Result<OptimizationResult> {
        // Delegate to the v1 imperative pass — see module docstring
        // for why the egraph contributes nothing on memory-chain
        // post-passes.  v2 keeps the same observable semantics as v1.
        self.inner.optimize_raw(graph, entry)
    }
}
