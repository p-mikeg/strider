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
use crate::errors::into_lift_err;
use crate::graph::PyGraph;
use crate::sleigh::PySleigh;

#[pyclass(name = "Strider", module = "strider")]
pub struct PyStrider {
    pub(crate) inner: strider_analyze::Strider,
    /// Cached arch — `strider_analyze::Strider` keeps `arch` private so we
    /// stash a copy here for pipeline-construction helpers.
    pub(crate) arch: target::SleighArch,
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
        let sleigh_borrow = sleigh.borrow(py);
        let regs = sleigh_borrow.regs.clone();
        drop(sleigh_borrow);
        let arch_copy = arch.inner;
        let inner = strider_analyze::Strider::new(arch.inner, regs, cc.inner).map_err(into_lift_err)?;
        Ok(Self {
            inner,
            arch: arch_copy,
        })
    }

    fn analyze_cfg(&self, py: Python<'_>, cfg: Py<PyCfg>) -> PyResult<PyAnalyzeOutcome> {
        let cfg_borrow = cfg.borrow(py);
        let outcome = self
            .inner
            .analyze_cfg(&cfg_borrow.inner)
            .map_err(into_lift_err)?;
        let unresolved_branch_count = outcome.unresolved_branches.len();
        let region_count = outcome.region_handles.len();
        let graph = outcome.graph;
        drop(cfg_borrow);
        let py_graph = Py::new(py, PyGraph::new(graph, cfg))?;
        Ok(PyAnalyzeOutcome {
            graph: py_graph,
            unresolved_branch_count,
            region_count,
        })
    }

    /// Mirror of `strider_analyze::Strider::build_optimizer_pipeline`.  Adds
    /// the convention-aware StackStoreDetect / StackLoadForward fixed-
    /// point passes plus CallStackArgCollect / FunctionArgDetect post
    /// passes on top of the default pipeline.
    fn build_optimizer_pipeline(&self) -> PyResult<crate::opt::PyOptimizerPipeline> {
        let cc = self.inner.calling_convention().clone();
        let arch = *self.calling_convention_arch();
        Ok(crate::opt::PyOptimizerPipeline::new_full_default(cc, arch))
    }

    /// Mirror of `strider_analyze::Strider::build_stable_optimizer_pipeline`.
    fn build_stable_optimizer_pipeline(&self) -> PyResult<crate::opt::PyOptimizerPipeline> {
        let cc = self.inner.calling_convention().clone();
        let arch = *self.calling_convention_arch();
        Ok(crate::opt::PyOptimizerPipeline::new_stable_default(cc, arch))
    }

    /// Mirror of `strider_analyze::Strider::build_destructive_optimizer_pipeline`.
    fn build_destructive_optimizer_pipeline(&self) -> PyResult<crate::opt::PyOptimizerPipeline> {
        let cc = self.inner.calling_convention().clone();
        Ok(crate::opt::PyOptimizerPipeline::new_destructive_default(cc))
    }
}

impl PyStrider {
    fn calling_convention_arch(&self) -> &target::SleighArch {
        &self.arch
    }

    /// Internal constructor used by `strider.run`.
    pub(crate) fn new_internal(
        py: Python<'_>,
        arch: PySleighArch,
        sleigh: &Py<PySleigh>,
        cc: PyCallingConvention,
    ) -> PyResult<Self> {
        let sleigh_borrow = sleigh.borrow(py);
        let regs = sleigh_borrow.regs.clone();
        drop(sleigh_borrow);
        let arch_copy = arch.inner;
        let inner = strider_analyze::Strider::new(arch.inner, regs, cc.inner).map_err(into_lift_err)?;
        Ok(Self {
            inner,
            arch: arch_copy,
        })
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyStrider>()?;
    m.add_class::<PyAnalyzeOutcome>()?;
    Ok(())
}
