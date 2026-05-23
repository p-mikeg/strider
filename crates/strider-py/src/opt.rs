//! `PyOptimizerPipeline` and one wrapper class per opt pass.
//!
//! The Rust `strider_analyze::opt::OptimizerPipeline::add` is generic over the concrete
//! pass type (`O: Optimizer + 'static`) and stores it as
//! `Box<dyn Optimizer>` internally.  We can't directly stuff a
//! type-erased `Box<dyn Optimizer>` back into `add`, so the Python
//! wrapper accumulates erased boxes and, at run time, transfers them
//! into a fresh real pipeline via a small adapter that re-implements
//! `Optimizer` as a forwarder.

use pyo3::prelude::*;
use pyo3::types::PyType;
use std::sync::{Mutex, MutexGuard};

use crate::errors::into_strider_err;

/// Trait-object holder owning a heap-allocated `strider_analyze::opt::Optimizer`.
/// The wrapper itself is `Send + Sync` so it can move across the
/// PyO3 boundary safely (Python objects are reachable from any
/// thread that holds the GIL).
pub(crate) type ErasedPass = Box<dyn strider_analyze::opt::Optimizer + Send + Sync>;

/// Adapter that turns an owned `ErasedPass` into something
/// `strider_analyze::opt::OptimizerPipeline::add` can accept.  `add` requires
/// `O: Optimizer + 'static`; this newtype satisfies both bounds and
/// forwards `optimize` straight through.
struct ForwardPass(ErasedPass);

impl Clone for ForwardPass {
    fn clone(&self) -> Self {
        // The wrapped pass owns its own clone strategy via `OptimizerClone`
        // (the supertrait of `Optimizer`).  Forwarding to it rather than
        // cloning the `Box` itself preserves the concrete pass type.
        // `Optimizer: Send + Sync` so the resulting `Box<dyn Optimizer>`
        // satisfies `ErasedPass`'s `Send + Sync` bound automatically.
        ForwardPass(self.0.clone_box())
    }
}

impl strider_analyze::opt::Optimizer for ForwardPass {
    fn optimize(
        &self,
        graph: &mut strider_ir::Graph,
        entry: strider_ir::node::NodeId,
    ) -> strider_analyze::opt::Result<strider_analyze::opt::OptimizationResult> {
        self.0.optimize(graph, entry)
    }
}

/// Internal builder representation: a list of fixed-point passes and
/// a list of post-passes, both as type-erased boxes.  Snapshot on
/// `run` materialises a real `strider_analyze::opt::OptimizerPipeline` ad-hoc.
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

    /// Snapshot a canonical `strider_analyze` pipeline into the wrapper's
    /// internal representation by `clone_box`-ing each pass.
    ///
    /// Iterating the canonical pipeline rather than hand-mirroring it
    /// makes drift between the Python wrapper and every Rust-side
    /// pipeline factory — `default_pipeline()` /
    /// `stable_default_pipeline()` / `destructive_default_pipeline()`
    /// and the CC-aware `Strider::build_optimizer_pipeline` /
    /// `build_stable_optimizer_pipeline` /
    /// `build_destructive_optimizer_pipeline` — structurally impossible.
    fn snapshot_from(pipeline: &strider_analyze::opt::OptimizerPipeline) -> Self {
        let mut s = Self::new();
        for pass in pipeline.passes() {
            s.passes.push(pass.clone_box());
        }
        for pass in pipeline.post_passes() {
            s.post_passes.push(pass.clone_box());
        }
        s
    }

    fn from_default() -> Self {
        Self::snapshot_from(&strider_analyze::opt::default_pipeline())
    }

    fn from_stable_default() -> Self {
        Self::snapshot_from(&strider_analyze::opt::stable_default_pipeline())
    }

    fn from_destructive_default() -> Self {
        Self::snapshot_from(&strider_analyze::opt::destructive_default_pipeline())
    }
}

/// Python-visible builder for an `strider_analyze::opt::OptimizerPipeline`.
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

    /// Acquire the internal `PipelineState` lock, converting a
    /// poisoned-lock error into the standard `StriderError` so every
    /// pyclass method can `?` it uniformly.
    fn lock_state(&self) -> PyResult<MutexGuard<'_, PipelineState>> {
        self.state
            .lock()
            .map_err(|_| into_strider_err(anyhow::anyhow!("OptimizerPipeline lock poisoned")))
    }

    /// Build the convention-aware "full" pipeline by delegating to
    /// `strider_analyze::Strider::build_optimizer_pipeline` and snapshotting
    /// its passes.  Iterating the canonical Rust pipeline rather than
    /// hand-mirroring it makes drift between the Python wrapper and
    /// `Strider::build_optimizer_pipeline` structurally impossible.
    pub(crate) fn new_full_default(strider: &strider_analyze::Strider) -> Self {
        let pipeline = strider.build_optimizer_pipeline();
        Self::new_with(PipelineState::snapshot_from(&pipeline))
    }

    /// Build the stable-only pipeline by delegating to
    /// `strider_analyze::Strider::build_stable_optimizer_pipeline` and
    /// snapshotting its passes.
    pub(crate) fn new_stable_default(strider: &strider_analyze::Strider) -> Self {
        let pipeline = strider.build_stable_optimizer_pipeline();
        Self::new_with(PipelineState::snapshot_from(&pipeline))
    }

    /// Build the destructive-only pipeline by delegating to
    /// `strider_analyze::Strider::build_destructive_optimizer_pipeline` and
    /// snapshotting its passes.
    pub(crate) fn new_destructive_default(strider: &strider_analyze::Strider) -> Self {
        let pipeline = strider.build_destructive_optimizer_pipeline();
        Self::new_with(PipelineState::snapshot_from(&pipeline))
    }

    /// Materialise a real `strider_analyze::opt::OptimizerPipeline` from the current
    /// state.  Drains the internal pass lists — call once per
    /// "transfer" cycle and rebuild the wrapper afterwards if you
    /// need to keep it.
    ///
    /// returns `Err(StriderError)` if
    /// the wrapper has already been drained (both pass lists empty).
    /// Without this guard a second `Graph.optimize(pipe)` would
    /// silently run an empty pipeline and report success — masking
    /// caller bugs where the same wrapper is reused after a previous
    /// `optimize` / `strider.run` consumed it.
    pub(crate) fn drain_into_pipeline(&self) -> PyResult<strider_analyze::opt::OptimizerPipeline> {
        let mut state = self.lock_state()?;
        if state.passes.is_empty() && state.post_passes.is_empty() {
            return Err(into_strider_err(anyhow::anyhow!(
                "OptimizerPipeline is empty — already drained by a prior \
                 Graph.optimize() / strider.run().  Build a fresh pipeline \
                 (e.g. OptimizerPipeline.default()) or re-add passes before \
                 calling again."
            )));
        }
        let mut pipe = strider_analyze::opt::OptimizerPipeline::new();
        for p in state.passes.drain(..) {
            pipe.add(ForwardPass(p));
        }
        for p in state.post_passes.drain(..) {
            pipe.add_post_pass(ForwardPass(p));
        }
        Ok(pipe)
    }

    /// Prepend a `LoadReadOnly` pass to the front of the pipeline's
    /// pass list.  Used by `run_with_custom_pipeline` to wire a
    /// user-supplied `rom` into the pipeline before draining it
    /// (otherwise the rom is silently discarded).
    pub(crate) fn prepend_load_read_only(
        &self,
        rom: std::sync::Arc<dyn strider_analyze::opt::ReadOnlyMemory>,
    ) -> PyResult<()> {
        let mut state = self.lock_state()?;
        let pass: ErasedPass = Box::new(strider_analyze::opt::LoadReadOnly::new(rom));
        state.passes.insert(0, pass);
        Ok(())
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
        let mut state = self.lock_state()?;
        state.passes.push(pass_obj.into_erased());
        Ok(())
    }

    fn add_post(&self, pass_obj: PyOptPass<'_>) -> PyResult<()> {
        let mut state = self.lock_state()?;
        state.post_passes.push(pass_obj.into_erased());
        Ok(())
    }

    fn pass_count(&self) -> PyResult<usize> {
        let state = self.lock_state()?;
        Ok(state.passes.len())
    }

    fn post_pass_count(&self) -> PyResult<usize> {
        let state = self.lock_state()?;
        Ok(state.post_passes.len())
    }
}

// ── Per-pass wrappers ──────────────────────────────────────────────────────
//
// One zero-sized class per pure (no-arg) Rust pass.  CC/arch-aware
// passes that need configuration land in a follow-up task.
//
// `pure_pass_class!` collapses the 5-line zero-sized-struct + #[new]
// boilerplate that each pass would otherwise repeat verbatim.  The
// macro emits a `pub struct Py<Name>` plus a `#[pymethods]` block with
// a single `#[new] fn new() -> Self { Self }`.

macro_rules! pure_pass_class {
    ($pyname:literal => $rust:ident) => {
        #[pyclass(name = $pyname, module = "strider.opt")]
        #[derive(Clone)]
        pub struct $rust;
        #[pymethods]
        impl $rust {
            #[new]
            fn new() -> Self { Self }
        }
    };
}

pure_pass_class!("ConstantFold" => PyConstantFold);
pure_pass_class!("KnownBits" => PyKnownBits);
pure_pass_class!("RedundantPhis" => PyRedundantPhis);
pure_pass_class!("DeadBranchElim" => PyDeadBranchElim);
pure_pass_class!("FlagCmpCanonicalize" => PyFlagCmpCanonicalize);
pure_pass_class!("IfCondInversion" => PyIfCondInversion);

// ── CC/arch-aware passes ──────────────────────────────────────────────────
//
// Each takes (sleigh, cc) — or (sleigh, cc, arch) — at construction
// time, builds a strider_target::BuiltCallingConvention against the Sleigh's
// register table, and stores the concrete pre-configured pass.

/// `StackStoreDetect(sleigh, cc)`
#[pyclass(name = "StackStoreDetect", module = "strider.opt")]
pub struct PyStackStoreDetect {
    pub(crate) inner: strider_analyze::opt::StackStoreDetect,
}
#[pymethods]
impl PyStackStoreDetect {
    #[new]
    fn new(
        py: Python<'_>,
        sleigh: Py<crate::sleigh::PySleigh>,
        cc: crate::cc::PyCallingConvention,
    ) -> PyResult<Self> {
        let built_cc = crate::cc::build_cc_for_sleigh(py, &sleigh, &cc)?;
        Ok(Self {
            inner: strider_analyze::opt::StackStoreDetect::from_convention(&built_cc),
        })
    }
}

/// `StackLoadForward(sleigh, cc, arch)`
#[pyclass(name = "StackLoadForward", module = "strider.opt")]
pub struct PyStackLoadForward {
    pub(crate) inner: strider_analyze::opt::StackLoadForward,
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
        let built_cc = crate::cc::build_cc_for_sleigh(py, &sleigh, &cc)?;
        Ok(Self {
            inner: strider_analyze::opt::StackLoadForward::from_convention(&built_cc, &arch.inner),
        })
    }
}

/// `FunctionArgDetect(sleigh, cc)`
#[pyclass(name = "FunctionArgDetect", module = "strider.opt")]
pub struct PyFunctionArgDetect {
    pub(crate) inner: strider_analyze::opt::FunctionArgDetect,
}
#[pymethods]
impl PyFunctionArgDetect {
    #[new]
    fn new(
        py: Python<'_>,
        sleigh: Py<crate::sleigh::PySleigh>,
        cc: crate::cc::PyCallingConvention,
    ) -> PyResult<Self> {
        let built_cc = crate::cc::build_cc_for_sleigh(py, &sleigh, &cc)?;
        Ok(Self {
            inner: strider_analyze::opt::FunctionArgDetect::from_convention(&built_cc),
        })
    }
}

/// `CallStackArgCollect(sleigh, cc)`
#[pyclass(name = "CallStackArgCollect", module = "strider.opt")]
pub struct PyCallStackArgCollect {
    pub(crate) inner: strider_analyze::opt::CallStackArgCollect,
}
#[pymethods]
impl PyCallStackArgCollect {
    #[new]
    fn new(
        py: Python<'_>,
        sleigh: Py<crate::sleigh::PySleigh>,
        cc: crate::cc::PyCallingConvention,
    ) -> PyResult<Self> {
        let built_cc = crate::cc::build_cc_for_sleigh(py, &sleigh, &cc)?;
        Ok(Self {
            inner: strider_analyze::opt::CallStackArgCollect::from_convention(&built_cc),
        })
    }
}

/// `LoadReadOnly(rom)` — `rom` is a `MemoryMap` or any
/// `ReadOnlyMemory` subclass (callback path).  Internally stored as
/// `Arc<dyn ReadOnlyMemory>` so both the fast path and the callback
/// path share one wrapper class.
#[pyclass(name = "LoadReadOnly", module = "strider.opt")]
pub struct PyLoadReadOnly {
    pub(crate) rom: std::sync::Arc<dyn strider_analyze::opt::ReadOnlyMemory>,
}
#[pymethods]
impl PyLoadReadOnly {
    #[new]
    fn new(rom: crate::reader::MemInput) -> Self {
        Self { rom: rom.into_arc() }
    }
}

// ── Polymorphic enum used by add/add_post ──────────────────────────────────

/// Aggregates every pass-wrapper class so `add` / `add_post` can
/// accept any of them via PyO3's automatic enum dispatch.
///
/// The six zero-sized passes (no per-instance state) carry the
/// wrapper class itself as their payload — `FromPyObject`'s
/// derive-generated dispatcher uses the type alone to pick the
/// variant, and the marker is then discarded by `into_erased`.
/// The five stateful passes carry a `Bound<'py, _>` so `into_erased`
/// can borrow and clone their inner state.
#[derive(FromPyObject)]
pub enum PyOptPass<'py> {
    ConstantFold(PyConstantFold),
    KnownBits(PyKnownBits),
    RedundantPhis(PyRedundantPhis),
    DeadBranchElim(PyDeadBranchElim),
    FlagCmpCanonicalize(PyFlagCmpCanonicalize),
    IfCondInversion(PyIfCondInversion),
    StackStoreDetect(Bound<'py, PyStackStoreDetect>),
    StackLoadForward(Bound<'py, PyStackLoadForward>),
    FunctionArgDetect(Bound<'py, PyFunctionArgDetect>),
    CallStackArgCollect(Bound<'py, PyCallStackArgCollect>),
    LoadReadOnly(Bound<'py, PyLoadReadOnly>),
}

impl PyOptPass<'_> {
    fn into_erased(self) -> ErasedPass {
        match self {
            PyOptPass::ConstantFold(_) => Box::new(strider_analyze::opt::ConstantFold),
            PyOptPass::KnownBits(_) => Box::new(strider_analyze::opt::KnownBits),
            PyOptPass::RedundantPhis(_) => Box::new(strider_analyze::opt::RedundantPhis),
            PyOptPass::DeadBranchElim(_) => Box::new(strider_analyze::opt::DeadBranchElimination),
            PyOptPass::FlagCmpCanonicalize(_) => Box::new(strider_analyze::opt::FlagCmpCanonicalize),
            PyOptPass::IfCondInversion(_) => Box::new(strider_analyze::opt::IfCondInversion),
            PyOptPass::StackStoreDetect(b) => Box::new(b.borrow().inner.clone()),
            PyOptPass::StackLoadForward(b) => Box::new(b.borrow().inner.clone()),
            PyOptPass::FunctionArgDetect(b) => Box::new(b.borrow().inner.clone()),
            PyOptPass::CallStackArgCollect(b) => Box::new(b.borrow().inner.clone()),
            PyOptPass::LoadReadOnly(b) => {
                Box::new(strider_analyze::opt::LoadReadOnly::new(std::sync::Arc::clone(&b.borrow().rom)))
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
