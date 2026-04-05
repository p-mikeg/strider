#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptimizationResult {
    NoChange,
    Changed,
}

impl OptimizationResult {
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

pub trait Optimizer {
    fn optimize(&self, function: &mut ir::BuiltFunctionGraph) -> OptimizationResult;
}

pub struct OptimizerPipeline {
    optimizers: Vec<Box<dyn Optimizer>>,
}

impl OptimizerPipeline {
    pub fn new() -> Self {
        Self {
            optimizers: Vec::new(),
        }
    }

    pub fn add<O: Optimizer + 'static>(&mut self, opt: O) {
        self.optimizers.push(Box::new(opt));
    }

    pub fn run(&self, graph: &mut ir::BuiltFunctionGraph) {
        loop {
            let mut changed = false;

            for opt in &self.optimizers {
                if opt.optimize(graph).changed() {
                    changed = true;
                }
            }

            if !changed {
                break;
            }
        }
    }
}