use pyo3::prelude::*;
use pyo3::types::PyType;
use std::sync::Mutex;
use strider_orchestrator::opt::OptimizerPipeline;

use crate::errors::into_strider_err;

/// A type-erased fixed-point pass.
pub(crate) type ErasedPass = Box<dyn strider_orchestrator::opt::Optimizer + Send>;

/// A type-erased post-pass, run once after the fixed-point loop.
pub(crate) type ErasedPostPass = Box<dyn strider_orchestrator::opt::PostOptimizer + Send>;

/// Lets a Python caller register an ordinary fixed-point pass via
/// `OptimizerPipeline.add_post(...)`: runs it once.  The post-pass loop keeps
/// the stack-slot stamps across post-passes, so a change drops them.
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
        if self.0.apply(edit, ctx)?.changed() {
            // A rewrite can move an address off the base a stamp names.
            edit.function().side_tables().clear_memory_slots();
        }
        Ok(())
    }

    /// The WRAPPED pass's name. Reporting the wrapper's would put a type name
    /// no caller can construct into `post_passes`, breaking the round trip
    /// `add_post` documents.
    fn name(&self) -> &'static str {
        self.0.name()
    }
}

/// Builder for an optimizer pipeline.  Construct via `empty()` or
/// `default()`, then `add(pass)` / `add_post(pass)`; apply it with
/// `Lifter.optimize(function, pipeline)`.  Applying a pipeline copies its
/// passes, so one pipeline drives any number of calls.
#[pyclass(name = "OptimizerPipeline", module = "strider.opt")]
pub struct PyOptimizerPipeline {
    /// A `Mutex` and not a `RefCell` for pyo3 0.23, which requires a
    /// `#[pyclass]` to be `Sync`; 0.22 asks only `Send`, which a `RefCell`
    /// already is.
    inner: Mutex<OptimizerPipeline>,
}

impl PyOptimizerPipeline {
    fn new_with(pipeline: OptimizerPipeline) -> Self {
        Self {
            inner: Mutex::new(pipeline),
        }
    }

    /// Every caller holds the GIL, so the lock never contends.
    fn lock(&self) -> std::sync::MutexGuard<'_, OptimizerPipeline> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn new_full_default() -> Self {
        Self::new_with(strider_orchestrator::opt::default_pipeline())
    }

    /// A copy of the wrapped pipeline, leaving the wrapper usable again.
    pub(crate) fn build_pipeline(&self) -> OptimizerPipeline {
        self.lock().clone()
    }
}

#[pymethods]
impl PyOptimizerPipeline {
    /// A pipeline with no passes; build one up with `add` / `add_post`.
    #[classmethod]
    fn empty(_cls: &Bound<'_, PyType>) -> Self {
        Self::new_with(OptimizerPipeline::new())
    }

    /// The canonical default pipeline (every built-in pass).
    #[classmethod]
    fn default(_cls: &Bound<'_, PyType>) -> Self {
        Self::new_full_default()
    }

    /// Append a `strider.opt.*` pass to the main list, which runs repeatedly
    /// until the graph stops changing.  A single-shot post-pass is rejected
    /// here; use `add_post` instead.
    fn add(&self, pass_obj: PyOptPass) -> PyResult<()> {
        let erased = pass_obj.into_erased()?;
        self.lock().add_boxed(erased);
        Ok(())
    }

    /// Append a post-pass, run once after the main passes finish. A
    /// fixed-point pass is accepted here too, and runs once.
    fn add_post(&self, pass_obj: PyOptPass) {
        self.lock().add_post_boxed(pass_obj.into_erased_post());
    }

    /// Names of the main (repeated) passes currently registered, in order.
    #[getter]
    fn passes(&self) -> Vec<String> {
        self.lock()
            .passes()
            .iter()
            .map(|p| p.name().to_string())
            .collect()
    }

    fn __repr__(&self) -> String {
        let pipeline = self.lock();
        format!(
            "OptimizerPipeline({} passes, {} post)",
            pipeline.passes().len(),
            pipeline.post_passes().len()
        )
    }

    /// Names of the post-passes currently registered, in order.
    #[getter]
    fn post_passes(&self) -> Vec<String> {
        self.lock()
            .post_passes()
            .iter()
            .map(|p| p.name().to_string())
            .collect()
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
            fn __repr__(&self) -> String {
                concat!($pyname, "()").to_string()
            }
        }
    };
}

/// The pass catalogue in two sections: a `main` row joins the fixed-point
/// list, a `post` row runs once after that loop converges and is rejected by
/// `add`. Each row is `"PythonName" => WrapperType = pass constructor, doc`.
macro_rules! opt_passes {
    (
        main { $($mn:literal => $mty:ident = $mctor:expr, $mdoc:literal;)* }
        post { $($pn:literal => $pty:ident = $pctor:expr, $pdoc:literal;)* }
    ) => {
        $( pure_pass_class!($mn => $mty, $mdoc); )*
        $( pure_pass_class!($pn => $pty, $pdoc); )*

        /// Aggregates every pass-wrapper class so `add` / `add_post` accept
        /// any of them via PyO3 enum dispatch.
        // Variants are named for their wrapper types, which the crate prefixes
        // `Py`; `annotation` is what a failed extraction reports.
        #[allow(clippy::enum_variant_names)]
        #[derive(FromPyObject)]
        pub enum PyOptPass {
            $( #[pyo3(annotation = $mn)] $mty($mty), )*
            $( #[pyo3(annotation = $pn)] $pty($pty), )*
        }

        impl PyOptPass {
            /// Erase to a fixed-point pass; a post-pass-only kind is an error.
            fn into_erased(self) -> PyResult<ErasedPass> {
                Ok(match self {
                    $( PyOptPass::$mty(_) => Box::new($mctor), )*
                    $( PyOptPass::$pty(_) )|* => {
                        return Err(into_strider_err(anyhow::anyhow!(
                            "{} are post-passes (they run once after the fixed-point \
                             loop converges): register them with \
                             OptimizerPipeline.add_post(...), not add(...).",
                            [$($pn),*].join(" / ")
                        )));
                    }
                })
            }

            /// Real post-passes erase to their concrete `PostOptimizer`;
            /// anything else goes through [`OptAsPostPass`] so it runs once
            /// after convergence.
            fn into_erased_post(self) -> ErasedPostPass {
                match self {
                    $( PyOptPass::$pty(_) => Box::new($pctor), )*
                    other => Box::new(OptAsPostPass(
                        other
                            .into_erased()
                            .expect("non-post pass always erases to a fixed-point pass"),
                    )),
                }
            }
        }

        fn register_passes(m: &Bound<'_, PyModule>) -> PyResult<()> {
            $( m.add_class::<$mty>()?; )*
            $( m.add_class::<$pty>()?; )*
            // `setattr`, not `add`: `add` would list them in `__all__`.
            m.setattr("_MAIN_PASS_NAMES", pyo3::types::PyTuple::new_bound(m.py(), [$($mn),*]))?;
            m.setattr("_POST_PASS_NAMES", pyo3::types::PyTuple::new_bound(m.py(), [$($pn),*]))?;
            Ok(())
        }
    };
}

opt_passes! {
    main {
        "ConstantFold" => PyConstantFold = strider_orchestrator::opt::ConstantFold::new(),
            "Constant-folds the IR: evaluates constant ops and applies algebraic \
             identities (`x+0->x`, `x^x->0`, AND-mask merging, ...).";
        "KnownBits" => PyKnownBits = strider_orchestrator::opt::KnownBits,
            "Bit-level known-zeros / known-ones lattice propagation, simplifying \
             ops whose result bits are statically determined.";
        "PhiCollapse" => PyPhiCollapse = strider_orchestrator::opt::PhiCollapse,
            "Braun trivial-phi elimination: collapses a `Phi` / `MemPhi` whose \
             non-self-referential value inputs all resolve to a single value, and \
             a strongly connected group of phis whose only outside input is one \
             value (destructive).";
        "RegionCollapse" => PyRegionCollapse = strider_orchestrator::opt::RegionCollapse,
            "Collapses a single-control-input `Region` join, rewiring its control \
             consumers to its lone predecessor (destructive).";
        "DeadBranchElimination" => PyDeadBranchElimination =
            strider_orchestrator::opt::DeadBranchElimination,
            "Folds a branch with a constant selector (an `If` on a constant `I1`, \
             a `Switch` on a constant dispatch address), wiring the live successor \
             past it and killing the branch node, and strips the dead arm's \
             predecessor slots from the joins (destructive).";
        "CfgDetach" => PyCfgDetach = strider_orchestrator::opt::CfgDetach,
            "Removes dead `Region`-predecessor slots (and the matching `Phi` / \
             `MemPhi` value slots) once a folded `If` makes a predecessor \
             control-unreachable (destructive).";
        "FlagCmpCanonicalize" => PyFlagCmpCanonicalize =
            strider_orchestrator::opt::FlagCmpCanonicalize::new(),
            "Rewrites a flag-tree (e.g. AArch64 NZCV-style chains) into a single \
             `IntCmpOp`.";
        "IfCondInversion" => PyIfCondInversion = strider_orchestrator::opt::IfCondInversion::new(),
            "Rewrites a branch on a negated condition (`Xor(C, IntConst(1)):I1`) into \
             a branch on the plain condition with its two arms swapped.";
        "LoadForward" => PyLoadForward = strider_orchestrator::opt::LoadForward::default(),
            "Forwards a store's value to a later load of the same location when \
             no intervening write, call or control merge can clobber it.";
        "LoadReadOnly" => PyLoadReadOnly = strider_orchestrator::opt::LoadReadOnly,
            "`LoadReadOnly()` folds constant-address loads against the rom supplied \
             via `strider.lift.lifter(arch, mem, rom=mem)` or \
             `strider.lift.load_elf(...)`. No change when no rom is available.";
    }
    post {
        "StackOffsetDetect" => PyStackOffsetDetect = strider_orchestrator::opt::StackOffsetDetect,
            "Stamps every SP-relative Store/Load with its concrete offset.";
        "FunctionArgDetect" => PyFunctionArgDetect = strider_orchestrator::opt::FunctionArgDetect,
            "Post-pass that records which values carry each incoming stack-passed \
             argument. Register arguments are recorded at lift time.";
        "CallStackArgCollect" => PyCallStackArgCollect =
            strider_orchestrator::opt::CallStackArgCollect,
            "Post-pass that wires positional stack arguments into `Call` nodes per \
             the calling convention's stack-arg layout.";
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyOptimizerPipeline>()?;
    register_passes(m)
}
