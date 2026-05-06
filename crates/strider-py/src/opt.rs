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
        // entries are not externally re-extractable).  Pass list and
        // order MUST mirror `opt::default_pipeline()` — drift here
        // silently produces graphs that look different from the
        // orchestrator path's, e.g. flag-cmp shapes the lifter emits
        // never get canonicalised and pattern queries miss them.
        let mut s = Self::new();
        s.passes.push(Box::new(opt::ConstantFold));
        s.passes.push(Box::new(opt::KnownBits));
        s.passes.push(Box::new(opt::FlagCmpCanonicalize));
        s.passes.push(Box::new(opt::IfCondInversion));
        s.passes.push(Box::new(opt::RedundantPhis));
        s.passes.push(Box::new(opt::DeadBranchElimination));
        s
    }

    fn from_stable_default() -> Self {
        // Mirrors `opt::stable_default_pipeline()` — see `from_default`
        // for why drift here is dangerous.
        let mut s = Self::new();
        s.passes.push(Box::new(opt::ConstantFold));
        s.passes.push(Box::new(opt::KnownBits));
        s.passes.push(Box::new(opt::FlagCmpCanonicalize));
        s.passes.push(Box::new(opt::IfCondInversion));
        s
    }

    fn from_destructive_default() -> Self {
        let mut s = Self::new();
        s.passes.push(Box::new(opt::RedundantPhis));
        s.passes.push(Box::new(opt::DeadBranchElimination));
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

    /// Build the convention-aware "full" pipeline mirroring
    /// `strider::Strider::build_optimizer_pipeline`.
    pub(crate) fn new_full_default(
        cc: target::BuiltCallingConvention,
        arch: target::SleighArch,
    ) -> Self {
        let mut state = PipelineState::from_default();
        state
            .passes
            .push(Box::new(opt::StackStoreDetect::from_convention(&cc)));
        state
            .passes
            .push(Box::new(opt::StackLoadForward::from_convention(&cc, &arch)));
        state
            .post_passes
            .push(Box::new(opt::CallStackArgCollect::from_convention(&cc)));
        state
            .post_passes
            .push(Box::new(opt::FunctionArgDetect::from_convention(&cc)));
        Self::new_with(state)
    }

    /// Build the stable-only pipeline mirroring
    /// `strider::Strider::build_stable_optimizer_pipeline`.
    pub(crate) fn new_stable_default(
        cc: target::BuiltCallingConvention,
        arch: target::SleighArch,
    ) -> Self {
        let mut state = PipelineState::from_stable_default();
        state
            .passes
            .push(Box::new(opt::StackStoreDetect::from_convention(&cc)));
        state
            .passes
            .push(Box::new(opt::StackLoadForward::from_convention(&cc, &arch)));
        state
            .post_passes
            .push(Box::new(opt::FunctionArgDetect::from_convention(&cc)));
        Self::new_with(state)
    }

    /// Build the destructive-only pipeline mirroring
    /// `strider::Strider::build_destructive_optimizer_pipeline`.
    pub(crate) fn new_destructive_default(cc: target::BuiltCallingConvention) -> Self {
        let mut state = PipelineState::from_destructive_default();
        state
            .post_passes
            .push(Box::new(opt::CallStackArgCollect::from_convention(&cc)));
        Self::new_with(state)
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

#[pyclass(name = "FlagCmpCanonicalize", module = "strider.opt")]
pub struct PyFlagCmpCanonicalize;
#[pymethods]
impl PyFlagCmpCanonicalize {
    #[new]
    fn new() -> Self { Self }
}

#[pyclass(name = "IfCondInversion", module = "strider.opt")]
pub struct PyIfCondInversion;
#[pymethods]
impl PyIfCondInversion {
    #[new]
    fn new() -> Self { Self }
}

// ── CC/arch-aware passes ──────────────────────────────────────────────────
//
// Each takes (sleigh, cc) — or (sleigh, cc, arch) — at construction
// time, builds a target::BuiltCallingConvention against the Sleigh's
// register table, and stores the concrete pre-configured pass.

/// `StackStoreDetect(sleigh, cc)`
#[pyclass(name = "StackStoreDetect", module = "strider.opt")]
pub struct PyStackStoreDetect {
    pub(crate) inner: opt::StackStoreDetect,
}
#[pymethods]
impl PyStackStoreDetect {
    #[new]
    fn new(
        py: Python<'_>,
        sleigh: Py<crate::sleigh::PySleigh>,
        cc: crate::cc::PyCallingConvention,
    ) -> PyResult<Self> {
        let sleigh_borrow = sleigh.borrow(py);
        let regs = sleigh_borrow.regs.clone();
        drop(sleigh_borrow);
        let built_cc = cc
            .inner
            .build(&regs)
            .map_err(crate::errors::into_lift_err)?;
        Ok(Self {
            inner: opt::StackStoreDetect::from_convention(&built_cc),
        })
    }
}

/// `StackLoadForward(sleigh, cc, arch)`
#[pyclass(name = "StackLoadForward", module = "strider.opt")]
pub struct PyStackLoadForward {
    pub(crate) inner: opt::StackLoadForward,
}
#[pymethods]
impl PyStackLoadForward {
    #[new]
    fn new(
        py: Python<'_>,
        sleigh: Py<crate::sleigh::PySleigh>,
        cc: crate::cc::PyCallingConvention,
        arch: crate::arch::PySleighArch,
    ) -> PyResult<Self> {
        let sleigh_borrow = sleigh.borrow(py);
        let regs = sleigh_borrow.regs.clone();
        drop(sleigh_borrow);
        let built_cc = cc
            .inner
            .build(&regs)
            .map_err(crate::errors::into_lift_err)?;
        Ok(Self {
            inner: opt::StackLoadForward::from_convention(&built_cc, &arch.inner),
        })
    }
}

/// `FunctionArgDetect(sleigh, cc)`
#[pyclass(name = "FunctionArgDetect", module = "strider.opt")]
pub struct PyFunctionArgDetect {
    pub(crate) inner: opt::FunctionArgDetect,
}
#[pymethods]
impl PyFunctionArgDetect {
    #[new]
    fn new(
        py: Python<'_>,
        sleigh: Py<crate::sleigh::PySleigh>,
        cc: crate::cc::PyCallingConvention,
    ) -> PyResult<Self> {
        let sleigh_borrow = sleigh.borrow(py);
        let regs = sleigh_borrow.regs.clone();
        drop(sleigh_borrow);
        let built_cc = cc
            .inner
            .build(&regs)
            .map_err(crate::errors::into_lift_err)?;
        Ok(Self {
            inner: opt::FunctionArgDetect::from_convention(&built_cc),
        })
    }
}

/// `CallStackArgCollect(sleigh, cc)`
#[pyclass(name = "CallStackArgCollect", module = "strider.opt")]
pub struct PyCallStackArgCollect {
    pub(crate) inner: opt::CallStackArgCollect,
}
#[pymethods]
impl PyCallStackArgCollect {
    #[new]
    fn new(
        py: Python<'_>,
        sleigh: Py<crate::sleigh::PySleigh>,
        cc: crate::cc::PyCallingConvention,
    ) -> PyResult<Self> {
        let sleigh_borrow = sleigh.borrow(py);
        let regs = sleigh_borrow.regs.clone();
        drop(sleigh_borrow);
        let built_cc = cc
            .inner
            .build(&regs)
            .map_err(crate::errors::into_lift_err)?;
        Ok(Self {
            inner: opt::CallStackArgCollect::from_convention(&built_cc),
        })
    }
}

/// `LoadReadOnly(rom)` — `rom` is a `MemoryMap` or any
/// `ReadOnlyMemory` subclass (callback path).  Internally stored as
/// `Arc<dyn ReadOnlyMemory>` so both the fast path and the callback
/// path share one wrapper class.
#[pyclass(name = "LoadReadOnly", module = "strider.opt")]
pub struct PyLoadReadOnly {
    pub(crate) rom: std::sync::Arc<dyn opt::ReadOnlyMemory>,
}
#[pymethods]
impl PyLoadReadOnly {
    #[new]
    fn new(rom: crate::reader::RomInput) -> Self {
        Self { rom: rom.into_arc() }
    }
}

// ── Polymorphic enum used by add/add_post ──────────────────────────────────

/// Aggregates every pass-wrapper class so `add` / `add_post` can
/// accept any of them via PyO3's automatic enum dispatch.  The
/// `Bound<'py, _>` payload is consumed by `FromPyObject`'s
/// macro-generated dispatcher to pick the right variant; for the
/// zero-sized passes we never read it back, hence the `dead_code`
/// allow on those variants.
#[derive(FromPyObject)]
pub enum PyOptPass<'py> {
    #[allow(dead_code)]
    ConstantFold(Bound<'py, PyConstantFold>),
    #[allow(dead_code)]
    KnownBits(Bound<'py, PyKnownBits>),
    #[allow(dead_code)]
    RedundantPhis(Bound<'py, PyRedundantPhis>),
    #[allow(dead_code)]
    DeadBranchElim(Bound<'py, PyDeadBranchElim>),
    #[allow(dead_code)]
    FlagCmpCanonicalize(Bound<'py, PyFlagCmpCanonicalize>),
    #[allow(dead_code)]
    IfCondInversion(Bound<'py, PyIfCondInversion>),
    StackStoreDetect(Bound<'py, PyStackStoreDetect>),
    StackLoadForward(Bound<'py, PyStackLoadForward>),
    FunctionArgDetect(Bound<'py, PyFunctionArgDetect>),
    CallStackArgCollect(Bound<'py, PyCallStackArgCollect>),
    LoadReadOnly(Bound<'py, PyLoadReadOnly>),
}

impl PyOptPass<'_> {
    fn into_erased(self) -> ErasedPass {
        match self {
            PyOptPass::ConstantFold(_) => Box::new(opt::ConstantFold),
            PyOptPass::KnownBits(_) => Box::new(opt::KnownBits),
            PyOptPass::RedundantPhis(_) => Box::new(opt::RedundantPhis),
            PyOptPass::DeadBranchElim(_) => Box::new(opt::DeadBranchElimination),
            PyOptPass::FlagCmpCanonicalize(_) => Box::new(opt::FlagCmpCanonicalize),
            PyOptPass::IfCondInversion(_) => Box::new(opt::IfCondInversion),
            PyOptPass::StackStoreDetect(b) => Box::new(b.borrow().inner.clone()),
            PyOptPass::StackLoadForward(b) => Box::new(b.borrow().inner.clone()),
            PyOptPass::FunctionArgDetect(b) => Box::new(b.borrow().inner.clone()),
            PyOptPass::CallStackArgCollect(b) => Box::new(b.borrow().inner.clone()),
            PyOptPass::LoadReadOnly(b) => {
                Box::new(opt::LoadReadOnly(std::sync::Arc::clone(&b.borrow().rom)))
            }
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
    m.add_class::<PyFlagCmpCanonicalize>()?;
    m.add_class::<PyIfCondInversion>()?;
    m.add_class::<PyStackStoreDetect>()?;
    m.add_class::<PyStackLoadForward>()?;
    m.add_class::<PyFunctionArgDetect>()?;
    m.add_class::<PyCallStackArgCollect>()?;
    m.add_class::<PyLoadReadOnly>()?;
    parent.add_submodule(&m)?;
    let sys = py.import_bound("sys")?;
    sys.getattr("modules")?.set_item("strider.opt", &m)?;
    Ok(())
}
