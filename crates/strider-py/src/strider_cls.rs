//! `PyStrider` — wraps `strider::Strider`, exposes `analyze_cfg`.
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
    pub(crate) inner: strider::Strider,
}

/// Mirror of `strider::AnalyzeOutcome`.
///
/// `unresolved_branches` and `region_handles` carry low-level lift
/// state used by the indirect-branch resolver in Rust; v1 exposes
/// only their counts so Python users can detect "did we have any
/// indirect branches?" without dragging the full payload across the
/// boundary.
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
        let inner = strider::Strider::new(arch.inner, regs, cc.inner).map_err(into_lift_err)?;
        Ok(Self { inner })
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
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyStrider>()?;
    m.add_class::<PyAnalyzeOutcome>()?;
    Ok(())
}
