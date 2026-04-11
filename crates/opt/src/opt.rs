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
    pub fn changed(self) -> bool {
        matches!(self, OptimizationResult::Changed)
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
}

impl OptimizerPipeline {
    /// Creates an empty pipeline with no passes registered.
    pub fn new() -> Self {
        Self {
            optimizers: Vec::new(),
        }
    }

    /// Appends `opt` to the end of the pass list.
    pub fn add<O: Optimizer + 'static>(&mut self, opt: O) {
        self.optimizers.push(Box::new(opt));
    }

    /// Runs all registered passes in a fixed-point loop until convergence.
    ///
    /// Returns `Ok(())` when no pass changed the graph in a full iteration.
    /// Propagates the first error returned by any pass.
    pub fn run(&self, graph: &mut ir::BuiltFunctionGraph) -> crate::Result<()> {
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
        }
        Ok(())
    }
}