//! `GraphRewriter`, a thin façade over [`crate::pattern::rewrite_rule`] that
//! lets users replace any node's input with a constant (or any other built
//! pattern) and re-run the optimizer to collapse jump tables / switches.
//!
//! # Architecture
//!
//! The rewriter is a tiny adapter — substitution logic lives in the
//! `pattern` crate (`rewrite_rule`, `apply_rules_in_order`).  This module
//! adds two pieces of glue on top:
//!
//! 1. A graph-walk loop that calls a single rule's closure at every
//!    candidate root node in the reachable graph (that's the closure
//!    [`crate::pattern::rewrite_rule`] hands back).  The walk fixes the
//!    "rule applied at node N may match at node M too" gap left by
//!    `rewrite_rule`, which is per-root.
//! 2. A `re_optimize` shortcut that runs an [`crate::opt::OptimizerPipeline`] on
//!    the wrapped graph after one or more rules fired — so users can
//!    chain "rewrite → re-optimize → rewrite again" without leaving the
//!    rewriter.
//!
//! The rewriter API is constants + input replacement only: callers
//! compose patterns with the existing `pattern` builder constructors
//! (`int_const`, `var`, `add`, `load`, …) and pass the resulting
//! closure (built via [`crate::pattern::rewrite_rule`]) into
//! [`GraphRewriter::apply_rule`].
//!
//! # Use case — wrap a built graph and apply a no-op rule
//!
//! ```
//! # use anyhow::Result;
//! # fn doc() -> Result<()> {
//! use strider_ir::node::NodeOutputType;
//!
//! // Build a minimal graph: `fn() -> 0u64`.
//! let mut built = strider_ir_test_utils::make_empty_fn(|b| {
//!     b.build_int_const(0u64, NodeOutputType::U64)
//! })?;
//!
//! // A no-op rule: matches anything, returns `Ok(false)` (didn't fire).
//! // Real rules come from `crate::pattern::rewrite_rule(matcher_pat, replacement_pat)`
//! // and would mutate the graph here.
//! let mut rewriter = strider_analyze::GraphRewriter::wrap_built(&mut built);
//! let fired = rewriter.apply_rule(|_g, _n| Ok(false))?;
//! assert_eq!(fired, 0);
//! # Ok(())
//! # }
//! # doc().unwrap();
//! ```

use anyhow::Result;
use strider_ir::node::NodeId;
use strider_ir::{BuiltFunctionGraph, Graph};

/// Thin façade over [`crate::pattern::rewrite_rule`] / [`crate::pattern::apply_rules_in_order`]
/// that lets users replace any node's input with a constant (or any other
/// built pattern) and re-run the optimizer pipeline on the rewritten graph.
///
/// See module-level docs for the architecture and intended use case.
pub struct GraphRewriter<'a> {
    /// The graph to rewrite.  Held as `&mut Graph` rather than
    /// `&mut BuiltFunctionGraph` to align with the optimizer pass
    /// contract `(&mut Graph, NodeId)`.  `crate::pattern::rewrite_rule`'s
    /// closure expects `&mut RewriteCtx<'_>`, so [`Self::apply_rule`]
    /// builds a fresh `RewriteCtx::new(&mut *self.graph, self.entry)`
    /// per call — same shape as `crate::opt::with_rewrite_ctx`.
    graph: &'a mut Graph,
    /// The function's entry [`NodeId`] — needed by the validator's
    /// reachable-set walk and by [`crate::opt::OptimizerPipeline::run`].
    entry: NodeId,
}

impl<'a> GraphRewriter<'a> {
    /// Wraps a [`BuiltFunctionGraph`].
    pub fn wrap_built(built: &'a mut BuiltFunctionGraph) -> Self {
        let entry = built.entry();
        Self {
            graph: built.graph_mut(),
            entry,
        }
    }

    /// Walks every reachable node in the graph and invokes `rule` once
    /// per candidate root.  Returns the number of times the rule fired
    /// (i.e. returned `Ok(true)`).
    ///
    /// The closure shape matches what [`crate::pattern::rewrite_rule`] hands
    /// back — `Fn(&mut BuiltFunctionGraph, NodeId) -> crate::pattern::Result<bool>`.
    /// Wraps the wrapped graph into a short-lived `BuiltFunctionGraph`
    /// per call (via [`std::mem::take`]) so the closure has the input shape
    /// the `pattern` crate's rewrite engine was designed for.  The
    /// dummy `BuiltFunctionGraph` carries empty `variables` /
    /// `call_clobbered` / `ret_val_regs` — `crate::pattern::rewrite_rule`
    /// only touches `graph` and `entry`, verified by inspection of
    /// [`crate::pattern::rewrite_rule`]'s implementation.
    ///
    /// CORRECTNESS — reachable-set walk:
    /// We pre-collect the candidate node ids before invoking the rule
    /// because the rule may mutate the graph (e.g. detach an Add by
    /// rewiring its uses), and walking `preorder` while the graph
    /// mutates underneath us would be undefined.  Pre-collection
    /// freezes the candidate set; nodes detached by an earlier
    /// invocation just return `Ok(false)` from the rule (their
    /// matcher's structural check fails on a node whose inputs were
    /// rewired) and don't contribute to the count.
    ///
    /// # Errors
    ///
    /// Propagates the rule closure's first non-skip error via `anyhow`.
    ///
    /// # Rule-closure context
    ///
    /// The closure receives `&mut crate::pattern::RewriteCtx`, NOT the wrapped
    /// `BuiltFunctionGraph`.  `RewriteCtx` exposes only `graph` and
    /// `entry`; the calling convention's `variables`, `call_clobbered`,
    /// `ret_val_regs`, `call_other_clobbered` fields on the BFG are
    /// **not** visible from inside the closure.  Rules that need CC
    /// information must close over it from the surrounding scope at
    /// call site.
    ///
    /// # Validation
    ///
    /// `apply_rule` does **NOT** call [`strider_ir::validate::validate`] after
    /// the rule fires.  Callers building unusual rules should run
    /// `re_optimize` (which validates as the last step of the optimizer
    /// pipeline) or call [`strider_ir::validate::validate`] explicitly before
    /// relying on the graph being well-formed.
    pub fn apply_rule<F>(&mut self, rule: F) -> Result<usize>
    where
        F: for<'g> Fn(&mut crate::pattern::RewriteCtx<'g>, NodeId) -> crate::pattern::Result<bool>,
    {
        let mut applied: usize = 0;
        // Pre-collect candidate roots before mutating; the walk's
        // iterator borrows the graph immutably.
        let candidates: Vec<NodeId> = self.graph.preorder(self.entry).collect();
        for node in candidates {
            let mut ctx = crate::pattern::RewriteCtx::new(&mut *self.graph, self.entry);
            // `cranelift_entity::PrimaryMap` doesn't reuse keys, so
            // every id from the pre-collected preorder is still a
            // valid arena slot — even if the node was detached by an
            // earlier rule firing on this same walk.  The rule's
            // structural matcher returns `Ok(false)` on a detached /
            // rewired node, so this is safe.
            if rule(&mut ctx, node)? {
                applied += 1;
            }
        }
        Ok(applied)
    }

    /// Applies every rule in `rules` round-robin at every candidate
    /// root and returns the total fire count.
    ///
    /// Composes [`crate::pattern::apply_rules_in_order`] (which OR-folds N
    /// rules into a single closure that runs every rule once at a
    /// single root) with [`Self::apply_rule`]'s graph-wide walk.  The
    /// resulting policy is exactly: "for each reachable node N, for
    /// each rule R, try R(N); count every fire."
    ///
    /// # Errors
    ///
    /// Propagates the first error from any rule.  See [`Self::apply_rule`].
    pub fn apply_rules(&mut self, rules: &[crate::pattern::BoxedRule]) -> Result<usize> {
        let composed = crate::pattern::apply_rules_in_order(rules);
        self.apply_rule(composed)
    }

    /// Re-runs `pipeline` on the wrapped graph.  Pairs naturally with
    /// [`Self::apply_rule`] — the typical flow is "rewrite, then re-
    /// optimize so the rewrite's downstream effects (constant-fold
    /// the new const, prune the dead branches it enabled) settle".
    ///
    /// The pipeline is the caller's choice; for the standard collapse-
    /// dead-branches behaviour pass [`crate::opt::default_pipeline`].
    /// Convention-aware callers (e.g. tests that need
    /// [`crate::opt::StackStoreDetect`]) pass [`crate::Strider::build_optimizer_pipeline`]'s
    /// output instead.
    ///
    /// CORRECTNESS — idempotent: running the same pipeline twice in a
    /// row produces the same final graph because [`crate::opt::OptimizerPipeline::run`]
    /// itself runs to a fixed point internally.  Pinned by the
    /// `re_optimize_is_idempotent` test below.
    ///
    /// # Destructive passes
    ///
    /// `re_optimize` runs whatever pipeline the caller passes — it does
    /// not gate on stable-vs-destructive.  Destructive passes
    /// (`RedundantPhis`, `DeadBranchElimination`) detach nodes and may
    /// invalidate `NodeId`s the caller is still holding outside the
    /// `GraphRewriter`.  Use `crate::opt::stable_default_pipeline()` (or
    /// `Strider::build_stable_optimizer_pipeline()`) when you need to
    /// preserve external `NodeId` references; pass the destructive
    /// pipeline explicitly when you want the cleanup.
    ///
    /// # Errors
    ///
    /// Propagates the first error from any pass in `pipeline`.
    pub fn re_optimize(&mut self, pipeline: &crate::opt::OptimizerPipeline) -> Result<()> {
        pipeline.run(&mut *self.graph, self.entry)
    }
}


#[cfg(test)]
#[path = "rewrite_tests.rs"]
mod rewrite_tests;
