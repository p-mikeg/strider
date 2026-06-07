//! `strider.run` convenience entry point.
//!
//! Delegates to the canonical Rust orchestrator
//! (`strider_orchestrator::Strider::analyze`) which drives the
//! indirect-branch fixed-point loop, running the full optimiser pipeline
//! on each iteration.  Works for both `MemoryMap` and Python-callback
//! `MemReader` subclasses since the orchestrator is generic over
//! `R: rsleigh::MemReader`.
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
use crate::reader::MemInput;
use crate::sleigh::PySleigh;
use crate::strider_cls::PyLifter;

/// Resolve a `PyCallingConvention` against an already-fetched register
/// table into a `BuiltCallingConvention` (preset → resolve; custom →
/// already-resolved clone).  Shared by `strider.run` and the `Strider`
/// run pyclass so both paths build CCs identically.
pub(crate) fn build_cc(
    cc: &PyCallingConvention,
    regs: &rsleigh::SleighRegs,
) -> PyResult<strider_target::BuiltCallingConvention> {
    match &cc.inner {
        crate::cc::CcImpl::Preset(preset) => preset.build(regs).map_err(into_strider_err),
        crate::cc::CcImpl::Custom(built) => Ok(*built.clone()),
    }
}

/// Resolve a map of per-target-address calling-convention overrides
/// against `regs`.  Both preset and custom CCs are accepted (custom CCs
/// are already resolved at construction).  Shared by `strider.run` and
/// the `Strider` run pyclass.
pub(crate) fn build_per_address_ccs(
    per_address_ccs_py: std::collections::HashMap<u64, PyCallingConvention>,
    regs: &rsleigh::SleighRegs,
) -> PyResult<rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>> {
    per_address_ccs_py
        .into_iter()
        .map(|(addr, py_cc)| {
            let built = match py_cc.inner {
                crate::cc::CcImpl::Preset(preset) => preset.build(regs).map_err(|e| {
                    into_strider_err(anyhow::anyhow!("per-address CC at {addr:#x} unresolved: {e:?}"))
                })?,
                crate::cc::CcImpl::Custom(built) => *built,
            };
            Ok((addr, built))
        })
        .collect::<PyResult<_>>()
}

/// Reject `function_max_size=0` at the Python boundary with a typed
/// `ValueError` (zero is meaningless — the Rust builder would silently
/// coerce it to unbounded).  Shared by `strider.run` and the `Strider`
/// run pyclass.
pub(crate) fn reject_zero_max_size(function_max_size: Option<u64>) -> PyResult<()> {
    if matches!(function_max_size, Some(0)) {
        return Err(pyo3::exceptions::PyValueError::new_err(
            "function_max_size must be > 0 (zero is meaningless — omit the argument for unbounded)",
        ));
    }
    Ok(())
}

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
/// indirect-branch fixed-point loop, running the full optimiser
/// pipeline each iteration).  Passing `pipeline=` skips the
/// orchestrator: it lifts once and applies your pipeline (indirect
/// branches are not resolved on that path).
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
    reject_zero_max_size(function_max_size)?;
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

/// Orchestrator path — the canonical `strider_orchestrator::Strider::analyze`
/// flow.  Drives the indirect-branch fixed-point loop and returns the
/// final IR graph.
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
    // Snapshot the reader so we can hand a fresh AnyMemReader to the
    // orchestrator (consumed), the snapshot CFG's `Lifter` (consumed),
    // and the user-facing `Sleigh` handle (consumed).
    let reader_for_lifter = mem.clone_one()?;
    let reader_for_sleigh = mem.clone_one()?.into_any();
    let reader_for_orch = mem.into_any();

    // Build a Sleigh handle the user can keep (returned on the
    // `RunResult`).  Independent of the snapshot CFG's owned Sleigh.
    let py_sleigh = PySleigh::new_internal(arch.clone(), reader_for_sleigh)?;
    let sleigh_arc = Py::new(py, py_sleigh)?;

    // Build the snapshot CFG via a throwaway `Lifter` that owns a fresh
    // Sleigh.  Building the `Lifter` surfaces CC-resolution errors early;
    // its owned Sleigh resolves register names for dot rendering on the
    // returned `Function`.  The orchestrator builds its own
    // `strider_orchestrator::Strider` below.
    let snapshot_lifter = Py::new(
        py,
        PyLifter::new_internal(arch.clone(), reader_for_lifter, cc.clone())?,
    )?;
    let cfg_obj = Py::new(
        py,
        PyLifter::build_cfg_internal(
            snapshot_lifter,
            py,
            entry,
            allow_code_before_start_addr,
            function_max_size,
        )?,
    )?;

    // Build the orchestrator-owned Sleigh handle (fresh reader).  This is
    // consumed by `strider_orchestrator::Strider`.
    let orch_sleigh = rsleigh::Sleigh::new(arch.inner.sla_spec(), arch.inner.pspec(), reader_for_orch)
        .map_err(|e| into_strider_err(anyhow::anyhow!("Sleigh::new failed: {e:?}")))?;

    // The orchestrator owns the rom via `Box<dyn ReadOnlyMemory>` so
    // it can thread it down as `&dyn ReadOnlyMemory` through the
    // optimizer's `OptCtx`.  The `PyReadOnlyMemoryAdapter` adapter
    // refcounts the Python object itself (`Py<PyAny>`), so a single
    // boxed owner suffices for the duration of the run.
    let rom_box: Option<Box<dyn strider_orchestrator::opt::ReadOnlyMemory>> =
        rom.map(MemInput::into_box);

    // Resolve the register table once (`Sleigh::regs()` is expensive) so
    // we can build the function-default CC and every per-address override
    // against it before constructing the `Strider` (which consumes the
    // Sleigh).  Construction + CC resolution happen before `allow_threads`
    // so any typed `StriderError` becomes a `PyErr` while we still hold
    // the GIL.
    let arch_inner = arch.inner;
    let regs = orch_sleigh
        .regs()
        .map_err(|e| into_strider_err(anyhow::anyhow!("Sleigh::regs() failed: {e:?}")))?;

    // Build the function-default CC + per-address overrides against the
    // same register table (shared helpers — see `build_cc` /
    // `build_per_address_ccs`).
    let cc_built = build_cc(&cc, &regs)?;
    let per_address_ccs = build_per_address_ccs(per_address_ccs_py, &regs)?;

    let lift_opts = strider_orchestrator::LiftOptions {
        cfg: strider_cfg::CfgOptions {
            fn_max_size: function_max_size,
            allow_code_before_start_addr,
            ..strider_cfg::CfgOptions::default()
        },
        per_address_ccs,
    };
    let opt_opts = strider_orchestrator::opt::OptOptions {
        compact,
        ..strider_orchestrator::opt::OptOptions::default()
    };

    // The `Strider` owns the sleigh, the rom, and the cached register
    // table for the whole run, so the fixed-point loop runs without the
    // GIL.
    let mut strider = strider_orchestrator::Strider::new(arch_inner, orch_sleigh, rom_box)
        .map_err(into_strider_err)?;
    let function = py
        .allow_threads(|| strider.analyze(entry, &cc_built, &lift_opts, &opt_opts))
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
    let rom_box: Option<Box<dyn strider_orchestrator::opt::ReadOnlyMemory>> =
        rom.map(MemInput::into_box);
    if rom_box.is_some() {
        pipeline.prepend_load_read_only()?;
    }
    // A user-facing `Sleigh` handle for the `RunResult` (independent of
    // the `Lifter`'s owned Sleigh).
    let reader_for_sleigh = mem.clone_one()?.into_any();
    let sleigh = Py::new(py, PySleigh::new_internal(arch.clone(), reader_for_sleigh)?)?;

    // The `Lifter` owns its Sleigh (built from `mem`); it both builds the
    // CFG and lifts it.
    let lifter = PyLifter::new_internal(arch.clone(), mem, cc.clone())?;
    let lifter_obj = Py::new(py, lifter)?;

    let cfg_obj = Py::new(
        py,
        PyLifter::build_cfg_internal(
            lifter_obj.clone_ref(py),
            py,
            entry,
            allow_code_before_start_addr,
            function_max_size,
        )?,
    )?;

    // Resolve per-address CCs against the lifter's register table — the
    // same table the function-default CC was built against — mirroring
    // the orchestrator's `LoopState::new` behaviour so both pipeline
    // paths honour `per_address_ccs` identically.
    let per_address_built_ccs: rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention> =
        if per_address_ccs_py.is_empty() {
            rustc_hash::FxHashMap::default()
        } else {
            build_per_address_ccs(per_address_ccs_py, lifter_obj.borrow(py).inner.sleigh_regs())?
        };

    let lifter_borrow = lifter_obj.borrow(py);
    let cfg_borrow = cfg_obj.borrow(py);
    let outcome = lifter_borrow
        .inner
        .analyze_cfg_with(
            &cfg_borrow.inner,
            &lifter_borrow.cc,
            &strider_orchestrator::LiftOptions {
                per_address_ccs: per_address_built_ccs,
                ..strider_orchestrator::LiftOptions::default()
            },
        )
        .map_err(into_strider_err)?;
    drop(cfg_borrow);
    let function = outcome.function;
    drop(lifter_borrow);
    let py_function = Py::new(py, PyFunction::new(function, cfg_obj.clone_ref(py)))?;

    let actual_pipeline = pipeline.drain_into_pipeline()?;
    {
        let py_function_borrow = py_function.borrow(py);
        let mut function = py_function_borrow.write_inner().map_err(into_strider_err)?;
        let mut ctx = match rom_box.as_deref() {
            Some(rom) => strider_orchestrator::opt::OptCtx::with_rom(rom),
            None => strider_orchestrator::opt::OptCtx::empty(),
        };
        actual_pipeline
            .run(&mut function, &mut ctx)
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
