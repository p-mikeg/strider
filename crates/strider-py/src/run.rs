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
use crate::errors::into_strider_err;
use crate::function::PyFunction;
use crate::reader::{AnyMemReader, MemInput};
use crate::sleigh::PySleigh;
use crate::strider_cls::PyStrider;

/// Result of `strider.run`: the lifted/optimised `function`, a snapshot
/// `cfg`, and the `sleigh` handle.
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
    /// The lifted and optimised IR graph for the analysed function.
    #[pyo3(get)]
    function: Py<PyFunction>,
    /// The `Sleigh` handle used for the analysis (its `regs` stay
    /// accessible even though the inner Sleigh was consumed).
    #[pyo3(get)]
    sleigh: Py<PySleigh>,
}

/// Lift and analyse the function at `entry`, returning a `RunResult`.
///
/// With no `pipeline`, runs the canonical orchestrator (drives the
/// indirect-branch fixed-point loop, the stable optimiser between
/// iterations, then the destructive subset once).  Passing
/// `pipeline=` skips the orchestrator: it lifts once and applies your
/// pipeline (indirect branches are not resolved on that path).
///
/// Args:
///     arch: Target `SleighArch`.
///     cc: Default `CallingConvention` for the function.
///     mem: A `MemoryMap` or a `MemReader` subclass.
///     entry: Address of the function to analyse.
///     rom: Optional read-only memory for constant-load folding; on the
///         custom-pipeline path it is wired in via a prepended
///         `LoadReadOnly` pass.
///     pipeline: Optional `OptimizerPipeline` (drained on use).
///     allow_code_before_start_addr: Permit lifting before `entry`.
///     function_max_size: Optional byte bound past `entry`; must be > 0.
///     compact: Compact the IR arena after analysis (default `True`).
///     per_address_ccs: Per-target-address calling-convention overrides
///         (the orchestrator path accepts only preset CCs here).
///
/// Raises `ValueError` for `function_max_size == 0` and `StriderError`
/// on lift/analysis failure.
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
        .map_err(|e| into_strider_err(anyhow::anyhow!("Sleigh::new failed: {e:?}")))?;

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
    // per_address_ccs currently only supports preset-form CCs (the
    // orchestrator's Config field resolves them against Sleigh at
    // startup).  Custom CCs are already resolved, so feeding them
    // here would mean carrying two parallel maps through Config —
    // not yet wired.  Surface a clear error rather than silently
    // dropping the override.
    let per_address_ccs: rustc_hash::FxHashMap<u64, strider_target::CallingConvention> =
        per_address_ccs_py
            .into_iter()
            .map(|(addr, py_cc)| match py_cc.inner {
                crate::cc::CcImpl::Preset(preset) => Ok((addr, preset)),
                crate::cc::CcImpl::Custom(_) => Err(crate::errors::into_strider_err(
                    anyhow::anyhow!(
                        "per_address_ccs[{addr:#x}] = a custom CallingConvention; \
                         this field currently only accepts preset CCs.  Use \
                         a preset (e.g. x86_64_all_preserving) or open an issue \
                         for custom-CC per-address-override support."
                    )
                )),
            })
            .collect::<PyResult<_>>()?;
    let function = py.allow_threads(|| {
        let config = strider_analyze::Config {
            strider: &strider_owned,
            start_addr: entry.into(),
            sleigh: orch_sleigh,
            rom: rom_arc,
            fn_max_size: function_max_size,
            allow_code_before_start_addr,
            compact,
            per_address_ccs_unbuilt: per_address_ccs,
        };
        strider_analyze::run(config)
    })
    .map_err(into_strider_err)?;

    // If a Python callback inside the orchestrator (e.g. a custom
    // `ReadOnlyMemory.read` that raised `KeyboardInterrupt` /
    // `SystemExit`) stashed the PyErr in the thread-local
    // PENDING_CONTROL_FLOW cell, surface it here so PyO3 propagates
    // it as `Err(PyErr)` to the Python caller.
    if let Some(err) = crate::pattern::take_pending_control_flow() {
        return Err(err);
    }

    let py_function = Py::new(py, PyFunction::new(function, cfg_obj.clone_ref(py)))?;

    Ok(PyRunResult {
        cfg: cfg_obj,
        function: py_function,
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
    let per_address_built_ccs: rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention> =
        if per_address_ccs_py.is_empty() {
            rustc_hash::FxHashMap::default()
        } else {
            let regs = sleigh.borrow(py).regs.clone();
            per_address_ccs_py
                .into_iter()
                .map(|(addr, py_cc)| {
                    let built = match py_cc.inner {
                        crate::cc::CcImpl::Preset(preset) => preset.build(&regs).map_err(|e| {
                            into_strider_err(anyhow::anyhow!(
                                "per-address CC at {addr:#x} unresolved: {e:?}"
                            ))
                        })?,
                        // Custom CCs are already resolved at construction time.
                        crate::cc::CcImpl::Custom(built) => *built,
                    };
                    Ok((addr, built))
                })
                .collect::<PyResult<_>>()?
        };

    let strider_borrow = strider_obj.borrow(py);
    let outcome = strider_borrow
        .inner
        .analyze_cfg_with(
            &cfg_obj.borrow(py).inner,
            strider_analyze::AnalyzeOptions {
                per_address_ccs: Some(&per_address_built_ccs),
                ..strider_analyze::AnalyzeOptions::default()
            },
        )
        .map_err(into_strider_err)?;
    let function = outcome.function;
    drop(strider_borrow);
    let py_function = Py::new(py, PyFunction::new(function, cfg_obj.clone_ref(py)))?;

    let actual_pipeline = pipeline.drain_into_pipeline()?;
    {
        let py_function_borrow = py_function.borrow(py);
        let mut function = py_function_borrow.write_inner().map_err(into_strider_err)?;
        let entry = function.entry().ok_or_else(|| {
            into_strider_err(anyhow::anyhow!(
                "strider.run: function has not been built (entry is None)"
            ))
        })?;
        actual_pipeline
            .run(&mut function, entry)
            .map_err(|e| into_strider_err(anyhow::anyhow!("optimize failed: {e:?}")))?;
        if compact {
            function.compact().map_err(into_strider_err)?;
        }
    }

    // Same propagation as the orchestrator path: drain the pending
    // control-flow cell so a `KeyboardInterrupt`/`SystemExit` raised
    // inside e.g. a Python ReadOnlyMemory callback during LoadReadOnly
    // surfaces as `Err(PyErr)` rather than vanishing silently.
    if let Some(err) = crate::pattern::take_pending_control_flow() {
        return Err(err);
    }

    Ok(PyRunResult {
        cfg: cfg_obj,
        function: py_function,
        sleigh,
    })
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyRunResult>()?;
    m.add_function(wrap_pyfunction!(run, m)?)?;
    Ok(())
}
