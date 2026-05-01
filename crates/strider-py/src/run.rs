//! `strider.run` convenience entry point.
//!
//! Wraps the canonical building-blocks path: build a Sleigh, build a
//! CFG, build a Strider, analyze, optimize.  Returns a `RunResult`
//! with `cfg`, `graph`, and the original `sleigh` (handed back to the
//! user even though its inner Sleigh has been consumed by build_cfg
//! — Sleigh.regs remains accessible).
//!
//! v1 does NOT drive the indirect-branch fixed-point loop —
//! `strider::run` (the Rust orchestrator) requires the Sleigh to wrap
//! its reader in a `BufMemReader<B>`, which the Python wrapper's
//! `PyMemoryMapReader` does not satisfy.  Users who need indirect-
//! branch resolution should use the Rust API directly for now.

use pyo3::prelude::*;

use crate::arch::PySleighArch;
use crate::cc::PyCallingConvention;
use crate::cfg::{build_cfg, PyCfg};
use crate::errors::into_lift_err;
use crate::graph::PyGraph;
use crate::reader::PyMemoryMap;
use crate::sleigh::PySleigh;
use crate::strider_cls::PyStrider;

#[pyclass(name = "RunResult", module = "strider")]
pub struct PyRunResult {
    #[pyo3(get)]
    cfg: Py<PyCfg>,
    #[pyo3(get)]
    graph: Py<PyGraph>,
    #[pyo3(get)]
    sleigh: Py<PySleigh>,
}

#[pyfunction(signature = (
    arch,
    cc,
    mem,
    entry,
    rom = None,
    pipeline = None,
    allow_code_before_start_addr = false,
    function_max_size = None,
))]
#[allow(clippy::too_many_arguments)]
pub fn run(
    py: Python<'_>,
    arch: PySleighArch,
    cc: PyCallingConvention,
    mem: PyMemoryMap,
    entry: u64,
    rom: Option<PyMemoryMap>,
    pipeline: Option<&crate::opt::PyOptimizerPipeline>,
    allow_code_before_start_addr: bool,
    function_max_size: Option<u64>,
) -> PyResult<PyRunResult> {
    // 1. Construct Sleigh.
    let sleigh = Py::new(py, PySleigh::new_internal(arch.clone(), mem)?)?;

    // 2. Build Strider (must happen BEFORE build_cfg consumes Sleigh).
    let s = PyStrider::new_internal(py, arch.clone(), &sleigh, cc.clone())?;
    let strider_obj = Py::new(py, s)?;

    // 3. Build CFG (consumes the Sleigh's inner).
    let cfg_obj = Py::new(
        py,
        build_cfg(py, sleigh.clone_ref(py), entry, allow_code_before_start_addr, function_max_size)?,
    )?;

    // 4. Analyze CFG.
    let strider_borrow = strider_obj.borrow(py);
    let outcome = strider_borrow
        .inner
        .analyze_cfg(&cfg_obj.borrow(py).inner)
        .map_err(into_lift_err)?;
    let graph = outcome.graph;
    drop(strider_borrow);
    let py_graph = Py::new(py, PyGraph::new(graph, cfg_obj.clone_ref(py)))?;

    // 5. Optimize, building a default pipeline if the user didn't supply one.
    let actual_pipeline = match pipeline {
        Some(p) => p.drain_into_pipeline()?,
        None => {
            let strider_borrow = strider_obj.borrow(py);
            let cc_built = strider_borrow.inner.calling_convention().clone();
            let arch_copy = strider_borrow.arch;
            drop(strider_borrow);
            let p = crate::opt::PyOptimizerPipeline::new_full_default(cc_built, arch_copy);
            // If the user supplied a rom, layer LoadReadOnly on top.
            if let Some(rom_map) = rom {
                p.add_load_readonly(rom_map)?;
            }
            p.drain_into_pipeline()?
        }
    };
    {
        let py_graph_borrow = py_graph.borrow(py);
        let mut graph = py_graph_borrow
            .write_inner()
            .map_err(crate::errors::into_strider_err)?;
        actual_pipeline.run_on_built(&mut graph).map_err(|e| {
            crate::errors::into_strider_err(anyhow::anyhow!("optimize failed: {e:?}"))
        })?;
    }

    Ok(PyRunResult {
        cfg: cfg_obj,
        graph: py_graph,
        sleigh,
    })
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyRunResult>()?;
    m.add_function(wrap_pyfunction!(run, m)?)?;
    Ok(())
}
