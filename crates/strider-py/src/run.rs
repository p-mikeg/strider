//! `strider.run` convenience entry point.
//!
//! Delegates to the canonical Rust orchestrator
//! (`strider_analyze::run(Config)`) which drives the indirect-branch
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
use crate::reader::{AnyMemReader, MemInput};
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
    compact = true,
    per_address_ccs = None,
))]
#[allow(clippy::too_many_arguments)]
pub fn run(
    py: Python<'_>,
    arch: PySleighArch,
    cc: PyCallingConvention,
    mem: MemInput,
    entry: u64,
    rom: Option<MemInput>,
    pipeline: Option<&crate::opt::PyOptimizerPipeline>,
    allow_code_before_start_addr: bool,
    function_max_size: Option<u64>,
    compact: bool,
    per_address_ccs: Option<std::collections::HashMap<u64, PyCallingConvention>>,
) -> PyResult<PyRunResult> {
    // Reject `function_max_size=0` at the Python boundary with a typed
    // `ValueError` rather than letting it reach the Rust builder where
    // it would be silently coerced to unbounded (a Python user expects
    // an exception, not silent behavioural change).  A zero-byte
    // function bound is meaningless and historically caused the
    // lifter to decode past `entry`.
    if matches!(function_max_size, Some(0)) {
        return Err(pyo3::exceptions::PyValueError::new_err(
            "function_max_size must be > 0 (zero is meaningless — omit the argument for unbounded)",
        ));
    }
    let per_address_ccs = per_address_ccs.unwrap_or_default();
    match pipeline {
        Some(p) => run_with_custom_pipeline(
            py,
            arch,
            cc,
            mem,
            entry,
            rom,
            p,
            allow_code_before_start_addr,
            function_max_size,
            compact,
            per_address_ccs,
        ),
        None => run_via_orchestrator(
            py,
            arch,
            cc,
            mem,
            entry,
            rom,
            allow_code_before_start_addr,
            function_max_size,
            compact,
            per_address_ccs,
        ),
    }
}

/// Orchestrator path — the canonical strider_analyze::run flow.  Drives the
/// indirect-branch fixed-point loop and returns the final IR graph.
#[allow(clippy::too_many_arguments)]
fn run_via_orchestrator(
    py: Python<'_>,
    arch: PySleighArch,
    cc: PyCallingConvention,
    mem: MemInput,
    entry: u64,
    rom: Option<MemInput>,
    allow_code_before_start_addr: bool,
    function_max_size: Option<u64>,
    compact: bool,
    per_address_ccs_py: std::collections::HashMap<u64, PyCallingConvention>,
) -> PyResult<PyRunResult> {
    // Snapshot the reader so we can hand a fresh AnyMemReader to both
    // the orchestrator (consumed) and the snapshot CFG (consumed).
    let reader_for_cfg = mem.clone_one()?.into_any();
    let reader_for_orch = mem.into_any();

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
    // reader).  This is consumed by Config.
    let orch_sleigh = rsleigh::Sleigh::new(arch.inner.sla_spec(), arch.inner.pspec(), reader_for_orch)
        .map_err(|e| into_lift_err(anyhow::anyhow!("Sleigh::new failed: {e:?}")))?;

    let rom_arc = rom.map(|r| r.into_arc());

    // Snapshot the Strider out of the PyRef so we can release the GIL
    // across the long-running strider_analyze::run call.  Strider is cheap to
    // clone (three Clone fields), and detaching the borrow lets other
    // Python threads run during the lift / fixed-point loop.  Callback
    // readers (PyMemReaderAdapter::read) re-acquire the GIL via
    // Python::with_gil per-call, so Cb readers stay correct.
    let strider_owned: strider_analyze::Strider = {
        let borrow = strider_obj.borrow(py);
        borrow.inner.clone()
    };
    let per_address_ccs: std::collections::HashMap<u64, strider_target::CallingConvention> =
        per_address_ccs_py
            .into_iter()
            .map(|(addr, py_cc)| (addr, py_cc.inner))
            .collect();
    let graph = py.allow_threads(|| {
        let config = strider_analyze::Config {
            strider: &strider_owned,
            start_addr: entry.into(),
            sleigh: orch_sleigh,
            rom: rom_arc,
            fn_max_size: function_max_size,
            allow_code_before_start_addr,
            compact,
            per_address_ccs,
        };
        strider_analyze::run(config)
    })
    .map_err(into_strider_err)?;

    let py_graph = Py::new(py, PyGraph::new(graph, cfg_obj.clone_ref(py)))?;

    Ok(PyRunResult {
        cfg: cfg_obj,
        graph: py_graph,
        sleigh: sleigh_arc,
    })
}

/// Custom-pipeline path — lift once via `analyze_cfg_with`, then apply
/// the user's pipeline.  Indirect branches are not resolved on this
/// path.  `per_address_ccs` is honoured at lift time the same way as
/// on the orchestrator path.
#[allow(clippy::too_many_arguments)]
fn run_with_custom_pipeline(
    py: Python<'_>,
    arch: PySleighArch,
    cc: PyCallingConvention,
    mem: MemInput,
    entry: u64,
    rom: Option<MemInput>,
    pipeline: &crate::opt::PyOptimizerPipeline,
    allow_code_before_start_addr: bool,
    function_max_size: Option<u64>,
    compact: bool,
    per_address_ccs_py: std::collections::HashMap<u64, PyCallingConvention>,
) -> PyResult<PyRunResult> {
    // Wire the user-supplied rom into the pipeline by prepending a
    // LoadReadOnly pass.  Previously the rom was silently discarded on
    // this path — users with custom pipelines who passed `rom=mem`
    // expecting LoadReadOnly to fold loads got no folding at all.
    // Pass `rom=None` to opt out.
    if let Some(rom_input) = rom {
        let rom_arc = rom_input.into_arc();
        pipeline.prepend_load_read_only(rom_arc)?;
    }
    let reader: AnyMemReader = mem.into_any();
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

    // Resolve per-address CCs against the same Sleigh register table
    // the function-default CC was built against — mirrors the
    // orchestrator's `LoopState::new` behaviour so both pipeline paths
    // honour `per_address_ccs` identically.
    let per_address_built_ccs: std::collections::HashMap<u64, strider_target::BuiltCallingConvention> =
        if per_address_ccs_py.is_empty() {
            std::collections::HashMap::new()
        } else {
            let regs = sleigh.borrow(py).regs.clone();
            per_address_ccs_py
                .into_iter()
                .map(|(addr, py_cc)| {
                    py_cc
                        .inner
                        .build(&regs)
                        .map(|built| (addr, built))
                        .map_err(|e| {
                            into_lift_err(anyhow::anyhow!(
                                "per-address CC at {addr:#x} unresolved: {e:?}"
                            ))
                        })
                })
                .collect::<Result<_, _>>()?
        };

    let strider_borrow = strider_obj.borrow(py);
    let outcome = strider_borrow
        .inner
        .analyze_cfg_with(
            &cfg_obj.borrow(py).inner,
            strider_analyze::AnalyzeOptions {
                per_address_ccs: &per_address_built_ccs,
                ..strider_analyze::AnalyzeOptions::default()
            },
        )
        .map_err(into_lift_err)?;
    let graph = outcome.graph;
    drop(strider_borrow);
    let py_graph = Py::new(py, PyGraph::new(graph, cfg_obj.clone_ref(py)))?;

    let actual_pipeline = pipeline.drain_into_pipeline()?;
    {
        let py_graph_borrow = py_graph.borrow(py);
        let mut graph = py_graph_borrow.write_inner().map_err(into_strider_err)?;
        let entry = graph.entry().ok_or_else(|| {
            into_strider_err(anyhow::anyhow!(
                "strider.run: graph has not been built (entry is None)"
            ))
        })?;
        actual_pipeline
            .run(graph.graph_mut(), entry)
            .map_err(|e| into_strider_err(anyhow::anyhow!("optimize failed: {e:?}")))?;
        if compact {
            graph.compact().map_err(into_strider_err)?;
        }
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
