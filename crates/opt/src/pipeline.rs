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
/// Implement this trait to add a new pass.  The pass receives a mutable
/// reference to the function graph, applies whatever transformations it can in
/// one sweep, and returns [`OptimizationResult::Changed`] if anything was
/// modified (causing the pipeline to run another iteration) or
/// [`OptimizationResult::NoChange`] if the graph is already in normal form for
/// this pass.
pub trait Optimizer {
    /// Run one sweep of this pass over `function`.
    ///
    /// # Errors
    ///
    /// Returns the first error encountered by the pass — typically an IR
    /// validation failure or a pattern-rewrite error propagated up through
    /// [`crate::Error`].
    fn optimize(&self, function: &mut ir::BuiltFunctionGraph) -> crate::Result<OptimizationResult>;

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
    /// Returns the first [`crate::Error`] reported by any pass, or the
    /// final-validate error from `ir::validate::validate` if the post-pass
    /// run leaves an invalid graph.
    pub fn run(&self, graph: &mut ir::BuiltFunctionGraph) -> crate::Result<()> {
        const MAX_ITERS: u32 = 1024;
        let mut iters: u32 = 0;
        loop {
            let mut changed = false;
            for opt in &self.optimizers {
                if opt.optimize(graph)?.changed() {
                    changed = true;
                }
            }
            if !changed {
                break;
            }
            iters += 1;
            if iters >= MAX_ITERS {
                return Err(crate::error::ErrorKind::FixedPointLimitExceeded(MAX_ITERS).into());
            }
        }
        for opt in &self.post_passes {
            opt.optimize(graph)?;
        }
        ir::validate::validate(&graph.graph, graph.entry)?;
        Ok(())
    }
}
