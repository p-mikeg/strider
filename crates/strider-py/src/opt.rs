//! `PyOptimizerPipeline` and one wrapper class per opt pass.
//!
//! The Rust `opt::OptimizerPipeline::add` is generic over the concrete
//! pass type (`O: Optimizer + 'static`) and stores it as
//! `Box<dyn Optimizer>` internally.  We can't directly stuff a
//! type-erased `Box<dyn Optimizer>` back into `add`, so the Python
//! wrapper accumulates erased boxes and, at run time, transfers them
//! into a fresh real pipeline via a small adapter that re-implements
//! `Optimizer` as a forwarder.

use pyo3::prelude::*;
use pyo3::types::PyType;
use std::sync::Mutex;

use crate::errors::into_strider_err;

/// Trait-object holder owning a heap-allocated `opt::Optimizer`.
/// The wrapper itself is `Send + Sync` so it can move across the
/// PyO3 boundary safely (Python objects are reachable from any
/// thread that holds the GIL).
pub(crate) type ErasedPass = Box<dyn opt::Optimizer + Send + Sync>;

/// Adapter that turns an owned `ErasedPass` into something
/// `opt::OptimizerPipeline::add` can accept.  `add` requires
/// `O: Optimizer + 'static`; this newtype satisfies both bounds and
/// forwards `optimize` straight through.
struct ForwardPass(ErasedPass);

impl opt::Optimizer for ForwardPass {
    fn optimize(
        &self,
        graph: &mut ir::Graph,
        entry: ir::node::NodeId,
    ) -> opt::Result<opt::OptimizationResult> {
        self.0.optimize(graph, entry)
    }
}

/// Internal builder representation: a list of fixed-point passes and
/// a list of post-passes, both as type-erased boxes.  Snapshot on
/// `run` materialises a real `opt::OptimizerPipeline` ad-hoc.
struct PipelineState {
    passes: Vec<ErasedPass>,
    post_passes: Vec<ErasedPass>,
}

impl PipelineState {
    fn new() -> Self {
        Self {
            passes: Vec::new(),
            post_passes: Vec::new(),
        }
    }

    fn from_default() -> Self {
        // Re-create the default pipeline by reconstructing each pass
        // individually rather than calling opt::default_pipeline()
        // (which returns an OptimizerPipeline whose Box<dyn Optimizer>
        // entries are not externally re-extractable).
        let mut s = Self::new();
        s.passes.push(Box::new(opt::ConstantFold));
        s.passes.push(Box::new(opt::KnownBits));
        s.passes.push(Box::new(opt::RedundantPhis));
        s.passes.push(Box::new(opt::DeadBranchElimination));
        s.passes.push(Box::new(opt::CallOtherElide));
        s
    }

    fn from_stable_default() -> Self {
        let mut s = Self::new();
        s.passes.push(Box::new(opt::ConstantFold));
        s.passes.push(Box::new(opt::KnownBits));
        s
    }

    fn from_destructive_default() -> Self {
        let mut s = Self::new();
        s.passes.push(Box::new(opt::RedundantPhis));
        s.passes.push(Box::new(opt::DeadBranchElimination));
        s.passes.push(Box::new(opt::CallOtherElide));
        s
    }
}

/// Python-visible builder for an `opt::OptimizerPipeline`.
///
/// Holds the internal state behind a `Mutex` so `add` / `add_post`
/// don't require `&mut self` (PyO3 method receivers are typically
/// `&self` for ergonomics).
#[pyclass(name = "OptimizerPipeline", module = "strider")]
pub struct PyOptimizerPipeline {
    state: Mutex<PipelineState>,
}

impl PyOptimizerPipeline {
    fn new_with(state: PipelineState) -> Self {
        Self {
            state: Mutex::new(state),
        }
    }

    /// Materialise a real `opt::OptimizerPipeline` from the current
    /// state.  Drains the internal pass lists — call once per
    /// "transfer" cycle and rebuild the wrapper afterwards if you
    /// need to keep it.
    pub(crate) fn drain_into_pipeline(&self) -> PyResult<opt::OptimizerPipeline> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| into_strider_err(anyhow::anyhow!("OptimizerPipeline lock poisoned")))?;
        let mut pipe = opt::OptimizerPipeline::new();
        for p in state.passes.drain(..) {
            pipe.add(ForwardPass(p));
        }
        for p in state.post_passes.drain(..) {
            pipe.add_post_pass(ForwardPass(p));
        }
        Ok(pipe)
    }
}

#[pymethods]
impl PyOptimizerPipeline {
    #[classmethod]
    fn empty(_cls: &Bound<'_, PyType>) -> Self {
        Self::new_with(PipelineState::new())
    }

    #[classmethod]
    fn default(_cls: &Bound<'_, PyType>) -> Self {
        Self::new_with(PipelineState::from_default())
    }

    #[classmethod]
    fn stable_default(_cls: &Bound<'_, PyType>) -> Self {
        Self::new_with(PipelineState::from_stable_default())
    }

    #[classmethod]
    fn destructive_default(_cls: &Bound<'_, PyType>) -> Self {
        Self::new_with(PipelineState::from_destructive_default())
    }

    fn add(&self, pass_obj: PyOptPass<'_>) -> PyResult<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| into_strider_err(anyhow::anyhow!("OptimizerPipeline lock poisoned")))?;
        state.passes.push(pass_obj.into_erased());
        Ok(())
    }

    fn add_post(&self, pass_obj: PyOptPass<'_>) -> PyResult<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| into_strider_err(anyhow::anyhow!("OptimizerPipeline lock poisoned")))?;
        state.post_passes.push(pass_obj.into_erased());
        Ok(())
    }

    fn pass_count(&self) -> PyResult<usize> {
        let state = self
            .state
            .lock()
            .map_err(|_| into_strider_err(anyhow::anyhow!("OptimizerPipeline lock poisoned")))?;
        Ok(state.passes.len())
    }

    fn post_pass_count(&self) -> PyResult<usize> {
        let state = self
            .state
            .lock()
            .map_err(|_| into_strider_err(anyhow::anyhow!("OptimizerPipeline lock poisoned")))?;
        Ok(state.post_passes.len())
    }
}

// ── Per-pass wrappers ──────────────────────────────────────────────────────
//
// One zero-sized class per pure (no-arg) Rust pass.  CC/arch-aware
// passes that need configuration land in a follow-up task.

#[pyclass(name = "ConstantFold", module = "strider.opt")]
pub struct PyConstantFold;
#[pymethods]
impl PyConstantFold {
    #[new]
    fn new() -> Self { Self }
}

#[pyclass(name = "KnownBits", module = "strider.opt")]
pub struct PyKnownBits;
#[pymethods]
impl PyKnownBits {
    #[new]
    fn new() -> Self { Self }
}

#[pyclass(name = "RedundantPhis", module = "strider.opt")]
pub struct PyRedundantPhis;
#[pymethods]
impl PyRedundantPhis {
    #[new]
    fn new() -> Self { Self }
}

#[pyclass(name = "DeadBranchElim", module = "strider.opt")]
pub struct PyDeadBranchElim;
#[pymethods]
impl PyDeadBranchElim {
    #[new]
    fn new() -> Self { Self }
}

#[pyclass(name = "CallOtherElide", module = "strider.opt")]
pub struct PyCallOtherElide;
#[pymethods]
impl PyCallOtherElide {
    #[new]
    fn new() -> Self { Self }
}

// ── Polymorphic enum used by add/add_post ──────────────────────────────────

/// Aggregates every pass-wrapper class so `add` / `add_post` can
/// accept any of them via PyO3's automatic enum dispatch.
#[derive(FromPyObject)]
#[allow(dead_code)] // The Bound payload is used only to drive variant selection.
pub enum PyOptPass<'py> {
    ConstantFold(Bound<'py, PyConstantFold>),
    KnownBits(Bound<'py, PyKnownBits>),
    RedundantPhis(Bound<'py, PyRedundantPhis>),
    DeadBranchElim(Bound<'py, PyDeadBranchElim>),
    CallOtherElide(Bound<'py, PyCallOtherElide>),
}

impl PyOptPass<'_> {
    fn into_erased(self) -> ErasedPass {
        match self {
            PyOptPass::ConstantFold(_) => Box::new(opt::ConstantFold),
            PyOptPass::KnownBits(_) => Box::new(opt::KnownBits),
            PyOptPass::RedundantPhis(_) => Box::new(opt::RedundantPhis),
            PyOptPass::DeadBranchElim(_) => Box::new(opt::DeadBranchElimination),
            PyOptPass::CallOtherElide(_) => Box::new(opt::CallOtherElide),
        }
    }
}

pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyOptimizerPipeline>()?;
    let m = PyModule::new_bound(py, "opt")?;
    m.add_class::<PyConstantFold>()?;
    m.add_class::<PyKnownBits>()?;
    m.add_class::<PyRedundantPhis>()?;
    m.add_class::<PyDeadBranchElim>()?;
    m.add_class::<PyCallOtherElide>()?;
    parent.add_submodule(&m)?;
    let sys = py.import_bound("sys")?;
    sys.getattr("modules")?.set_item("strider.opt", &m)?;
    Ok(())
}
