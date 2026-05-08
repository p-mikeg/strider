//! `GraphRewriter`, a thin façade over [`pattern::rewrite_rule`] that
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
//!    [`pattern::rewrite_rule`] hands back).  The walk fixes the
//!    "rule applied at node N may match at node M too" gap left by
//!    `rewrite_rule`, which is per-root.
//! 2. A `re_optimize` shortcut that runs an [`opt::OptimizerPipeline`] on
//!    the wrapped graph after one or more rules fired — so users can
//!    chain "rewrite → re-optimize → rewrite again" without leaving the
//!    rewriter.
//!
//! The rewriter API is constants + input replacement only: callers
//! compose patterns with the existing `pattern` builder constructors
//! (`int_const`, `var`, `add`, `load`, …) and pass the resulting
//! closure (built via [`pattern::rewrite_rule`]) into
//! [`GraphRewriter::apply_rule`].
//!
//! # Use case — wrap a built graph and apply a no-op rule
//!
//! ```
//! # use anyhow::Result;
//! # fn doc() -> Result<()> {
//! use ir::node::NodeOutputType;
//!
//! // Build a minimal graph: `fn() -> 0u64`.
//! let mut built = ir::test_utils::make_empty_fn(|b| {
//!     b.build_int_const(0u64, NodeOutputType::U64)
//! })?;
//!
//! // A no-op rule: matches anything, returns `Ok(false)` (didn't fire).
//! // Real rules come from `pattern::rewrite_rule(matcher_pat, replacement_pat)`
//! // and would mutate the graph here.
//! let mut rewriter = strider::GraphRewriter::wrap_built(&mut built);
//! let fired = rewriter.apply_rule(|_g, _n| Ok(false))?;
//! assert_eq!(fired, 0);
//! # Ok(())
//! # }
//! # doc().unwrap();
//! ```

use anyhow::Result;
use ir::node::NodeId;
use ir::{BuiltFunctionGraph, Graph};

/// Thin façade over [`pattern::rewrite_rule`] / [`pattern::apply_rules_in_order`]
/// that lets users replace any node's input with a constant (or any other
/// built pattern) and re-run the optimizer pipeline on the rewritten graph.
///
/// See module-level docs for the architecture and intended use case.
pub struct GraphRewriter<'a> {
    /// The graph to rewrite.  Held as `&mut Graph` rather than
    /// `&mut BuiltFunctionGraph` to align with the optimizer pass
    /// contract `(&mut Graph, NodeId)`.  `pattern::rewrite_rule`'s
    /// closure expects `&mut BuiltFunctionGraph`, so [`Self::apply_rule`]
    /// swaps the graph into a short-lived `BuiltFunctionGraph` per
    /// call (via `mem::take` — same trick as `opt::with_built`).
    graph: &'a mut Graph,
    /// The function's entry [`NodeId`] — needed by the validator's
    /// reachable-set walk and by [`opt::OptimizerPipeline::run`].
    entry: NodeId,
}

impl<'a> GraphRewriter<'a> {
    /// Wraps a [`BuiltFunctionGraph`].
    pub fn wrap_built(built: &'a mut BuiltFunctionGraph) -> Self {
        let entry = built.entry;
        Self {
            graph: &mut built.graph,
            entry,
        }
    }

    /// Walks every reachable node in the graph and invokes `rule` once
    /// per candidate root.  Returns the number of times the rule fired
    /// (i.e. returned `Ok(true)`).
    ///
    /// The closure shape matches what [`pattern::rewrite_rule`] hands
    /// back — `Fn(&mut BuiltFunctionGraph, NodeId) -> pattern::Result<bool>`.
    /// Wraps the wrapped graph into a short-lived `BuiltFunctionGraph`
    /// per call (via [`mem::take`]) so the closure has the input shape
    /// the `pattern` crate's rewrite engine was designed for.  The
    /// dummy `BuiltFunctionGraph` carries empty `variables` /
    /// `call_clobbered` / `ret_val_regs` — `pattern::rewrite_rule`
    /// only touches `graph` and `entry`, verified by inspection of
    /// [`pattern::rewrite_rule`]'s implementation.
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
    /// # Validation
    ///
    /// `apply_rule` does **NOT** call [`ir::validate::validate`] after
    /// the rule fires.  Callers building unusual rules should run
    /// `re_optimize` (which validates as the last step of the optimizer
    /// pipeline) or call [`ir::validate::validate`] explicitly before
    /// relying on the graph being well-formed.
    pub fn apply_rule<F>(&mut self, rule: F) -> Result<usize>
    where
        F: Fn(&mut BuiltFunctionGraph, NodeId) -> pattern::Result<bool>,
    {
        let mut applied: usize = 0;
        // Pre-collect candidate roots before mutating; the walk's
        // iterator borrows the graph immutably.
        let candidates: Vec<NodeId> = self.graph.preorder(self.entry).collect();
        // Take the graph out of `&mut self.graph` so we can package it
        // into a `BuiltFunctionGraph` the rule expects.  Restored at
        // the end of every iteration by writing back through the
        // `*self.graph = ...` slot.
        for node in candidates {
            let stolen = std::mem::take(&mut *self.graph);
            let mut tmp = BuiltFunctionGraph::from_graph_and_entry_for_rewrite(stolen, self.entry);
            // `cranelift_entity::PrimaryMap` doesn't reuse keys, so
            // every id from the pre-collected preorder is still a
            // valid arena slot — even if the node was detached by an
            // earlier rule firing on this same walk.  The rule's
            // structural matcher returns `Ok(false)` on a detached /
            // rewired node, so this is safe.
            let fired_result = rule(&mut tmp, node);
            *self.graph = tmp.graph;
            if fired_result? {
                applied += 1;
            }
        }
        Ok(applied)
    }

    /// Applies every rule in `rules` round-robin at every candidate
    /// root and returns the total fire count.
    ///
    /// Composes [`pattern::apply_rules_in_order`] (which OR-folds N
    /// rules into a single closure that runs every rule once at a
    /// single root) with [`Self::apply_rule`]'s graph-wide walk.  The
    /// resulting policy is exactly: "for each reachable node N, for
    /// each rule R, try R(N); count every fire."
    ///
    /// # Errors
    ///
    /// Propagates the first error from any rule.  See [`Self::apply_rule`].
    pub fn apply_rules(&mut self, rules: &[pattern::BoxedRule]) -> Result<usize> {
        let composed = pattern::apply_rules_in_order(rules);
        self.apply_rule(composed)
    }

    /// Re-runs `pipeline` on the wrapped graph.  Pairs naturally with
    /// [`Self::apply_rule`] — the typical flow is "rewrite, then re-
    /// optimize so the rewrite's downstream effects (constant-fold
    /// the new const, prune the dead branches it enabled) settle".
    ///
    /// The pipeline is the caller's choice; for the standard collapse-
    /// dead-branches behaviour pass [`opt::default_pipeline`].
    /// Convention-aware callers (e.g. tests that need
    /// [`opt::StackStoreDetect`]) pass [`crate::Strider::build_optimizer_pipeline`]'s
    /// output instead.
    ///
    /// CORRECTNESS — idempotent: running the same pipeline twice in a
    /// row produces the same final graph because [`opt::OptimizerPipeline::run`]
    /// itself runs to a fixed point internally.  Pinned by the
    /// `re_optimize_is_idempotent` test below.
    ///
    /// # Errors
    ///
    /// Propagates the first error from any pass in `pipeline`.
    pub fn re_optimize(&mut self, pipeline: &opt::OptimizerPipeline) -> Result<()> {
        pipeline.run(&mut *self.graph, self.entry)
    }
}


#[cfg(test)]
#[path = "rewrite_tests.rs"]
mod rewrite_tests;
