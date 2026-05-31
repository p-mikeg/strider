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

    // Build a Strider (the Python-facing wrapper) so callers that pass
    // the user-facing `Strider` class through `analyze_cfg` see the same
    // resolved CC the orchestrator does.  The orchestrator itself doesn't
    // consume this — it constructs its own RunConfig below — but
    // building it here surfaces CC-resolution errors early.
    let _strider_obj = Py::new(
        py,
        PyStrider::new_internal(py, arch.clone(), &sleigh_arc, cc.clone())?,
    )?;

    // Build the second Sleigh handle (orchestrator-owned, fresh
    // reader).  This is consumed by RunConfig.
    let orch_sleigh = rsleigh::Sleigh::new(arch.inner.sla_spec(), arch.inner.pspec(), reader_for_orch)
        .map_err(|e| into_strider_err(anyhow::anyhow!("Sleigh::new failed: {e:?}")))?;

    // The orchestrator owns the rom via `Box<dyn ReadOnlyMemory>` so
    // it can thread it down as `&dyn ReadOnlyMemory` through the
    // optimizer's `OptCtx`.  The `PyReadOnlyMemoryAdapter` adapter
    // refcounts the Python object itself (`Py<PyAny>`), so a single
    // boxed owner suffices for the duration of the run.
    let rom_box: Option<Box<dyn strider_analyze::opt::ReadOnlyMemory>> =
        rom.map(MemInput::into_box);

    // per_address_ccs currently only supports preset-form CCs (the
    // orchestrator's RunConfig field resolves them against Sleigh at
    // startup).  Custom CCs are already resolved, so feeding them
    // here would mean carrying two parallel maps through RunConfig —
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
    // Construct the RunConfig before `allow_threads`: `RunConfig::new` /
    // `from_built_cc` need the function-default CC and resolve it
    // against the sleigh's register table, paths that may surface a
    // typed `StriderError` that must be turned into `PyErr` while we
    // still hold the GIL.  After construction the `RunConfig` owns the
    // sleigh, the rom, and every CC, so the loop runs without the GIL.
    let arch_inner = arch.inner;
    let options = strider_analyze::RunOptions {
        rom: rom_box,
        fn_max_size: function_max_size,
        allow_code_before_start_addr,
        compact,
        per_address_ccs_unbuilt: per_address_ccs,
    };
    let config = match cc.inner {
        crate::cc::CcImpl::Preset(preset) => strider_analyze::RunConfig::new(
            arch_inner,
            preset,
            orch_sleigh,
            entry.into(),
            options,
        )
        .map_err(into_strider_err)?,
        crate::cc::CcImpl::Custom(built) => strider_analyze::RunConfig::from_built_cc(
            arch_inner,
            *built,
            orch_sleigh,
            entry.into(),
            options,
        )
        .map_err(into_strider_err)?,
    };
    let function = py.allow_threads(|| strider_analyze::run(config))
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
    // The rom now travels via the optimizer's `OptCtx` rather than
    // being attached to a `LoadReadOnly` pass instance.  Prepend a
    // unit `LoadReadOnly` if the caller supplied a rom so the
    // pipeline definitely runs the fold step (even if the user's
    // hand-built pipeline didn't add it explicitly); the actual rom
    // is bound into the ctx below.  No-op when `rom` is `None`.
    let rom_box: Option<Box<dyn strider_analyze::opt::ReadOnlyMemory>> =
        rom.map(MemInput::into_box);
    if rom_box.is_some() {
        pipeline.prepend_load_read_only()?;
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
    let cfg_borrow = cfg_obj.borrow(py);
    let sleigh_borrow = cfg_borrow.sleigh.borrow(py);
    let outcome = strider_borrow
        .inner
        .analyze_cfg_with(
            &cfg_borrow.inner,
            &sleigh_borrow.inner,
            strider_analyze::AnalyzeOptions {
                per_address_ccs: Some(&per_address_built_ccs),
                ..strider_analyze::AnalyzeOptions::default()
            },
        )
        .map_err(into_strider_err)?;
    drop(sleigh_borrow);
    drop(cfg_borrow);
    let function = outcome.function;
    drop(strider_borrow);
    let py_function = Py::new(py, PyFunction::new(function, cfg_obj.clone_ref(py)))?;

    let actual_pipeline = pipeline.drain_into_pipeline()?;
    {
        let py_function_borrow = py_function.borrow(py);
        let mut function = py_function_borrow.write_inner().map_err(into_strider_err)?;
        let ctx = match rom_box.as_deref() {
            Some(rom) => strider_analyze::opt::OptCtx::with_rom(rom),
            None => strider_analyze::opt::OptCtx::empty(),
        };
        actual_pipeline
            .run(&mut function, &ctx)
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
