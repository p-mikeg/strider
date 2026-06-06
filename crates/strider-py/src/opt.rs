//! `PyOptimizerPipeline` and one wrapper class per opt pass.
//!
//! The Rust `strider_orchestrator::opt::OptimizerPipeline::add` is generic over the concrete
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

/// Trait-object holder owning a heap-allocated `strider_orchestrator::opt::Optimizer`.
///
/// `Optimizer` is no longer `Send + Sync` — strider runs single-
/// threaded and the Python wrapper crosses the PyO3 boundary under
/// the GIL — so the pipeline-state mutex (see `PipelineState`) is the
/// sole synchronisation point and the boxed pass itself does not need
/// any thread-safety markers.
pub(crate) type ErasedPass = Box<dyn strider_orchestrator::opt::Optimizer>;

/// Adapter that turns an owned `ErasedPass` into something
/// `strider_orchestrator::opt::OptimizerPipeline::add` can accept.  `add` requires
/// `O: Optimizer + 'static`; this newtype satisfies the bound and
/// forwards `apply` (the real entry point) straight through, so a
/// `ForwardPass` driven by the shared-ctx pipeline shares the same
/// `EditFunction` as every other pass.
struct ForwardPass(ErasedPass);

impl Clone for ForwardPass {
    fn clone(&self) -> Self {
        // The wrapped pass owns its own clone strategy via `OptimizerClone`
        // (the supertrait of `Optimizer`).  Forwarding to it rather than
        // cloning the `Box` itself preserves the concrete pass type.
        ForwardPass(self.0.clone_box())
    }
}

impl strider_orchestrator::opt::Optimizer for ForwardPass {
    fn apply(
        &self,
        rctx: &mut strider_opt::EditFunction<'_>,
        ctx: &mut strider_orchestrator::opt::OptCtx<'_>,
    ) -> strider_orchestrator::opt::Result<strider_orchestrator::opt::OptimizationResult> {
        self.0.apply(rctx, ctx)
    }
}

/// Internal builder representation: a list of fixed-point passes and
/// a list of post-passes, both as type-erased boxes.  Snapshot on
/// `run` materialises a real `strider_orchestrator::opt::OptimizerPipeline` ad-hoc.
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

    /// Snapshot a canonical `strider_orchestrator` pipeline into the wrapper's
    /// internal representation by `clone_box`-ing each pass.
    ///
    /// Iterating the canonical pipeline rather than hand-mirroring it
    /// makes drift between the Python wrapper and the Rust-side pipeline
    /// factories — `default_pipeline()` and the CC-aware
    /// `LiftDriver::build_optimizer_pipeline` — structurally impossible.
    fn snapshot_from(pipeline: &strider_orchestrator::opt::OptimizerPipeline) -> Self {
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
        Self::snapshot_from(&strider_orchestrator::opt::default_pipeline())
    }
}

/// Builder for an optimizer pipeline.  Construct via `empty()` or
/// `default()`, then `add(pass)` / `add_post(pass)`; apply it with
/// `Function.optimize` or pass `pipeline=` to `strider.run`.  Applying
/// a pipeline drains it, so rebuild before reuse.
///
/// Holds the internal state behind a `Mutex` so `add` / `add_post`
/// don't require `&mut self` (PyO3 method receivers are typically
/// `&self` for ergonomics).
///
/// `unsendable`: the boxed `dyn Optimizer` passes are no longer
/// `Send + Sync` (strider runs single-threaded under the GIL).  The
/// `unsendable` marker tells PyO3 to keep the wrapper pinned to the
/// thread that created it; revocation on cross-thread access raises
/// a `RuntimeError` at the Python boundary rather than silently
/// allowing UB.
#[pyclass(name = "OptimizerPipeline", module = "strider", unsendable)]
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
    /// `strider_orchestrator::LiftDriver::build_optimizer_pipeline` and snapshotting
    /// its passes.  Iterating the canonical Rust pipeline rather than
    /// hand-mirroring it makes drift between the Python wrapper and
    /// `LiftDriver::build_optimizer_pipeline` structurally impossible.
    pub(crate) fn new_full_default(strider: &strider_orchestrator::LiftDriver) -> Self {
        let pipeline = strider.build_optimizer_pipeline();
        Self::new_with(PipelineState::snapshot_from(&pipeline))
    }

    /// Materialise a real `strider_orchestrator::opt::OptimizerPipeline` from the current
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
    pub(crate) fn drain_into_pipeline(&self) -> PyResult<strider_orchestrator::opt::OptimizerPipeline> {
        let mut state = self.lock_state()?;
        if state.passes.is_empty() && state.post_passes.is_empty() {
            return Err(into_strider_err(anyhow::anyhow!(
                "OptimizerPipeline is empty — already drained by a prior \
                 Graph.optimize() / strider.run().  Build a fresh pipeline \
                 (e.g. OptimizerPipeline.default()) or re-add passes before \
                 calling again."
            )));
        }
        let mut pipe = strider_orchestrator::opt::OptimizerPipeline::new();
        for p in state.passes.drain(..) {
            pipe.add(ForwardPass(p));
        }
        for p in state.post_passes.drain(..) {
            pipe.add_post_pass(ForwardPass(p));
        }
        Ok(pipe)
    }

    /// Prepend a `LoadReadOnly` pass to the front of the pipeline's
    /// pass list.  Used by `run_with_custom_pipeline` to ensure the
    /// user-supplied `rom` is consumed even if the caller's pipeline
    /// didn't include `LoadReadOnly` explicitly — the rom itself
    /// flows via the [`strider_orchestrator::opt::OptCtx`] passed to
    /// `OptimizerPipeline::run`.
    pub(crate) fn prepend_load_read_only(&self) -> PyResult<()> {
        let mut state = self.lock_state()?;
        let pass: ErasedPass = Box::new(strider_orchestrator::opt::LoadReadOnly);
        state.passes.insert(0, pass);
        Ok(())
    }
}

#[pymethods]
impl PyOptimizerPipeline {
    /// A pipeline with no passes; build one up with `add` / `add_post`.
    #[classmethod]
    fn empty(_cls: &Bound<'_, PyType>) -> Self {
        Self::new_with(PipelineState::new())
    }

    /// The canonical default pipeline (every built-in pass),
    /// mirroring `strider_orchestrator::opt::default_pipeline`.
    #[classmethod]
    fn default(_cls: &Bound<'_, PyType>) -> Self {
        Self::new_with(PipelineState::from_default())
    }

    /// Append a pass to the fixed-point pass list (any `strider.opt.*`
    /// pass instance).
    fn add(&self, pass_obj: PyOptPass<'_>) -> PyResult<()> {
        let mut state = self.lock_state()?;
        state.passes.push(pass_obj.into_erased());
        Ok(())
    }

    /// Append a post-pass — run once after the fixed-point loop converges.
    fn add_post(&self, pass_obj: PyOptPass<'_>) -> PyResult<()> {
        let mut state = self.lock_state()?;
        state.post_passes.push(pass_obj.into_erased());
        Ok(())
    }

    /// Number of fixed-point passes currently registered.
    fn pass_count(&self) -> PyResult<usize> {
        let state = self.lock_state()?;
        Ok(state.passes.len())
    }

    /// Number of post-passes currently registered.
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
    ($pyname:literal => $rust:ident, $doc:literal) => {
        #[doc = $doc]
        #[pyclass(name = $pyname, module = "strider.opt")]
        #[derive(Clone)]
        pub struct $rust;
        #[pymethods]
        impl $rust {
            #[doc = concat!("Construct the ", $pyname, " pass (no configuration).")]
            #[new]
            fn new() -> Self { Self }
        }
    };
}

pure_pass_class!("ConstantFold" => PyConstantFold,
    "Constant-folds the IR: evaluates constant ops and applies algebraic \
     identities (`x+0→x`, `x^x→0`, AND-mask merging, …).");
pure_pass_class!("KnownBits" => PyKnownBits,
    "Bit-level known-zeros / known-ones lattice propagation, simplifying \
     ops whose result bits are statically determined.");
pure_pass_class!("PhiCollapse" => PyPhiCollapse,
    "Braun trivial-phi elimination: collapses a `Phi` / `MemPhi` whose \
     non-self-referential value inputs all resolve to a single value \
     (destructive).");
pure_pass_class!("RegionCollapse" => PyRegionCollapse,
    "Collapses a single-control-input `Region` join, rewiring its control \
     consumers to its lone predecessor (destructive).");
pure_pass_class!("DeadBranchElimination" => PyDeadBranchElimination,
    "Folds `If(const)` branches: redirects the live successor past the `If` \
     and detaches the folded `If` (destructive).");
pure_pass_class!("CfgDetach" => PyCfgDetach,
    "Removes dead `Region`-predecessor slots (and the matching `Phi` / \
     `MemPhi` value slots) once a folded `If` makes a predecessor \
     control-unreachable (destructive).");
pure_pass_class!("FlagCmpCanonicalize" => PyFlagCmpCanonicalize,
    "Rewrites a flag-tree (e.g. AArch64 NZCV-style chains) into a single \
     `IntCmpOp`.");
pure_pass_class!("IfCondInversion" => PyIfCondInversion,
    "Rewrites `If(BitNot(C)){A}{B}` → `If(C){B}{A}` so patterns match the \
     canonical, un-negated condition shape.");

// ── CC/arch-aware passes ──────────────────────────────────────────────────
//
// Each takes (sleigh, cc) — or (sleigh, cc, arch) — at construction
// time, builds a strider_target::BuiltCallingConvention against the
// Sleigh's register table, and stores the concrete pre-configured
// pass.
//
// `cc_aware_pass_class!` collapses the 17-line boilerplate that the
// (sleigh, cc) -> from_convention shape would otherwise repeat
// verbatim for every CC-aware pass.  The sibling `pure_pass_class!`
// macro above covers the zero-arg pass shape; CC + extra-arg passes
// (e.g. LoadForward's `arch` param) stay hand-written below.

macro_rules! cc_aware_pass_class {
    ($pyname:literal => $rust:ident, $analyze:ty, $doc:literal) => {
        #[doc = $doc]
        #[pyclass(name = $pyname, module = "strider.opt")]
        pub struct $rust {
            pub(crate) inner: $analyze,
        }
        #[pymethods]
        impl $rust {
            #[doc = concat!(
                "`", $pyname, "(sleigh, cc)` — constructs the pass.  The \
                 calling convention is read from the function under analysis \
                 (`Function.default_cc`); the `(sleigh, cc)` arguments are \
                 retained for backward compatibility but no longer configure \
                 the pass."
            )]
            #[new]
            fn new(
                py: Python<'_>,
                sleigh: Py<crate::sleigh::PySleigh>,
                cc: crate::cc::PyCallingConvention,
            ) -> PyResult<Self> {
                let _ = (py, sleigh, cc);
                Ok(Self {
                    inner: <$analyze>::new(),
                })
            }
        }
    };
}

/// `LoadForward(sleigh, cc, arch)` — forwards values from stack-tagged
/// `Store` nodes to subsequent same-offset `Load` nodes.
#[pyclass(name = "LoadForward", module = "strider.opt")]
pub struct PyLoadForward {
    pub(crate) inner: strider_orchestrator::opt::LoadForward,
}
#[pymethods]
impl PyLoadForward {
    /// `LoadForward(sleigh, cc, arch)` — resolves the convention against
    /// `sleigh`'s registers and configures the pass for `arch`.
    #[new]
    fn new(
        py: Python<'_>,
        sleigh: Py<crate::sleigh::PySleigh>,
        cc: crate::cc::PyCallingConvention,
        arch: crate::arch::PySleighArch,
    ) -> PyResult<Self> {
        // SP varnode + endianness are read from the function under analysis;
        // the (sleigh, cc, arch) args are retained for compatibility.
        let _ = (py, sleigh, cc, arch);
        Ok(Self {
            inner: strider_orchestrator::opt::LoadForward::new(),
        })
    }
}

/// `StackOffsetDetect(sleigh, cc)` — stamps every SP-relative
/// Store/Load's concrete offset in `Function::stack_offsets`.
#[pyclass(name = "StackOffsetDetect", module = "strider.opt")]
pub struct PyStackOffsetDetect {
    pub(crate) inner: strider_orchestrator::opt::StackOffsetDetect,
}
#[pymethods]
impl PyStackOffsetDetect {
    /// `StackOffsetDetect(sleigh, cc)` — resolves the convention against
    /// `sleigh`'s registers and configures the pass.
    #[new]
    fn new(
        py: Python<'_>,
        sleigh: Py<crate::sleigh::PySleigh>,
        cc: crate::cc::PyCallingConvention,
    ) -> PyResult<Self> {
        // SP varnode is read from the function under analysis; the
        // (sleigh, cc) args are retained for compatibility.
        let _ = (py, sleigh, cc);
        Ok(Self {
            inner: strider_orchestrator::opt::StackOffsetDetect::new(),
        })
    }
}

cc_aware_pass_class!(
    "FunctionArgDetect" => PyFunctionArgDetect,
    strider_orchestrator::opt::FunctionArgDetect,
    "Post-pass that canonicalises register / stack argument reads into \
     the `Function.arg_index_to_values` side-table (carrier `InitialVar` \
     for register args, `Load` for stack args)."
);

cc_aware_pass_class!(
    "CallStackArgCollect" => PyCallStackArgCollect,
    strider_orchestrator::opt::CallStackArgCollect,
    "Post-pass that wires positional stack arguments into `Call` nodes \
     per the calling convention's stack-arg layout."
);

/// `LoadReadOnly()` — folds constant-address loads against the rom
/// supplied via `strider.run(..., rom=mem)`.  The rom flows through
/// the orchestrator's `RunConfig.rom` → `OptCtx` plumbing rather
/// than being attached to the pass; an instance constructed here is
/// a marker, and the pass short-circuits to no-change when no rom is
/// available.
#[pyclass(name = "LoadReadOnly", module = "strider.opt")]
#[derive(Clone)]
pub struct PyLoadReadOnly;
#[pymethods]
impl PyLoadReadOnly {
    /// `LoadReadOnly()` — the rom is no longer attached to the pass;
    /// supply it via `strider.run(..., rom=mem)` (orchestrator path) or
    /// the analogous custom-pipeline plumbing.
    #[new]
    fn new() -> Self {
        Self
    }
}

// ── Polymorphic enum used by add/add_post ──────────────────────────────────

/// Aggregates every pass-wrapper class so `add` / `add_post` can
/// accept any of them via PyO3's automatic enum dispatch.
///
/// The zero-sized passes (no per-instance state) carry the
/// wrapper class itself as their payload — `FromPyObject`'s
/// derive-generated dispatcher uses the type alone to pick the
/// variant, and the marker is then discarded by `into_erased`.
/// The stateful passes carry a `Bound<'py, _>` so `into_erased`
/// can borrow and clone their inner state.
#[derive(FromPyObject)]
pub enum PyOptPass<'py> {
    ConstantFold(PyConstantFold),
    KnownBits(PyKnownBits),
    PhiCollapse(PyPhiCollapse),
    RegionCollapse(PyRegionCollapse),
    DeadBranchElimination(PyDeadBranchElimination),
    CfgDetach(PyCfgDetach),
    FlagCmpCanonicalize(PyFlagCmpCanonicalize),
    IfCondInversion(PyIfCondInversion),
    LoadForward(Bound<'py, PyLoadForward>),
    FunctionArgDetect(Bound<'py, PyFunctionArgDetect>),
    CallStackArgCollect(Bound<'py, PyCallStackArgCollect>),
    LoadReadOnly(PyLoadReadOnly),
    StackOffsetDetect(Bound<'py, PyStackOffsetDetect>),
}

impl PyOptPass<'_> {
    fn into_erased(self) -> ErasedPass {
        match self {
            PyOptPass::ConstantFold(_) => Box::new(strider_orchestrator::opt::ConstantFold::new()),
            PyOptPass::KnownBits(_) => Box::new(strider_orchestrator::opt::KnownBits),
            PyOptPass::PhiCollapse(_) => Box::new(strider_orchestrator::opt::PhiCollapse),
            PyOptPass::RegionCollapse(_) => Box::new(strider_orchestrator::opt::RegionCollapse),
            PyOptPass::DeadBranchElimination(_) => Box::new(strider_orchestrator::opt::DeadBranchElimination),
            PyOptPass::CfgDetach(_) => Box::new(strider_orchestrator::opt::CfgDetach),
            PyOptPass::FlagCmpCanonicalize(_) => Box::new(strider_orchestrator::opt::FlagCmpCanonicalize::new()),
            PyOptPass::IfCondInversion(_) => Box::new(strider_orchestrator::opt::IfCondInversion::new()),
            PyOptPass::LoadForward(b) => Box::new(b.borrow().inner.clone()),
            PyOptPass::FunctionArgDetect(b) => Box::new(b.borrow().inner.clone()),
            PyOptPass::CallStackArgCollect(b) => Box::new(b.borrow().inner.clone()),
            PyOptPass::LoadReadOnly(_) => Box::new(strider_orchestrator::opt::LoadReadOnly),
            PyOptPass::StackOffsetDetect(b) => Box::new(b.borrow().inner.clone()),
        }
    }
}

pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyOptimizerPipeline>()?;
    let m = PyModule::new_bound(py, "opt")?;
    m.add_class::<PyConstantFold>()?;
    m.add_class::<PyKnownBits>()?;
    m.add_class::<PyPhiCollapse>()?;
    m.add_class::<PyRegionCollapse>()?;
    m.add_class::<PyDeadBranchElimination>()?;
    m.add_class::<PyCfgDetach>()?;
    m.add_class::<PyFlagCmpCanonicalize>()?;
    m.add_class::<PyIfCondInversion>()?;
    m.add_class::<PyLoadForward>()?;
    m.add_class::<PyStackOffsetDetect>()?;
    m.add_class::<PyFunctionArgDetect>()?;
    m.add_class::<PyCallStackArgCollect>()?;
    m.add_class::<PyLoadReadOnly>()?;
    parent.add_submodule(&m)?;
    let sys = py.import_bound("sys")?;
    sys.getattr("modules")?.set_item("strider.opt", &m)?;
    Ok(())
}
