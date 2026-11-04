use pyo3::prelude::*;
use pyo3::types::PyType;
use strider_orchestrator::opt::OptimizerPipeline;

use crate::errors::into_strider_err;

pub(crate) type ErasedPass = &'static dyn strider_orchestrator::opt::Optimizer;

/// An ordered list of optimizer passes plus post-passes that run once at the
/// end. Applying a pipeline copies it, so one object serves any number of
/// calls.
#[pyclass(name = "OptimizerPipeline", module = "strider.opt")]
pub struct PyOptimizerPipeline {
    inner: OptimizerPipeline,
}

impl PyOptimizerPipeline {
    pub(crate) fn build_pipeline(&self) -> OptimizerPipeline {
        self.inner.clone()
    }
}

#[pymethods]
impl PyOptimizerPipeline {
    /// A pipeline with no passes.
    #[classmethod]
    fn empty(_cls: &Bound<'_, PyType>) -> Self {
        Self {
            inner: OptimizerPipeline::new(),
        }
    }

    /// The pipeline `analyze` runs when given none: every built-in pass.
    #[classmethod]
    fn default(_cls: &Bound<'_, PyType>) -> Self {
        Self {
            inner: strider_orchestrator::opt::default_pipeline(),
        }
    }

    /// Append a pass to the main list, run until the graph stops changing.
    /// Raises `StriderError` for a post-pass.
    fn add(&mut self, pass_obj: PyOptPass) -> PyResult<()> {
        self.inner.add(pass_obj.into_erased()?);
        Ok(())
    }

    /// Append a post-pass, run once after the main passes. A main pass is
    /// accepted too, and runs once.
    fn add_post(&mut self, pass_obj: PyOptPass) {
        self.inner.add_post_pass(pass_obj.into_erased_post());
    }

    /// The main passes' names, in order.
    #[getter]
    fn passes(&self) -> Vec<String> {
        self.inner
            .passes()
            .iter()
            .map(|p| p.name().to_string())
            .collect()
    }

    fn __repr__(&self) -> String {
        format!(
            "OptimizerPipeline({} passes, {} post)",
            self.inner.passes().len(),
            self.inner.post_passes().len()
        )
    }

    /// The post-passes' names, in order.
    #[getter]
    fn post_passes(&self) -> Vec<String> {
        self.inner
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

/// Rows are `"PythonName" => WrapperType = pass, doc`; `post` rows are
/// rejected by `add`.
macro_rules! opt_passes {
    (
        main { $($mn:literal => $mty:ident = $mctor:expr, $mdoc:literal;)* }
        post { $($pn:literal => $pty:ident = $pctor:expr, $pdoc:literal;)* }
    ) => {
        $( pure_pass_class!($mn => $mty, $mdoc); )*
        $( pure_pass_class!($pn => $pty, $pdoc); )*

        /// Any pass-wrapper class.
        #[allow(clippy::enum_variant_names)]
        #[derive(FromPyObject)]
        pub enum PyOptPass {
            $( #[pyo3(annotation = $mn)] $mty($mty), )*
            $( #[pyo3(annotation = $pn)] $pty($pty), )*
        }

        impl PyOptPass {
            /// The pass for `add`; a post-pass is an error.
            fn into_erased(self) -> PyResult<ErasedPass> {
                Ok(match self {
                    $( PyOptPass::$mty(_) => &$mctor, )*
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

            /// The pass for `add_post`.
            fn into_erased_post(self) -> ErasedPass {
                match self {
                    $( PyOptPass::$mty(_) => &$mctor, )*
                    $( PyOptPass::$pty(_) => &$pctor, )*
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

// Each text equals its class docstring in `strider/opt.pyi` (`test_docstrings.py`).
opt_passes! {
    main {
        "ConstantFold" => PyConstantFold = strider_orchestrator::opt::ConstantFold,
            "Evaluates constant expressions and applies algebraic identities (`x+0` \
             to `x`, `x^x` to `0`, AND-mask merging).";
        "KnownBits" => PyKnownBits = strider_orchestrator::opt::KnownBits,
            "Tracks which bits are known to be zero or one and simplifies \
             operations whose result is fully determined.";
        "PhiCollapse" => PyPhiCollapse = strider_orchestrator::opt::PhiCollapse,
            "Removes trivial phis: a `Phi` / `MemPhi`, or a strongly connected group \
             of them, whose inputs other than themselves are one value is replaced by \
             that value.";
        "RegionCollapse" => PyRegionCollapse = strider_orchestrator::opt::RegionCollapse,
            "Collapses a `Region` with exactly one control input into that \
             predecessor, replacing each phi over it with its lone value input.";
        "DeadBranchElimination" => PyDeadBranchElimination =
            strider_orchestrator::opt::DeadBranchElimination,
            "Folds an `If` or `Switch` on a constant selector to its live successor, \
             then strips the `Region` predecessors entry can no longer reach, with \
             their `Phi` / `MemPhi` slots.";
        "FlagCmpCanonicalize" => PyFlagCmpCanonicalize =
            strider_orchestrator::opt::FlagCmpCanonicalize,
            "Folds an architecture's condition-flag tree into a single direct \
             comparison node.";
        "IfCondInversion" => PyIfCondInversion = strider_orchestrator::opt::IfCondInversion,
            "Rewrites a branch on a negated condition (`Xor(C, IntConst(1)):I1`) \
             into a branch on the plain condition with its two arms swapped.";
        "LoadForward" => PyLoadForward = strider_orchestrator::opt::LoadForward,
            "Forwards a store's value to a later load when the nearest may-aliasing \
             memory definition is a store to the same address covering the whole \
             load. A partial overlap, `InitialMemory`, a `MemPhi` whose arms \
             disagree, or a call the assumptions cannot see through blocks it.";
        "LoadReadOnly" => PyLoadReadOnly = strider_orchestrator::opt::LoadReadOnly,
            "Folds a load from a constant address into a constant read from the \
             read-only memory image (`rom=`); a no-op without one.\n\nThe fold \
             ignores the load's memory chain, so the rom must map only \
             runtime-immutable memory: code and `.rodata`, never `.data` / `.got`.";
    }
    post {
        "FunctionArgDetect" => PyFunctionArgDetect = strider_orchestrator::opt::FunctionArgDetect,
            "Records which values carry each incoming stack-passed argument. \
             Register arguments are recorded at lift time.";
        "CallStackArgCollect" => PyCallStackArgCollect =
            strider_orchestrator::opt::CallStackArgCollect,
            "Wires the stack slots written before a call in as that call's \
             positional arguments.";
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyOptimizerPipeline>()?;
    register_passes(m)
}
