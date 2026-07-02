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

/// Type-erased **post**-pass — a `Box<dyn PostOptimizer>`.  Post-passes
/// (`StackOffsetDetect`, `FunctionArgDetect`, `CallStackArgCollect`,
/// `IndirectBranchClassify`) live on a distinct trait that runs once after the
/// fixed-point loop and returns no `Change`/`NoChange`, so they cannot be
/// stored as an [`ErasedPass`].
pub(crate) type ErasedPostPass = Box<dyn strider_orchestrator::opt::PostOptimizer>;

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
        edit: &mut strider_opt::EditFunction<'_>,
        ctx: &mut strider_orchestrator::opt::OptCtx<'_>,
    ) -> strider_orchestrator::opt::Result<strider_orchestrator::opt::OptimizationResult> {
        self.0.apply(edit, ctx)
    }
}

/// Post-pass sibling of [`ForwardPass`].  Wraps an [`ErasedPostPass`] so
/// `strider_orchestrator::opt::OptimizerPipeline::add_post_pass` (which requires
/// `O: PostOptimizer + 'static`) can accept the type-erased box, forwarding
/// `apply` straight through.
struct ForwardPostPass(ErasedPostPass);

impl Clone for ForwardPostPass {
    fn clone(&self) -> Self {
        ForwardPostPass(self.0.clone_box())
    }
}

impl strider_orchestrator::opt::PostOptimizer for ForwardPostPass {
    fn apply(
        &self,
        edit: &mut strider_opt::EditFunction<'_>,
        ctx: &mut strider_orchestrator::opt::OptCtx<'_>,
    ) -> strider_orchestrator::opt::Result<()> {
        self.0.apply(edit, ctx)
    }
}

/// Bridges a fixed-point [`ErasedPass`] (a `Box<dyn Optimizer>`) into a
/// [`PostOptimizer`][strider_orchestrator::opt::PostOptimizer] so a Python
/// caller may register an ordinary pass as a post-pass via
/// `OptimizerPipeline.add_post(...)`.  Runs the inner pass once and discards
/// its `Change`/`NoChange` (a post-pass is single-shot and never re-iterates).
struct OptAsPostPass(ErasedPass);

impl Clone for OptAsPostPass {
    fn clone(&self) -> Self {
        OptAsPostPass(self.0.clone_box())
    }
}

impl strider_orchestrator::opt::PostOptimizer for OptAsPostPass {
    fn apply(
        &self,
        edit: &mut strider_opt::EditFunction<'_>,
        ctx: &mut strider_orchestrator::opt::OptCtx<'_>,
    ) -> strider_orchestrator::opt::Result<()> {
        // Discard the inner pass's Change/NoChange — a post-pass runs once.
        let _ = self.0.apply(edit, ctx)?;
        Ok(())
    }
}

/// Internal builder representation: a list of fixed-point passes and
/// a list of post-passes, both as type-erased boxes.  Snapshot on
/// `run` materialises a real `strider_orchestrator::opt::OptimizerPipeline` ad-hoc.
struct PipelineState {
    passes: Vec<ErasedPass>,
    post_passes: Vec<ErasedPostPass>,
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
    /// `Lifter::build_optimizer_pipeline` — structurally impossible.
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
}

/// Builder for an optimizer pipeline.  Construct via `empty()` or
/// `default()`, then `add(pass)` / `add_post(pass)`; apply it with
/// `Function.optimize(pipeline)`.  (`Lifter.analyze` always drives its
/// own internal default pipeline — there is no Python-facing way to
/// hand it a custom one; build the custom pipeline and apply it via
/// `Function.optimize` afterwards instead.)  Applying a pipeline drains
/// it, so rebuild before reuse.
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

    /// Build the canonical "full" pipeline by snapshotting
    /// [`strider_orchestrator::opt::default_pipeline`]'s passes.  Iterating
    /// the canonical Rust pipeline rather than hand-mirroring it makes drift
    /// between the Python wrapper and the Rust default structurally
    /// impossible.
    pub(crate) fn new_full_default() -> Self {
        let pipeline = strider_orchestrator::opt::default_pipeline();
        Self::new_with(PipelineState::snapshot_from(&pipeline))
    }

    /// Materialise a real `strider_orchestrator::opt::OptimizerPipeline` from the current
    /// state.  Drains the internal pass lists — call once per
    /// "transfer" cycle and rebuild the wrapper afterwards if you
    /// need to keep it.
    ///
    /// returns `Err(StriderError)` if
    /// the wrapper has already been drained (both pass lists empty).
    /// Without this guard a second `Function.optimize(pipe)` would
    /// silently run an empty pipeline and report success — masking
    /// caller bugs where the same wrapper is reused after a previous
    /// `Function.optimize` call already consumed it.
    ///
    /// When `prepend_load_read_only` is set, a unit `LoadReadOnly` pass is
    /// prepended to the materialised pipeline so a caller-supplied `rom`
    /// is consumed even when the user's hand-built pipeline didn't add
    /// `LoadReadOnly` explicitly (the rom itself flows via the
    /// [`strider_orchestrator::opt::OptCtx`] passed to
    /// `OptimizerPipeline::run`).  The prepend happens on the *materialised*
    /// pipeline, NOT on the wrapper's own `state`, so the caller's
    /// `PyOptimizerPipeline` object is only drained (its documented
    /// "consumed on use" behaviour) — never silently grown with an extra
    /// pass that would double up on a second `run`.
    pub(crate) fn drain_into_pipeline(
        &self,
        prepend_load_read_only: bool,
    ) -> PyResult<strider_orchestrator::opt::OptimizerPipeline> {
        let mut state = self.lock_state()?;
        if state.passes.is_empty() && state.post_passes.is_empty() {
            return Err(into_strider_err(anyhow::anyhow!(
                "OptimizerPipeline is empty — already drained by a prior \
                 Function.optimize() call.  Build a fresh pipeline \
                 (e.g. OptimizerPipeline.default()) or re-add passes before \
                 calling again."
            )));
        }
        let mut pipe = strider_orchestrator::opt::OptimizerPipeline::new();
        if prepend_load_read_only {
            pipe.add(strider_orchestrator::opt::LoadReadOnly);
        }
        for p in state.passes.drain(..) {
            pipe.add(ForwardPass(p));
        }
        for p in state.post_passes.drain(..) {
            pipe.add_post_pass(ForwardPostPass(p));
        }
        Ok(pipe)
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
        Self::new_full_default()
    }

    /// Append a pass to the fixed-point pass list (any fixed-point
    /// `strider.opt.*` pass instance).
    ///
    /// The three single-shot post-passes (`StackOffsetDetect`,
    /// `FunctionArgDetect`, `CallStackArgCollect`) are rejected here — register
    /// them with `add_post` instead.  (`IndirectBranchClassify` is also a
    /// post-pass but is appended by the orchestrator, not user-registerable.)
    fn add(&self, pass_obj: PyOptPass) -> PyResult<()> {
        let erased = pass_obj.into_erased()?;
        let mut state = self.lock_state()?;
        state.passes.push(erased);
        Ok(())
    }

    /// Append a post-pass — run once after the fixed-point loop converges.
    fn add_post(&self, pass_obj: PyOptPass) -> PyResult<()> {
        let mut state = self.lock_state()?;
        state.post_passes.push(pass_obj.into_erased_post());
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
            fn new() -> Self {
                Self
            }
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

// ── Formerly CC/arch-aware passes ─────────────────────────────────────────
//
// These four passes once took `(sleigh, cc[, arch])` at construction, but
// every argument was discarded: the calling convention is read from the
// function under analysis (`Function.default_cc`) at run time, so the
// passes carry no per-instance state.  They are now zero-sized, no-arg
// classes exactly like the pure passes above.

pure_pass_class!("LoadForward" => PyLoadForward,
    "`LoadForward()` — forwards values from stack-tagged `Store` nodes to \
     subsequent same-offset `Load` nodes.");
pure_pass_class!("StackOffsetDetect" => PyStackOffsetDetect,
    "`StackOffsetDetect()` — stamps every SP-relative Store/Load's concrete \
     offset in `Function::stack_offsets`.");
pure_pass_class!("FunctionArgDetect" => PyFunctionArgDetect,
    "Post-pass that canonicalises register / stack argument reads into the \
     `Function.arg_index_to_values` side-table (carrier `InitialVar` for \
     register args, `Load` for stack args).");
pure_pass_class!("CallStackArgCollect" => PyCallStackArgCollect,
    "Post-pass that wires positional stack arguments into `Call` nodes per \
     the calling convention's stack-arg layout.");

pure_pass_class!("LoadReadOnly" => PyLoadReadOnly,
    "`LoadReadOnly()` — folds constant-address loads against the rom \
     supplied via `strider.lifter(arch, mem, rom=mem)` / \
     `strider.load_elf(...)`.  The rom flows through the \
     orchestrator's `Strider::rom` → `OptCtx` plumbing rather than being \
     attached to the pass; an instance constructed here is a marker, and \
     the pass short-circuits to no-change when no rom is available.");

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
pub enum PyOptPass {
    ConstantFold(PyConstantFold),
    KnownBits(PyKnownBits),
    PhiCollapse(PyPhiCollapse),
    RegionCollapse(PyRegionCollapse),
    DeadBranchElimination(PyDeadBranchElimination),
    CfgDetach(PyCfgDetach),
    FlagCmpCanonicalize(PyFlagCmpCanonicalize),
    IfCondInversion(PyIfCondInversion),
    LoadForward(PyLoadForward),
    FunctionArgDetect(PyFunctionArgDetect),
    CallStackArgCollect(PyCallStackArgCollect),
    LoadReadOnly(PyLoadReadOnly),
    StackOffsetDetect(PyStackOffsetDetect),
}

impl PyOptPass {
    /// Erase a **fixed-point** pass into a `Box<dyn Optimizer>` for the
    /// fixed-point pass list.  The single-shot post-passes
    /// (`StackOffsetDetect`, `FunctionArgDetect`, `CallStackArgCollect`) are
    /// `PostOptimizer`s — they cannot run in the fixed-point loop — so they are
    /// rejected with a `StriderError` directing the caller to `add_post`.
    fn into_erased(self) -> PyResult<ErasedPass> {
        Ok(match self {
            PyOptPass::ConstantFold(_) => Box::new(strider_orchestrator::opt::ConstantFold::new()),
            PyOptPass::KnownBits(_) => Box::new(strider_orchestrator::opt::KnownBits),
            PyOptPass::PhiCollapse(_) => Box::new(strider_orchestrator::opt::PhiCollapse),
            PyOptPass::RegionCollapse(_) => Box::new(strider_orchestrator::opt::RegionCollapse),
            PyOptPass::DeadBranchElimination(_) => {
                Box::new(strider_orchestrator::opt::DeadBranchElimination)
            }
            PyOptPass::CfgDetach(_) => Box::new(strider_orchestrator::opt::CfgDetach),
            PyOptPass::FlagCmpCanonicalize(_) => {
                Box::new(strider_orchestrator::opt::FlagCmpCanonicalize::new())
            }
            PyOptPass::IfCondInversion(_) => {
                Box::new(strider_orchestrator::opt::IfCondInversion::new())
            }
            PyOptPass::LoadForward(_) => Box::new(strider_orchestrator::opt::LoadForward),
            PyOptPass::LoadReadOnly(_) => Box::new(strider_orchestrator::opt::LoadReadOnly),
            PyOptPass::FunctionArgDetect(_)
            | PyOptPass::CallStackArgCollect(_)
            | PyOptPass::StackOffsetDetect(_) => {
                return Err(into_strider_err(anyhow::anyhow!(
                    "StackOffsetDetect / FunctionArgDetect / CallStackArgCollect are \
                     post-passes (they run once after the fixed-point loop converges) — \
                     register them with OptimizerPipeline.add_post(...), not add(...)."
                )));
            }
        })
    }

    /// Erase any pass into a `Box<dyn PostOptimizer>` for the post-pass list.
    /// The four reclassified passes erase to their concrete `PostOptimizer`
    /// type directly; any other (fixed-point) pass is wrapped in
    /// [`OptAsPostPass`] so it runs once after convergence.
    fn into_erased_post(self) -> ErasedPostPass {
        match self {
            PyOptPass::FunctionArgDetect(_) => {
                Box::new(strider_orchestrator::opt::FunctionArgDetect)
            }
            PyOptPass::CallStackArgCollect(_) => {
                Box::new(strider_orchestrator::opt::CallStackArgCollect)
            }
            PyOptPass::StackOffsetDetect(_) => {
                Box::new(strider_orchestrator::opt::StackOffsetDetect)
            }
            // Any ordinary fixed-point pass added as a post-pass runs once and
            // discards its Change/NoChange (via the OptAsPostPass bridge).
            other => Box::new(OptAsPostPass(
                other
                    .into_erased()
                    .expect("non-post pass always erases to a fixed-point pass"),
            )),
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
