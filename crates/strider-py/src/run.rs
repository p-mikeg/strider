//! `strider.run` convenience entry point.
//!
//! Delegates to the canonical Rust orchestrator
//! (`strider::run(RunConfig)`) which drives the indirect-branch
//! fixed-point loop, runs the stable optimiser between iterations,
//! and finally runs the destructive subset once.  Works for both
//! `MemoryMap` and Python-callback `MemReader` subclasses since the
//! orchestrator is generic over `R: rsleigh::MemReader`.
//!
//! When the user supplies a `pipeline=...` argument we bypass the
//! orchestrator and run their pipeline against a single-iteration
//! `analyze_cfg` lift — they're saying "just analyse, I'll do my own
//! optimisation".

use pyo3::prelude::*;

use crate::arch::PySleighArch;
use crate::cc::PyCallingConvention;
use crate::cfg::PyCfg;
use crate::errors::{into_lift_err, into_strider_err};
use crate::graph::PyGraph;
use crate::reader::{AnyMemReader, ReaderInput, ReaderInputClone, RomInput};
use crate::sleigh::PySleigh;
use crate::strider_cls::PyStrider;

#[pyclass(name = "RunResult", module = "strider")]
pub struct PyRunResult {
    /// Snapshot CFG built from the same memory reader the orchestrator
    /// uses internally.  Reflects the structure visible at the entry
    /// point — note that the orchestrator's iterations may rebuild the
    /// CFG several times to resolve indirect branches; this field
    /// carries the FINAL post-resolution snapshot only when the
    /// orchestrator did not have to rebuild.  For simple functions
    /// (no indirect branches), `cfg` matches what the orchestrator
    /// uses.
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
    mem: ReaderInput,
    entry: u64,
    rom: Option<RomInput>,
    pipeline: Option<&crate::opt::PyOptimizerPipeline>,
    allow_code_before_start_addr: bool,
    function_max_size: Option<u64>,
) -> PyResult<PyRunResult> {
    if pipeline.is_some() {
        return run_with_custom_pipeline(
            py,
            arch,
            cc,
            mem,
            entry,
            rom,
            pipeline.expect("checked above"),
            allow_code_before_start_addr,
            function_max_size,
        );
    }

    run_via_orchestrator(
        py,
        arch,
        cc,
        mem,
        entry,
        rom,
        allow_code_before_start_addr,
        function_max_size,
    )
}

/// Orchestrator path — the canonical strider::run flow.  Drives the
/// indirect-branch fixed-point loop and returns the final IR graph.
#[allow(clippy::too_many_arguments)]
fn run_via_orchestrator(
    py: Python<'_>,
    arch: PySleighArch,
    cc: PyCallingConvention,
    mem: ReaderInput,
    entry: u64,
    rom: Option<RomInput>,
    allow_code_before_start_addr: bool,
    function_max_size: Option<u64>,
) -> PyResult<PyRunResult> {
    // Snapshot the reader so we can hand a fresh AnyMemReader to both
    // the orchestrator (consumed) and the snapshot CFG (consumed).
    let reader_clone: ReaderInputClone = mem.into_clone()?;
    let reader_for_orch = reader_clone.materialise().map_err(into_lift_err)?;
    let reader_for_cfg = reader_clone.materialise().map_err(into_lift_err)?;

    // Build a Sleigh handle the user can keep (its inner is consumed
    // by the snapshot CFG below; Sleigh.regs remains accessible).
    let py_sleigh = PySleigh::new_internal(arch.clone(), reader_for_cfg)?;
    let sleigh_arc = Py::new(py, py_sleigh)?;

    // Build the snapshot CFG from the user-facing Sleigh.
    let cfg_obj = Py::new(
        py,
        crate::cfg::build_cfg(
            py,
            sleigh_arc.clone_ref(py),
            entry,
            allow_code_before_start_addr,
            function_max_size,
        )?,
    )?;

    // Build a Strider for the orchestrator.
    let strider_obj = Py::new(
        py,
        PyStrider::new_internal(py, arch.clone(), &sleigh_arc, cc.clone())?,
    )?;

    // Build the second Sleigh handle (orchestrator-owned, fresh
    // reader).  This is consumed by RunConfig.
    let orch_sleigh = rsleigh::Sleigh::new(arch.inner.sla_spec, arch.inner.pspec, reader_for_orch)
        .map_err(|e| into_lift_err(anyhow::anyhow!("Sleigh::new failed: {e:?}")))?;

    let rom_arc = rom.map(|r| r.into_arc());

    let strider_borrow = strider_obj.borrow(py);
    let config = strider::RunConfig {
        strider: &strider_borrow.inner,
        start_addr: entry,
        sleigh: orch_sleigh,
        rom: rom_arc,
        fn_max_size: function_max_size,
        allow_code_before_start_addr,
    };
    let graph = strider::run(config).map_err(into_strider_err)?;
    drop(strider_borrow);

    let py_graph = Py::new(py, PyGraph::new(graph, cfg_obj.clone_ref(py)))?;

    Ok(PyRunResult {
        cfg: cfg_obj,
        graph: py_graph,
        sleigh: sleigh_arc,
    })
}

/// Custom-pipeline path — preserves the v1 contract: lift once via
/// `analyze_cfg`, then apply the user's pipeline.  Indirect branches
/// are not resolved on this path.
#[allow(clippy::too_many_arguments)]
fn run_with_custom_pipeline(
    py: Python<'_>,
    arch: PySleighArch,
    cc: PyCallingConvention,
    mem: ReaderInput,
    entry: u64,
    rom: Option<RomInput>,
    pipeline: &crate::opt::PyOptimizerPipeline,
    allow_code_before_start_addr: bool,
    function_max_size: Option<u64>,
) -> PyResult<PyRunResult> {
    let _ = rom; // custom pipeline owns its own pass list
    let reader: AnyMemReader = mem.into_any().map_err(into_lift_err)?;
    let sleigh = Py::new(py, PySleigh::new_internal(arch.clone(), reader)?)?;

    let s = PyStrider::new_internal(py, arch.clone(), &sleigh, cc.clone())?;
    let strider_obj = Py::new(py, s)?;

    let cfg_obj = Py::new(
        py,
        crate::cfg::build_cfg(
            py,
            sleigh.clone_ref(py),
            entry,
            allow_code_before_start_addr,
            function_max_size,
        )?,
    )?;

    let strider_borrow = strider_obj.borrow(py);
    let outcome = strider_borrow
        .inner
        .analyze_cfg(&cfg_obj.borrow(py).inner)
        .map_err(into_lift_err)?;
    let graph = outcome.graph;
    drop(strider_borrow);
    let py_graph = Py::new(py, PyGraph::new(graph, cfg_obj.clone_ref(py)))?;

    let actual_pipeline = pipeline.drain_into_pipeline()?;
    {
        let py_graph_borrow = py_graph.borrow(py);
        let mut graph = py_graph_borrow.write_inner().map_err(into_strider_err)?;
        actual_pipeline
            .run_on_built(&mut graph)
            .map_err(|e| into_strider_err(anyhow::anyhow!("optimize failed: {e:?}")))?;
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
