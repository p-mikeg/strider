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
//! 2. [`GraphRewriter::function_mut`] / [`GraphRewriter::entry`] accessors
//!    that let callers drive an [`crate::opt::OptimizerPipeline`] on
//!    the wrapped graph after one or more rules fired — chaining
//!    "rewrite → re-optimize → rewrite again" without leaving the
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
//! let mut rewriter = strider_analyze::GraphRewriter::try_wrap_built(&mut built)?;
//! let fired = rewriter.apply_rule(|_g, _n| Ok(false))?;
//! assert_eq!(fired, 0);
//! # Ok(())
//! # }
//! # doc().unwrap();
//! ```

use anyhow::Result;
use strider_ir::node::NodeId;

/// Thin façade over [`crate::pattern::rewrite_rule`] / `crate::pattern::apply_rules_in_order`
/// that lets users replace any node's input with a constant (or any other
/// built pattern) and re-run the optimizer pipeline on the rewritten graph.
///
/// See module-level docs for the architecture and intended use case.
pub struct GraphRewriter<'a> {
    /// The function to rewrite.  `crate::pattern::rewrite_rule`'s
    /// closure expects `&mut RewriteCtx<'_>`, so [`Self::apply_rule`]
    /// builds a fresh `RewriteCtx::new(&mut *self.function, self.entry)`
    /// per call — same shape as `crate::opt::with_rewrite_ctx`.
    function: &'a mut strider_ir::Function,
    /// The function's entry [`NodeId`] — needed by the validator's
    /// reachable-set walk and by [`crate::opt::OptimizerPipeline::run`].
    entry: NodeId,
}

impl<'a> GraphRewriter<'a> {
    /// Wraps a built [`Graph`].
    ///
    /// # Errors
    ///
    /// Returns an error if the graph has not been built (i.e. `entry`
    /// is `None`).
    pub fn try_wrap_built(built: &'a mut strider_ir::Function) -> Result<Self> {
        let entry = built.entry().ok_or_else(|| {
            anyhow::anyhow!("GraphRewriter::try_wrap_built: entry node is not set")
        })?;
        Ok(Self {
            function: built,
            entry,
        })
    }

    /// Walks every reachable node in the graph and invokes `rule` once
    /// per candidate root.  Returns the number of times the rule fired
    /// (i.e. returned `Ok(true)`).
    ///
    /// The closure shape matches what [`crate::pattern::rewrite_rule`]
    /// hands back —
    /// `Fn(&mut crate::pattern::RewriteCtx<'_>, NodeId) -> crate::pattern::Result<bool>`.
    /// Each candidate root gets a freshly-constructed
    /// `RewriteCtx::new(&mut *self.function, self.entry)` so the closure
    /// has the input shape the `pattern` crate's rewrite engine was
    /// designed for.  `RewriteCtx` exposes only `graph` and `entry`;
    /// `crate::pattern::rewrite_rule` only touches those two fields,
    /// verified by inspection of its implementation.
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
    /// `Graph`.  `RewriteCtx` exposes only `graph` and
    /// `entry`; the calling convention's `variables`, `call_clobbered`,
    /// `ret_val_regs`, `call_other_clobbered` fields on the `Graph` are
    /// **not** visible from inside the closure.  Rules that need CC
    /// information must close over it from the surrounding scope at
    /// call site.
    ///
    /// # Validation
    ///
    /// `apply_rule` does **NOT** call [`strider_ir::validate::validate`] after
    /// the rule fires.  Callers building unusual rules should run an
    /// [`crate::opt::OptimizerPipeline`] (via [`Self::function_mut`] /
    /// [`Self::entry`]) — `OptimizerPipeline::run` validates as its
    /// last step — or call [`strider_ir::validate::validate`] explicitly
    /// before relying on the graph being well-formed.
    pub fn apply_rule<F>(&mut self, rule: F) -> Result<usize>
    where
        F: for<'g> Fn(&mut crate::pattern::RewriteCtx<'g>, NodeId) -> crate::pattern::Result<bool>,
    {
        let mut applied: usize = 0;
        // Pre-collect candidate roots before mutating; the walk's
        // iterator borrows the graph immutably.
        let candidates: Vec<NodeId> = self.function.walk_from(self.entry).collect();
        for node in candidates {
            let mut ctx = crate::pattern::RewriteCtx::new(&mut *self.function, self.entry);
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
    /// Composes `crate::pattern::apply_rules_in_order` (which OR-folds N
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

    /// Mutable access to the wrapped function.  Pairs with
    /// [`Self::entry`] for callers that want to drive an
    /// [`crate::opt::OptimizerPipeline`] directly after a rewrite —
    /// typical flow: "rewrite, then re-optimize so the rewrite's
    /// downstream effects (constant-fold the new const, prune the dead
    /// branches it enabled) settle".  `pipeline.run` itself runs to a
    /// fixed point internally, so re-running it on an unchanged graph
    /// is a no-op.
    pub fn function_mut(&mut self) -> &mut strider_ir::Function {
        self.function
    }

    /// The function's entry [`NodeId`].  Stable for the lifetime of
    /// the wrapped function; pair with [`Self::function_mut`] when feeding a
    /// pipeline.
    #[must_use]
    pub fn entry(&self) -> NodeId {
        self.entry
    }
}


#[cfg(test)]
#[path = "rewrite_tests.rs"]
mod rewrite_tests;
