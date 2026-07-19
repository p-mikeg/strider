//! `OptimizerPipeline::add` is generic (`O: Optimizer + 'static`), so a
//! type-erased `Box<dyn Optimizer>` cannot be fed back into it.  The Python
//! wrapper accumulates erased boxes and, at run time, transfers them into a
//! real pipeline through forwarder adapters that satisfy the bound.

use pyo3::prelude::*;
use pyo3::types::PyType;
use std::sync::{Mutex, MutexGuard};

use crate::errors::into_strider_err;

/// `Optimizer` is not `Send + Sync` (strider is single-threaded, and the
/// wrapper crosses the PyO3 boundary under the GIL), so `PipelineState`'s
/// mutex is the sole synchronisation point.
pub(crate) type ErasedPass = Box<dyn strider_orchestrator::opt::Optimizer>;

/// Post-passes run once after the fixed-point loop and return no
/// `Change`/`NoChange`, so they live on a distinct trait from [`ErasedPass`].
pub(crate) type ErasedPostPass = Box<dyn strider_orchestrator::opt::PostOptimizer>;

struct ForwardPass(ErasedPass);

impl Clone for ForwardPass {
    fn clone(&self) -> Self {
        // `clone_box` preserves the concrete pass type; cloning the `Box`
        // itself would not.
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

/// Lets a Python caller register an ordinary fixed-point pass via
/// `OptimizerPipeline.add_post(...)`: runs it once, discarding its
/// `Change`/`NoChange`.
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
        let _ = self.0.apply(edit, ctx)?;
        Ok(())
    }
}

struct PipelineState {
    passes: Vec<ErasedPass>,
    post_passes: Vec<ErasedPostPass>,
    /// Tracked explicitly rather than inferred from "both lists empty":
    /// a deliberately-empty pipeline (`OptimizerPipeline.empty()`) is
    /// otherwise indistinguishable from one a prior drain emptied, and it
    /// must still be drainable exactly once.
    drained: bool,
}

impl PipelineState {
    fn new() -> Self {
        Self {
            passes: Vec::new(),
            post_passes: Vec::new(),
            drained: false,
        }
    }

    /// Iterating the canonical pipeline rather than hand-mirroring it makes
    /// drift from the Rust-side pipeline factories structurally impossible.
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
/// `Lifter.optimize(function, pipeline)`.  (`Lifter.analyze` drives its
/// own internal default pipeline unless `LifterOptions.pipeline` is set,
/// in which case that pipeline runs instead, for that call only.)
/// Applying a pipeline drains it, so rebuild before reuse.
// The boxed `dyn Optimizer` passes are not `Send + Sync`, so `unsendable`
// pins the wrapper to its creating thread; cross-thread access raises a
// Python `RuntimeError` instead of silently allowing UB.
#[pyclass(name = "OptimizerPipeline", module = "strider.opt", unsendable)]
pub struct PyOptimizerPipeline {
    state: Mutex<PipelineState>,
}

impl PyOptimizerPipeline {
    fn new_with(state: PipelineState) -> Self {
        Self {
            state: Mutex::new(state),
        }
    }

    fn lock_state(&self) -> PyResult<MutexGuard<'_, PipelineState>> {
        self.state
            .lock()
            .map_err(|_| into_strider_err(anyhow::anyhow!("OptimizerPipeline lock poisoned")))
    }

    pub(crate) fn new_full_default() -> Self {
        let pipeline = strider_orchestrator::opt::default_pipeline();
        Self::new_with(PipelineState::snapshot_from(&pipeline))
    }

    /// Drains the wrapper's pass lists into a fresh real pipeline.  Draining
    /// twice errors rather than silently running an empty pipeline and
    /// reporting success, which would mask reuse bugs at the call site.
    ///
    /// `prepend_load_read_only` prepends a `LoadReadOnly` so a caller-supplied
    /// rom is consumed even if the hand-built pipeline omitted that pass.  The
    /// prepend lands on the materialised pipeline, never on `state`, so a
    /// second `run` cannot double up on it.
    pub(crate) fn drain_into_pipeline(
        &self,
        prepend_load_read_only: bool,
    ) -> PyResult<strider_orchestrator::opt::OptimizerPipeline> {
        let mut state = self.lock_state()?;
        if state.drained {
            return Err(into_strider_err(anyhow::anyhow!(
                "OptimizerPipeline is empty — already drained by a prior \
                 Lifter.optimize() call.  Build a fresh pipeline \
                 (e.g. OptimizerPipeline.default()) or re-add passes before \
                 calling again."
            )));
        }
        state.drained = true;
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

    /// The canonical default pipeline (every built-in pass).
    #[classmethod]
    fn default(_cls: &Bound<'_, PyType>) -> Self {
        Self::new_full_default()
    }

    /// Append a fixed-point `strider.opt.*` pass.
    ///
    /// The single-shot post-passes (`StackOffsetDetect`, `FunctionArgDetect`,
    /// `CallStackArgCollect`) are rejected here; use `add_post` instead.
    fn add(&self, pass_obj: PyOptPass) -> PyResult<()> {
        let erased = pass_obj.into_erased()?;
        let mut state = self.lock_state()?;
        state.passes.push(erased);
        Ok(())
    }

    /// Append a post-pass, run once after the fixed-point loop converges.
    fn add_post(&self, pass_obj: PyOptPass) -> PyResult<()> {
        let mut state = self.lock_state()?;
        state.post_passes.push(pass_obj.into_erased_post());
        Ok(())
    }

    /// Names of the fixed-point passes currently registered, in order.
    #[getter]
    fn passes(&self) -> PyResult<Vec<String>> {
        let state = self.lock_state()?;
        Ok(state.passes.iter().map(|p| p.name().to_string()).collect())
    }

    /// Names of the post-passes currently registered, in order.
    #[getter]
    fn post_passes(&self) -> PyResult<Vec<String>> {
        let state = self.lock_state()?;
        Ok(state
            .post_passes
            .iter()
            .map(|p| p.name().to_string())
            .collect())
    }
}

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

// These four read the calling convention from the function under analysis at
// run time, so they carry no per-instance state and take no constructor args.

pure_pass_class!("LoadForward" => PyLoadForward,
    "`LoadForward()` — forwards values from stack-tagged `Store` nodes to \
     subsequent same-offset `Load` nodes.");
pure_pass_class!("StackOffsetDetect" => PyStackOffsetDetect,
    "`StackOffsetDetect()` — stamps every SP-relative Store/Load's concrete \
     offset in the `stack_offsets` decomposition cache.");
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

/// Aggregates every pass-wrapper class so `add` / `add_post` accept any of
/// them via PyO3 enum dispatch.  Payloads are the zero-sized wrapper classes
/// themselves: the derived dispatcher picks the variant by type alone, and
/// `into_erased` discards the marker.
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
    /// The single-shot post-passes are `PostOptimizer`s and cannot run in the
    /// fixed-point loop, so they error out here pointing at `add_post`.
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

    /// Real post-passes erase to their concrete `PostOptimizer`; anything else
    /// goes through [`OptAsPostPass`] so it runs once after convergence.
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
            other => Box::new(OptAsPostPass(
                other
                    .into_erased()
                    .expect("non-post pass always erases to a fixed-point pass"),
            )),
        }
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyOptimizerPipeline>()?;
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
    Ok(())
}
