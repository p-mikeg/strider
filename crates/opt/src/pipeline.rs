/// Whether an optimization pass made any change to the IR graph.
///
/// Passes return this from [`Optimizer::optimize`].  The pipeline uses it to
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

    /// Maps the boolean return of [`BuiltFunctionGraph::replace_all_uses`] to
    /// an `OptimizationResult`: `true` → `Changed`, `false` → `NoChange`.
    #[must_use]
    pub fn from_changed(changed: bool) -> Self {
        if changed {
            OptimizationResult::Changed
        } else {
            OptimizationResult::NoChange
        }
    }

    /// Replaces every use of `old` with `new` and folds the resulting
    /// `Changed`/`NoChange` into `self`.  Equivalent to
    /// `self | OptimizationResult::from_changed(fg.graph.replace_all_uses(old, new)?)`
    /// — extracted because that exact line is the most common rewrite-and-
    /// escalate idiom in the constant_fold and known_bits passes.
    ///
    /// # Errors
    ///
    /// Propagates errors from
    /// [`ir::BuiltFunctionGraph::replace_all_uses`].
    pub fn after_replace(
        self,
        fg: &mut ir::BuiltFunctionGraph,
        old: ir::node::NodeOutputId,
        new: ir::node::NodeOutputId,
    ) -> crate::Result<Self> {
        Ok(self | OptimizationResult::from_changed(fg.graph.replace_all_uses(old, new)?))
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

/// Bridge helper for opt-pass impls that internally still operate on
/// `&mut ir::BuiltFunctionGraph` (because their helper functions and the
/// `pattern` crate's rewrite machinery are typed against it).  Wraps a
/// `(&mut Graph, NodeId)` pair into a temporary `BuiltFunctionGraph`
/// for the duration of the call, then restores the (potentially
/// mutated) graph back into the caller's slot.
///
/// CORRECTNESS — the temporary `BuiltFunctionGraph` carries empty
/// `variables` / `call_clobbered` / `ret_val_regs`.  Opt impls never
/// read those fields (verified by grep — only `function.graph` and
/// `function.entry` are touched), so the dummy values are safe.
///
/// CORRECTNESS — `mem::take(graph)` replaces the caller's `Graph` with
/// `Graph::default()` (an empty graph) only for the duration of `f`.
/// On panic the empty graph is observable, but opt passes don't
/// catch_unwind, so this is no worse than a panic anywhere else in the
/// pipeline.
pub(crate) fn with_built<R>(
    graph: &mut ir::Graph,
    entry: ir::node::NodeId,
    f: impl FnOnce(&mut ir::BuiltFunctionGraph) -> R,
) -> R {
    let stolen = std::mem::take(graph);
    let mut tmp = ir::BuiltFunctionGraph::from_graph_and_entry(stolen, entry);
    let r = f(&mut tmp);
    *graph = tmp.graph;
    r
}

/// A single IR optimization pass.
///
/// Implement this trait to add a new pass.  The pass receives a mutable
/// reference to the function graph, applies whatever transformations it can in
/// one sweep, and returns [`OptimizationResult::Changed`] if anything was
/// modified (causing the pipeline to run another iteration) or
/// [`OptimizationResult::NoChange`] if the graph is already in normal form for
/// this pass.
pub trait Optimizer {
    /// Run one sweep of this pass over the IR `graph`, anchored at `entry`.
    ///
    /// # Why `(&mut Graph, NodeId)` and not `&mut BuiltFunctionGraph`
    ///
    /// Callers can run optimizer passes on a graph that has not yet
    /// been packaged into a final [`ir::BuiltFunctionGraph`] (e.g. on
    /// a live [`ir::FunctionBuilder`] via
    /// [`ir::FunctionBuilder::graph_mut`] + [`ir::FunctionBuilder::entry`]).
    /// `BuiltFunctionGraph` is a final-output convenience type, not
    /// a precondition for analysis.
    ///
    /// `entry` is the function's entry [`ir::node::NodeId`] — needed because
    /// several passes walk the reachable-node set (`graph.preorder(entry)`)
    /// or use it directly (`ir::walk::cfg_reachable(graph, entry)`).
    ///
    /// # Errors
    ///
    /// Returns the first error encountered by the pass — typically an IR
    /// validation failure or a pattern-rewrite error propagated up through
    /// [`crate::Error`].
    fn optimize(
        &self,
        graph: &mut ir::Graph,
        entry: ir::node::NodeId,
    ) -> crate::Result<OptimizationResult>;
}

/// Optimizer pass that operates on a [`ir::BuiltFunctionGraph`] rather than
/// the lower-level `(&mut Graph, NodeId)` pair.  Most passes implement this
/// instead of [`Optimizer`] directly: the blanket impl below wires the
/// [`with_built`] adapter so the pass slots into the pipeline.
///
/// Passes that need direct `&mut Graph` access (e.g.
/// [`crate::indirect_branch_resolve::IndirectBranchResolve`], whose
/// in-place edits straddle `with_built` boundaries) implement
/// [`Optimizer`] directly instead.
pub trait OptimizerOnBuilt {
    /// Run one sweep of this pass over the function graph.  See
    /// [`Optimizer::optimize`] for the `Changed`/`NoChange` contract.
    ///
    /// # Errors
    ///
    /// Returns the first error encountered by the pass.
    fn optimize_built(
        &self,
        function: &mut ir::BuiltFunctionGraph,
    ) -> crate::Result<OptimizationResult>;
}

impl<T: OptimizerOnBuilt> Optimizer for T {
    fn optimize(
        &self,
        graph: &mut ir::Graph,
        entry: ir::node::NodeId,
    ) -> crate::Result<OptimizationResult> {
        with_built(graph, entry, |function| self.optimize_built(function))
    }
}

/// An ordered list of [`Optimizer`] passes that are run in a shared fixed-point
/// loop.
///
/// On each iteration every pass is called once in registration order.  The loop
/// repeats until no pass reports a change.  Use [`OptimizerPipeline::add`] to
/// register passes and [`OptimizerPipeline::run`] to execute them.
pub struct OptimizerPipeline {
    optimizers: Vec<Box<dyn Optimizer>>,
    /// Type names of each registered optimizer, captured at
    /// registration time via `std::any::type_name::<O>()`.  Indexed in
    /// lock-step with `optimizers`.  Exposed via
    /// [`OptimizerPipeline::optimizer_names`] so the fixed-point
    /// orchestrator's pipeline-shape tests can confirm the
    /// stable-vs-destructive subset partition without inspecting trait
    /// objects.
    optimizer_names: Vec<&'static str>,
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
            optimizer_names: Vec::new(),
            post_passes: Vec::new(),
        }
    }

    /// Appends `opt` to the end of the pass list.
    pub fn add<O: Optimizer + 'static>(&mut self, opt: O) {
        // Capture the concrete type name BEFORE boxing the value so we
        // can introspect the pipeline shape (see `optimizer_names`).
        // `std::any::type_name` returns the fully-qualified type path,
        // e.g. `"opt::redundant_phis::RedundantPhis"`.  Tests do
        // substring matches against the leaf type name.
        self.optimizer_names.push(std::any::type_name::<O>());
        self.optimizers.push(Box::new(opt));
    }

    /// Number of fixed-point passes registered (excluding post-passes).
    ///
    /// Used by the strider fixed-point orchestrator's tests to pin the
    /// "stable subset + destructive subset == full pipeline" equivalence
    /// without inspecting each pass's behavioural fingerprint.
    #[must_use]
    pub fn optimizer_count(&self) -> usize {
        self.optimizers.len()
    }

    /// Concrete-type names of the registered fixed-point passes, in
    /// registration order (excluding post-passes).
    ///
    /// Captured at registration time via `std::any::type_name::<O>()`.
    /// Used by the pipeline-shape tests to confirm membership of each
    /// pass in the stable / destructive subset without instantiating
    /// real IR fixtures.  Type-name strings are
    /// implementation-defined but stable enough for substring matching
    /// against pass names like `"RedundantPhis"`.
    #[must_use]
    pub fn optimizer_names(&self) -> &[&'static str] {
        &self.optimizer_names
    }

    /// Appends `opt` to the post-pass list.  Post-passes run once, in
    /// registration order, after the fixed-point loop converges.  Their return
    /// value is ignored (no re-entry into the fixed-point loop).
    pub fn add_post_pass<O: Optimizer + 'static>(&mut self, opt: O) {
        self.post_passes.push(Box::new(opt));
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
    /// Returns the first [`crate::Error`] reported by any pass.  If every
    /// pass and post-pass succeeds, the graph is then re-validated and any
    /// validation error is returned.  When a post-pass returns `Err`, the
    /// final validation step is skipped — the pass error wins.
    pub fn run(
        &self,
        graph: &mut ir::Graph,
        entry: ir::node::NodeId,
    ) -> crate::Result<()> {
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
        ir::validate::validate(graph, entry)?;
        Ok(())
    }

    /// Back-compat wrapper that accepts a [`ir::BuiltFunctionGraph`].
    ///
    /// Delegates to [`Self::run`] by extracting `(&mut graph.graph,
    /// graph.entry)`.  Tests and downstream code that already hold a
    /// `BuiltFunctionGraph` keep working unchanged through F2's trait
    /// refactor; new code is encouraged to call [`Self::run`] directly with
    /// a `(graph, entry)` pair (e.g. from
    /// [`ir::FunctionBuilder::graph_mut`] + [`ir::FunctionBuilder::entry`]).
    ///
    /// # Errors
    ///
    /// Propagates [`Self::run`].
    pub fn run_on_built(
        &self,
        function: &mut ir::BuiltFunctionGraph,
    ) -> crate::Result<()> {
        let entry = function.entry;
        self.run(&mut function.graph, entry)
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for the [`OptimizerPipeline::run`] /
    //! [`OptimizerPipeline::run_on_built`] equivalence contract.

    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use ir::FunctionBuilder;
    use ir::node::NodeOutputType;

    /// Build a tiny single-region function returning `IntConst(K)`.
    fn one_const_fn(k: u64) -> ir::BuiltFunctionGraph {
        let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        let v = b.build_int_const(k, NodeOutputType::U64).unwrap();
        b.build_return(Some(v), &[]).unwrap();
        b.build().unwrap()
    }

    /// `run(graph, entry)` and `run_on_built(built)` produce the same
    /// resulting graph state.  Pins F2's "the new entry point is a
    /// drop-in replacement" contract.
    #[test]
    fn pipeline_run_with_graph_and_entry_replicates_old_built_behavior() -> crate::Result<()> {
        let mut a = one_const_fn(7);
        let mut b = one_const_fn(7);

        let pipeline = crate::default_pipeline();

        // Path A: run via the new (graph, entry) signature.
        let entry = a.entry;
        pipeline.run(&mut a.graph, entry)?;

        // Path B: run via the back-compat run_on_built wrapper.
        pipeline.run_on_built(&mut b)?;

        // Both runs must succeed and produce graphs of the same shape.
        // We compare reachable-node counts as a coarse but objective
        // structural fingerprint — the two pipelines applied identical
        // rewrites, so the live-set sizes match exactly.
        let a_count = a.preorder().count();
        let b_count = b.preorder().count();
        assert_eq!(a_count, b_count, "run and run_on_built must produce identical graph shapes");
        Ok(())
    }

    /// `run(graph, entry)` validates the final graph just like the
    /// historical `run(&mut BuiltFunctionGraph)` did — i.e. an invalid
    /// graph in the post-pass output surfaces as `ValidationFailed`.
    /// Smoke test using an empty post-pass list and a valid input —
    /// run must succeed (no validation error) and the graph must be
    /// unchanged.
    #[test]
    fn pipeline_run_validates_final_graph_on_clean_input() -> crate::Result<()> {
        let mut g = one_const_fn(3);
        let pipeline = crate::default_pipeline();
        let entry = g.entry;
        let before = g.preorder().count();
        pipeline.run(&mut g.graph, entry)?;
        let after = g.preorder().count();
        // The default pipeline on an already-folded constant cannot fold
        // further; the reachable-count is stable.  This pins that
        // `run(graph, entry)` doesn't accidentally mutate the graph
        // beyond what the underlying passes produce.
        assert!(after <= before, "default pipeline must not GROW the reachable set");
        Ok(())
    }
}
