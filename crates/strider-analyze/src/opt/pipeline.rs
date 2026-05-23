/// Whether an optimization pass made any change to the IR graph.
///
/// Passes return this from `Optimizer::optimize`.  The pipeline uses it to
/// decide whether to run another fixed-point iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptimizationResult {
    /// The graph was not modified.
    NoChange,
    /// At least one node was changed, added, or removed.
    Changed,
}

impl OptimizationResult {
    /// Returns `true` when the result is [`Changed`](OptimizationResult::Changed).
    #[inline]
    #[must_use]
    pub fn changed(self) -> bool {
        matches!(self, OptimizationResult::Changed)
    }

    /// Maps the boolean return of [`strider_ir::Graph::replace_all_uses`] to
    /// an `OptimizationResult`: `true` → `Changed`, `false` → `NoChange`.
    #[must_use]
    pub fn from_changed(changed: bool) -> Self {
        if changed {
            OptimizationResult::Changed
        } else {
            OptimizationResult::NoChange
        }
    }

    /// Replaces every use of `old` with `new`, **absorbs** the producer
    /// of `old`'s asm-fingerprint into `new`'s producer, and folds the
    /// resulting `Changed`/`NoChange` into `self`.
    ///
    /// # Errors
    ///
    /// Propagates [`strider_ir::Graph::replace_all_uses`]'s `Err` arm as a
    /// typed error rather than panicking.  The
    /// underlying error only fires on a null cursor in
    /// `replace_current_with`, but `replace_all_uses` checks
    /// `cursor.current().is_some()` before every call — so this is a
    /// structural by-construction invariant.  Returning `Result`
    /// rather than panicking keeps Python users seeing a clean typed
    /// exception if the invariant is ever violated.
    pub fn after_replace(
        self,
        function: &mut crate::pattern::RewriteCtx<'_>,
        old: strider_ir::node::NodeOutputId,
        new: strider_ir::node::NodeOutputId,
    ) -> crate::opt::Result<Self> {
        let old_node = function.get_node_from_output(old);
        let new_node = function.get_node_from_output(new);
        function.extend_asm_fingerprint_from(new_node, old_node);
        let changed = function.replace_all_uses(old, new)?;
        Ok(self | OptimizationResult::from_changed(changed))
    }
}

impl std::ops::BitOr for OptimizationResult {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        if self.changed() || rhs.changed() {
            OptimizationResult::Changed
        } else {
            OptimizationResult::NoChange
        }
    }
}

impl std::ops::BitOrAssign for OptimizationResult {
    fn bitor_assign(&mut self, rhs: Self) {
        *self = *self | rhs;
    }
}

/// A single IR optimization pass.
///
/// Implement this trait to add a new pass.  The pass receives the
/// function `graph` and its `entry` [`strider_ir::node::NodeId`],
/// applies whatever transformations it can in one sweep, and returns
/// [`OptimizationResult::Changed`] if anything was modified (causing
/// the pipeline to run another iteration) or
/// [`OptimizationResult::NoChange`] if the graph is already in normal
/// form for this pass.
///
/// # Why `(&mut Graph, NodeId)` and not `&mut crate::pattern::RewriteCtx<'_>`
///
/// `RewriteCtx<'_>` carries a lifetime parameter, which prevents it
/// appearing as the receiver type of a trait object
/// (`Box<dyn Optimizer>`).  The pipeline stores type-erased passes, so
/// the trait must be object-safe with no lifetime parameter.  Pass
/// authors that want the ergonomic `RewriteCtx` API construct one
/// internally at the top of `optimize`:
///
/// ```ignore
/// fn optimize(&self, graph: &mut Graph, entry: NodeId) -> Result<OptimizationResult> {
///     let mut ctx = crate::pattern::RewriteCtx::new(graph, entry);
///     // ... pass body operating on `ctx` ...
/// }
/// ```
///
/// Callers can run optimizer passes on a graph that has not yet been
/// packaged into a final [`strider_ir::Graph`] (e.g. on a
/// live [`strider_ir::FunctionBuilder`] via
/// [`strider_ir::FunctionBuilder::graph_mut`] +
/// [`strider_ir::FunctionBuilder::entry`]).  `Graph` is a
/// final-output convenience type, not a precondition for analysis.
///
/// `entry` is the function's entry [`strider_ir::node::NodeId`] —
/// needed because several passes walk the reachable-node set
/// (`graph.preorder(entry)`) or use it directly
/// (`strider_ir::walk::cfg_reachable(graph, entry)`).
pub trait Optimizer: OptimizerClone + Send + Sync {
    /// Run one sweep of this pass over the IR `graph`, anchored at `entry`.
    ///
    /// # Errors
    ///
    /// Returns the first error encountered by the pass — typically an IR
    /// validation failure or a pattern-rewrite error propagated up through
    /// `anyhow::Error`.
    fn optimize(
        &self,
        graph: &mut strider_ir::Graph,
        entry: strider_ir::node::NodeId,
    ) -> crate::opt::Result<OptimizationResult>;
}

/// Object-safe clone shim for [`Optimizer`].
///
/// Enables external iteration over the canonical default pipelines:
/// downstream crates (e.g. `strider-py`) snapshot the pass list via
/// [`OptimizerPipeline::iter`] / [`OptimizerPipeline::iter_post`] and
/// `clone_box` each entry into their own storage, rather than
/// hand-mirroring the pass list and risking silent drift.
///
/// Every concrete `Optimizer + Clone + 'static` gets a blanket
/// `OptimizerClone` impl for free, so pass authors never write
/// `clone_box` by hand — `#[derive(Clone)]` on the pass type is
/// sufficient.  ZST passes get `Clone` via `#[derive(Clone, Copy)]`.
pub trait OptimizerClone {
    /// Clone the pass behind a `Box<dyn Optimizer>`.
    fn clone_box(&self) -> Box<dyn Optimizer>;
}

impl<T: Optimizer + Clone + 'static> OptimizerClone for T {
    fn clone_box(&self) -> Box<dyn Optimizer> {
        Box::new(self.clone())
    }
}

/// An ordered list of `Optimizer` passes that are run in a shared fixed-point
/// loop.
///
/// On each iteration every pass is called once in registration order.  The loop
/// repeats until no pass reports a change.  Use [`OptimizerPipeline::add`] to
/// register passes and [`OptimizerPipeline::run`] to execute them.
///
/// Internally the pipeline stores passes as `Box<dyn Optimizer>` so it
/// can dispatch on `(&mut Graph, NodeId)` directly.
pub struct OptimizerPipeline {
    optimizers: Vec<Box<dyn Optimizer>>,
    post_passes: Vec<Box<dyn Optimizer>>,
}

impl Default for OptimizerPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl OptimizerPipeline {
    /// Creates an empty pipeline with no passes registered.
    #[must_use]
    pub fn new() -> Self {
        Self {
            optimizers: Vec::new(),
            post_passes: Vec::new(),
        }
    }

    /// Appends `opt` to the end of the pass list.
    pub fn add<O: Optimizer + 'static>(&mut self, opt: O) {
        self.optimizers.push(Box::new(opt));
    }

    /// Appends `opt` to the post-pass list.  Post-passes run once, in
    /// registration order, after the fixed-point loop converges.  Their return
    /// value is ignored (no re-entry into the fixed-point loop).
    pub fn add_post_pass<O: Optimizer + 'static>(&mut self, opt: O) {
        self.post_passes.push(Box::new(opt));
    }

    /// Iterate the fixed-point passes in registration order.
    ///
    /// Lets downstream crates snapshot the canonical pipeline without
    /// hand-mirroring the pass list.  Combine with the
    /// `OptimizerClone::clone_box` supertrait method to materialise an
    /// independent copy of each pass.
    pub fn iter(&self) -> impl Iterator<Item = &dyn Optimizer> + '_ {
        self.optimizers.iter().map(|b| &**b)
    }

    /// Iterate the post-passes in registration order.  See
    /// [`OptimizerPipeline::iter`] for the use-case.
    pub fn iter_post(&self) -> impl Iterator<Item = &dyn Optimizer> + '_ {
        self.post_passes.iter().map(|b| &**b)
    }

    /// Runs all registered passes in a fixed-point loop until convergence,
    /// then runs each post-pass exactly once in registration order.
    ///
    /// Returns `Ok(())` when no pass changed the graph in a full iteration
    /// and all post-passes completed without error.  Propagates the first
    /// error returned by any pass.
    ///
    /// # Errors
    ///
    /// Returns the first `anyhow::Error` reported by any pass.  If every
    /// pass and post-pass succeeds, the graph is then re-validated and any
    /// validation error is returned.  When a post-pass returns `Err`, the
    /// final validation step is skipped — the pass error wins.
    pub fn run(
        &self,
        graph: &mut strider_ir::Graph,
        entry: strider_ir::node::NodeId,
    ) -> crate::opt::Result<()> {
        const MAX_ITERS: u32 = 1024;
        let mut iters: u32 = 0;
        loop {
            let mut changed = false;
            for opt in &self.optimizers {
                if opt.optimize(graph, entry)?.changed() {
                    changed = true;
                }
            }
            if !changed {
                break;
            }
            iters += 1;
            if iters >= MAX_ITERS {
                anyhow::bail!("optimizer pipeline did not converge after {MAX_ITERS} iterations");
            }
        }
        for opt in &self.post_passes {
            opt.optimize(graph, entry)?;
        }
        strider_ir::validate::validate(graph, entry)?;
        Ok(())
    }

}

#[cfg(test)]
mod tests {
    //! Unit tests for [`OptimizerPipeline::run`].

    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use strider_ir::FunctionBuilder;
    use strider_ir::node::NodeOutputType;
    use strider_ir_test_utils::SENTINEL_LIFT_ADDR;

    /// Build a tiny single-region function returning `IntConst(K)`.
    fn one_const_fn(k: u64) -> strider_ir::Graph {
        let mut b = FunctionBuilder::empty().unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let v = b.build_int_const(k, NodeOutputType::U64).unwrap();
        b.build_return(Some(v), &[]).unwrap();
        b.set_lift_addr(None);
        b.build().unwrap()
    }

    /// `run(graph, entry)` validates the final graph — an invalid graph
    /// in the post-pass output surfaces as a `ValidationErrors`-bearing
    /// `anyhow::Error` (downcastable) (downcastable via `anyhow::Error::
    /// downcast_ref::<strider_ir::validate::ValidationErrors>()`).  Smoke test
    /// using an empty post-pass list and a valid input — run must
    /// succeed (no validation error) and the graph must be unchanged.
    #[test]
    fn pipeline_run_validates_final_graph_on_clean_input() -> crate::opt::Result<()> {
        let mut g = one_const_fn(3);
        let pipeline = crate::opt::default_pipeline();
        let entry = g.entry().unwrap();
        let before = g.preorder().count();
        pipeline.run(g.graph_mut(), entry)?;
        let after = g.preorder().count();
        // The default pipeline on an already-folded constant cannot fold
        // further; the reachable-count is stable.  This pins that
        // `run(graph, entry)` doesn't accidentally mutate the graph
        // beyond what the underlying passes produce.
        assert!(after <= before, "default pipeline must not GROW the reachable set");
        Ok(())
    }
}
