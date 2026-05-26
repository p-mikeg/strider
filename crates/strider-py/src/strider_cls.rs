//! `PyStrider` — wraps `strider_analyze::Strider`, exposes `analyze_cfg`.
//!
//! Constructed with a `(SleighArch, Sleigh, CallingConvention)`
//! triple. The Sleigh is needed so we can read the register table to
//! resolve calling-convention register names; the Sleigh is *not*
//! consumed (`SleighRegs` is cloned out of the cached copy in
//! `PySleigh.regs`).

use pyo3::prelude::*;

use crate::arch::PySleighArch;
use crate::cc::PyCallingConvention;
use crate::cfg::PyCfg;
use crate::errors::into_strider_err;
use crate::graph::PyGraph;
use crate::sleigh::PySleigh;

#[pyclass(name = "Strider", module = "strider")]
pub struct PyStrider {
    pub(crate) inner: strider_analyze::Strider,
}

/// Mirror of `strider_analyze::AnalyzeOutcome`.
///
/// `unresolved_branches` and `region_handles` carry low-level lift
/// state used by the indirect-branch resolver in Rust; this binding
/// exposes only their counts so Python users can detect "did we have
/// any indirect branches?" without dragging the full payload across
/// the boundary.
#[pyclass(name = "AnalyzeOutcome", module = "strider")]
pub struct PyAnalyzeOutcome {
    #[pyo3(get)]
    pub(crate) graph: Py<PyGraph>,
    #[pyo3(get)]
    pub(crate) unresolved_branch_count: usize,
    #[pyo3(get)]
    pub(crate) region_count: usize,
}

#[pymethods]
impl PyStrider {
    #[new]
    fn new(
        py: Python<'_>,
        arch: PySleighArch,
        sleigh: Py<PySleigh>,
        cc: PyCallingConvention,
    ) -> PyResult<Self> {
        let inner = build_strider(py, arch, &sleigh, &cc)?;
        Ok(Self { inner })
    }

    fn analyze_cfg(&self, py: Python<'_>, cfg: Py<PyCfg>) -> PyResult<PyAnalyzeOutcome> {
        let cfg_borrow = cfg.borrow(py);
        let outcome = self
            .inner
            .analyze_cfg(&cfg_borrow.inner)
            .map_err(into_strider_err)?;
        let unresolved_branch_count = outcome.unresolved_branches.len();
        let region_count = outcome.region_count();
        let function = outcome.function;
        drop(cfg_borrow);
        let py_graph = Py::new(py, PyGraph::new(function, cfg))?;
        Ok(PyAnalyzeOutcome {
            graph: py_graph,
            unresolved_branch_count,
            region_count,
        })
    }

    /// Mirror of `strider_analyze::Strider::build_optimizer_pipeline`.  Adds
    /// the convention-aware LoadForward fixed-point pass plus
    /// CallStackArgCollect / FunctionArgDetect post passes on top of the
    /// default pipeline.
    fn build_optimizer_pipeline(&self) -> crate::opt::PyOptimizerPipeline {
        crate::opt::PyOptimizerPipeline::new_full_default(&self.inner)
    }

    /// Mirror of `strider_analyze::Strider::build_stable_optimizer_pipeline`.
    fn build_stable_optimizer_pipeline(&self) -> crate::opt::PyOptimizerPipeline {
        crate::opt::PyOptimizerPipeline::new_stable_default(&self.inner)
    }

    /// Mirror of `strider_analyze::Strider::build_destructive_optimizer_pipeline`.
    fn build_destructive_optimizer_pipeline(&self) -> crate::opt::PyOptimizerPipeline {
        crate::opt::PyOptimizerPipeline::new_destructive_default(&self.inner)
    }
}

impl PyStrider {
    /// Internal constructor used by `strider.run`.
    pub(crate) fn new_internal(
        py: Python<'_>,
        arch: PySleighArch,
        sleigh: &Py<PySleigh>,
        cc: PyCallingConvention,
    ) -> PyResult<Self> {
        let inner = build_strider(py, arch, sleigh, &cc)?;
        Ok(Self { inner })
    }
}

/// Build a `strider_analyze::Strider` from a `PyCallingConvention`.
/// Routes through either `Strider::new` (presets — does name
/// resolution against `sleigh`'s regs) or `Strider::new_with_built_cc`
/// (custom — CC is already resolved at construction time).
fn build_strider(
    py: Python<'_>,
    arch: PySleighArch,
    sleigh: &Py<PySleigh>,
    cc: &PyCallingConvention,
) -> PyResult<strider_analyze::Strider> {
    let sleigh_borrow = sleigh.borrow(py);
    let regs = sleigh_borrow.regs.clone();
    drop(sleigh_borrow);
    match &cc.inner {
        crate::cc::CcImpl::Preset(preset) => {
            strider_analyze::Strider::new(arch.inner, regs, *preset).map_err(into_strider_err)
        }
        crate::cc::CcImpl::Custom(built) => {
            Ok(strider_analyze::Strider::new_with_built_cc(
                arch.inner,
                regs,
                *built.clone(),
            ))
        }
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyStrider>()?;
    m.add_class::<PyAnalyzeOutcome>()?;
    Ok(())
}
