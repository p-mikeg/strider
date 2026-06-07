//! `PyLifter` — the low-level lift handle (Python `strider.Lifter`) —
//! plus `PyStriderRun` — the high-level run handle (Python
//! `strider.Strider`) and its `strider()` constructor.
//!
//! `PyLifter` wraps `strider_orchestrator::LiftDriver<AnyMemReader>` and
//! exposes `build_cfg` + `analyze_cfg` + `build_optimizer_pipeline` —
//! "build + lift one CFG, no indirect-branch resolution".  It is
//! constructed with a `(SleighArch, mem, CallingConvention)` triple; a
//! `Sleigh<AnyMemReader>` is built from `mem` and OWNED by the inner
//! `LiftDriver` (the lifter now owns the Sleigh).  The calling convention
//! is resolved against the lifter's register table at construction and
//! stored alongside it (it is a per-call argument to the lift methods).
//!
//! `PyStriderRun` (Python `strider.Strider`) wraps
//! `strider_orchestrator::Strider<AnyMemReader>` — the full
//! lift+optimise+indirect-resolve fixed-point loop, the same flow behind
//! `strider.run`.  It owns the Sleigh internally (so `analyze` is
//! `&mut self`, the Sleigh being stateful) and is bound to one default
//! calling convention fixed at construction.

use pyo3::prelude::*;

use crate::arch::PySleighArch;
use crate::cc::PyCallingConvention;
use crate::cfg::PyCfg;
use crate::errors::into_strider_err;
use crate::function::PyFunction;
use crate::reader::{AnyMemReader, MemInput};
use crate::run::{build_cc, build_per_address_ccs, reject_zero_max_size};

/// Build a `LiftDriver<AnyMemReader>` (owning a fresh `Sleigh` built from
/// `mem`) plus the resolved calling convention for `cc`.  Shared by the
/// `#[new]` constructor and `new_internal`.
fn build_lift_driver(
    arch: PySleighArch,
    mem: MemInput,
    cc: &PyCallingConvention,
) -> PyResult<(
    strider_orchestrator::LiftDriver<AnyMemReader>,
    strider_target::BuiltCallingConvention,
)> {
    let reader = mem.into_any();
    let sleigh = rsleigh::Sleigh::new(arch.inner.sla_spec(), arch.inner.pspec(), reader)
        .map_err(|e| into_strider_err(anyhow::anyhow!("Sleigh::new failed: {e:?}")))?;
    let driver =
        strider_orchestrator::LiftDriver::new(arch.inner, sleigh).map_err(into_strider_err)?;
    // Resolve the CC against the driver's (the lifter's) register table.
    let cc_built = build_cc(cc, driver.sleigh_regs())?;
    Ok((driver, cc_built))
}

/// Low-level lift handle bound to a `(SleighArch, mem, CallingConvention)`
/// triple.  Builds a `Cfg` via `build_cfg`, converts it into the IR graph
/// via `analyze_cfg`, and produces the canned optimizer pipelines.  No
/// indirect-branch resolution — use the high-level `Strider` (or
/// `strider.run`) for that.
///
/// `unsendable`: the inner `LiftDriver<AnyMemReader>` owns a `Sleigh`
/// whose `MemReader` may be a non-`Send` Python-callback / `MemoryMap`
/// reader.  Like every Python-thread-bound wrapper here, it is only ever
/// touched while holding the GIL.
#[pyclass(name = "Lifter", module = "strider", unsendable)]
pub struct PyLifter {
    /// The owning lift driver — owns the `Sleigh` (built from the `mem`
    /// passed at construction) and the cached register table.
    pub(crate) inner: strider_orchestrator::LiftDriver<AnyMemReader>,
    /// The function-default calling convention, resolved at construction
    /// against the driver's register table.  Threaded into every lift
    /// call (the owning `Lifter` engine does not store it).
    pub(crate) cc: strider_target::BuiltCallingConvention,
}

/// Mirror of `strider_orchestrator::LiftOutcome`.
///
/// `unresolved_branches` carries low-level lift state used by the
/// indirect-branch resolver in Rust; this binding exposes only its
/// count so Python users can detect "did we have any indirect
/// branches?" without dragging the full payload across the boundary.
#[pyclass(name = "AnalyzeOutcome", module = "strider")]
pub struct PyAnalyzeOutcome {
    /// The lifted IR graph for the analysed CFG.
    #[pyo3(get)]
    pub(crate) function: Py<PyFunction>,
    /// Number of indirect branches the analysis could not resolve.
    #[pyo3(get)]
    pub(crate) unresolved_branch_count: usize,
}

#[pymethods]
impl PyLifter {
    /// Construct a Lifter for `arch` reading from `mem`, with default
    /// calling convention `cc`.  A `Sleigh` is built from `mem` and owned
    /// by the inner lift engine; `cc` is resolved against the engine's
    /// register table.  Raises `StriderError` on Sleigh-construction or
    /// CC-resolution failure.
    #[new]
    fn new(arch: PySleighArch, mem: MemInput, cc: PyCallingConvention) -> PyResult<Self> {
        let (inner, cc) = build_lift_driver(arch, mem, &cc)?;
        Ok(Self { inner, cc })
    }

    /// Build a control-flow graph for the function at `entry` using the
    /// owned Sleigh.  The returned `Cfg` keeps a back-reference to this
    /// `Lifter` so dot rendering can resolve register names through the
    /// owned Sleigh.
    ///
    /// Borrows the owned Sleigh mutably for the build (`&mut self`).  The
    /// low-level `build_cfg` does no indirect-branch resolution: every
    /// `BranchIndirect` is left as an `UnresolvedIndirectBranch`
    /// terminator (resolution is `strider.run`'s job).
    ///
    /// Raises `ValueError` for `function_max_size == 0` and `StriderError`
    /// on a lift failure.
    #[pyo3(signature = (entry, allow_code_before_start_addr=false, function_max_size=None))]
    fn build_cfg(
        slf: Py<Self>,
        py: Python<'_>,
        entry: u64,
        allow_code_before_start_addr: bool,
        function_max_size: Option<u64>,
    ) -> PyResult<PyCfg> {
        Self::build_cfg_internal(slf, py, entry, allow_code_before_start_addr, function_max_size)
    }

    /// Lift `cfg` into the IR graph, returning an `AnalyzeOutcome`
    /// (function + unresolved-branch count).  Indirect branches are not
    /// driven to a fixed point here — use `strider.run` for that.
    fn analyze_cfg(slf: Py<Self>, py: Python<'_>, cfg: Py<PyCfg>) -> PyResult<PyAnalyzeOutcome> {
        let cfg_borrow = cfg.borrow(py);
        let outcome = {
            let lifter = slf.borrow(py);
            lifter
                .inner
                .build_ir(&cfg_borrow.inner, &lifter.cc)
                .map_err(into_strider_err)?
        };
        let unresolved_branch_count = outcome.unresolved_branches.len();
        let function = outcome.function;
        drop(cfg_borrow);
        let py_function = Py::new(py, PyFunction::new(function, cfg))?;
        Ok(PyAnalyzeOutcome {
            function: py_function,
            unresolved_branch_count,
        })
    }

    /// Mirror of `strider_orchestrator::Strider::build_optimizer_pipeline`.  Adds
    /// the convention-aware LoadForward fixed-point pass plus
    /// CallStackArgCollect / FunctionArgDetect post passes on top of the
    /// default pipeline.
    fn build_optimizer_pipeline(&self) -> crate::opt::PyOptimizerPipeline {
        crate::opt::PyOptimizerPipeline::new_full_default(&self.inner)
    }
}

impl PyLifter {
    /// Internal constructor used by `strider.run`'s custom-pipeline path.
    pub(crate) fn new_internal(
        arch: PySleighArch,
        mem: MemInput,
        cc: PyCallingConvention,
    ) -> PyResult<Self> {
        let (inner, cc) = build_lift_driver(arch, mem, &cc)?;
        Ok(Self { inner, cc })
    }

    /// Build a CFG for `entry` using `slf`'s owned Sleigh, returning a
    /// `PyCfg` that back-references `slf`.  Shared by the `build_cfg`
    /// pymethod and `strider.run`'s internal paths.
    pub(crate) fn build_cfg_internal(
        slf: Py<Self>,
        py: Python<'_>,
        entry: u64,
        allow_code_before_start_addr: bool,
        function_max_size: Option<u64>,
    ) -> PyResult<PyCfg> {
        reject_zero_max_size(function_max_size)?;
        let opts = strider_cfg::CfgOptions {
            allow_code_before_start_addr,
            fn_max_size: function_max_size,
            ..strider_cfg::CfgOptions::default()
        };
        let inner = {
            let mut lifter = slf.borrow_mut(py);
            lifter
                .inner
                .build_cfg(strider_cfg::MachineInsnAddr::from(entry), &opts)
                .map_err(into_strider_err)?
        };
        Ok(PyCfg { inner, lifter: slf })
    }
}

// ── PyStriderRun — the high-level run handle (Python `Strider`) ──────────

/// High-level run handle: lift + optimise + indirect-branch resolve a
/// function to its final IR, the same fixed-point flow behind
/// `strider.run`.  Construct via `strider.strider(arch, cc, mem,
/// rom=None)`.
///
/// Owns a `strider_orchestrator::Strider<AnyMemReader>` (which owns the
/// Sleigh — hence `analyze` is `&mut self`, the Sleigh being stateful)
/// plus the default calling convention fixed at construction.  A
/// cloneable snapshot of the memory input + arch (and the default CC) is
/// retained so each `analyze` can build a fresh snapshot `Cfg` (via a
/// throwaway `Lifter`) for register-name resolution on the returned
/// `Function`.
// `unsendable`: `PyStriderRun` retains a `MemInput`, whose `MemoryMap`
// variant holds a non-`Send` `Rc<RefCell<...>>` (the same reason
// `PyMemoryMap` is `unsendable`).  Like every Python-thread-bound
// wrapper here, it is only ever touched while holding the GIL.
#[pyclass(name = "Strider", module = "strider", unsendable)]
pub struct PyStriderRun {
    /// The orchestrator run handle (owns the Sleigh + cached regs + rom).
    inner: strider_orchestrator::Strider<AnyMemReader>,
    /// The function-default calling convention, resolved at construction
    /// against the orchestrator's register table.
    cc: strider_target::BuiltCallingConvention,
    /// Target arch (for building per-analyze snapshot CFGs).
    arch: PySleighArch,
    /// Cloneable memory input — each `analyze` mints a fresh snapshot
    /// reader from this to build the `Cfg` handed to the returned
    /// `Function` (the orchestrator owns its own reader inside `inner`).
    mem: MemInput,
}

#[pymethods]
impl PyStriderRun {
    /// Lift the function at `entry`, optimise it to a fixed point,
    /// resolve its indirect branches, and return the final IR `Function`.
    ///
    /// `cc` is fixed at construction; per-target-address overrides are
    /// supplied via `per_address_ccs` (preset or custom CCs accepted).
    ///
    /// Args:
    ///     entry: Address of the function to analyse.
    ///     function_max_size: Optional byte bound past `entry`; must be > 0.
    ///     allow_code_before_start_addr: Permit lifting before `entry`.
    ///     compact: Compact the IR arena after analysis (default `True`).
    ///     per_address_ccs: Per-target-address calling-convention overrides.
    ///
    /// Raises `ValueError` for `function_max_size == 0` and `StriderError`
    /// on lift/analysis failure.
    #[pyo3(signature = (
        entry,
        *,
        function_max_size = None,
        allow_code_before_start_addr = false,
        compact = true,
        per_address_ccs = None,
    ))]
    fn analyze(
        &mut self,
        py: Python<'_>,
        entry: u64,
        function_max_size: Option<u64>,
        allow_code_before_start_addr: bool,
        compact: bool,
        per_address_ccs: Option<std::collections::HashMap<u64, PyCallingConvention>>,
    ) -> PyResult<(Py<PyFunction>, Vec<u64>)> {
        reject_zero_max_size(function_max_size)?;
        let per_address_ccs_py = per_address_ccs.unwrap_or_default();

        // Build the per-address overrides against the orchestrator's
        // cached register table before constructing the options.
        let per_address_built =
            build_per_address_ccs(per_address_ccs_py, self.inner.sleigh_regs())?;

        let lift_opts = strider_orchestrator::LiftOptions {
            cfg: strider_cfg::CfgOptions {
                fn_max_size: function_max_size,
                allow_code_before_start_addr,
                ..strider_cfg::CfgOptions::default()
            },
            per_address_ccs: per_address_built,
            compact,
        };
        let opt_opts = strider_orchestrator::opt::OptOptions::default();

        // Run the fixed-point loop without the GIL (the orchestrator owns
        // the Sleigh + rom + cached regs for the whole run).
        let cc = self.cc.clone();
        let result = py
            .allow_threads(|| self.inner.analyze(entry, &cc, &lift_opts, &opt_opts, None))
            .map_err(into_strider_err)?;
        let function = result.function;
        // Surface the unresolved indirect-branch sites as machine addresses
        // so the Python caller can assert full resolution.
        let unresolved: Vec<u64> = result
            .unresolved_indirect_branches
            .iter()
            .map(|addr| addr.machine_addr.addr)
            .collect();

        // Surface any control-flow exception (KeyboardInterrupt /
        // SystemExit) a Python callback stashed during the GIL-released
        // loop (mirrors `strider.run`).
        if let Some(err) = crate::pattern::take_pending_control_flow() {
            return Err(err);
        }

        // Build a snapshot `Cfg` (via a throwaway `Lifter` that owns a
        // fresh Sleigh built from a cloned reader) so the returned
        // `Function` can resolve register names for dot rendering — the
        // orchestrator owns its own reader inside `inner` and doesn't
        // hand one back.  The CC choice doesn't affect the snapshot CFG;
        // reuse the default CC.
        let snapshot_mem = self.mem.clone_one()?;
        let snapshot_cc = PyCallingConvention {
            inner: crate::cc::CcImpl::Custom(Box::new(self.cc.clone())),
            preset_name: "custom",
        };
        let snapshot_lifter = Py::new(
            py,
            PyLifter::new_internal(self.arch.clone(), snapshot_mem, snapshot_cc)?,
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

        let py_function = Py::new(py, PyFunction::new(function, cfg_obj))?;
        Ok((py_function, unresolved))
    }
}

/// Construct a high-level `Strider` run handle over `mem` for `arch` +
/// `cc`, with optional read-only `rom` for constant-load folding.
///
/// `cc` becomes the function-default calling convention (resolved
/// against the target's register table at construction); per-target-
/// address overrides are supplied per `analyze` call.  The returned
/// handle drives the full lift+optimise+indirect-resolve fixed-point loop
/// — the same flow as `strider.run`, but configured once and reusable for
/// many `analyze(entry, ...)` calls.
///
/// Args:
///     arch: Target `SleighArch`.
///     cc: Default `CallingConvention` for analysed functions.
///     mem: A `MemoryMap` or a `MemReader` subclass.
///     rom: Optional read-only memory for `LoadReadOnly` constant folding.
///
/// Raises `StriderError` on Sleigh construction / CC-resolution failure.
#[pyfunction]
#[pyo3(signature = (arch, cc, mem, rom = None))]
pub fn strider(
    arch: PySleighArch,
    cc: PyCallingConvention,
    mem: MemInput,
    rom: Option<MemInput>,
) -> PyResult<PyStriderRun> {
    // Snapshot the reader: one fresh AnyMemReader for the orchestrator
    // (consumed by its Sleigh) and the original `mem` retained for
    // per-analyze snapshot CFGs.
    let reader_for_orch = mem.clone_one()?.into_any();

    let orch_sleigh =
        rsleigh::Sleigh::new(arch.inner.sla_spec(), arch.inner.pspec(), reader_for_orch)
            .map_err(|e| into_strider_err(anyhow::anyhow!("Sleigh::new failed: {e:?}")))?;

    let rom_box: Option<Box<dyn strider_orchestrator::opt::ReadOnlyMemory>> =
        rom.map(MemInput::into_box);

    // Resolve the default CC against the orchestrator's register table
    // before constructing the handle (which consumes the Sleigh).
    let regs = orch_sleigh
        .regs()
        .map_err(|e| into_strider_err(anyhow::anyhow!("Sleigh::regs() failed: {e:?}")))?;
    let cc_built = build_cc(&cc, &regs)?;

    let inner = strider_orchestrator::Strider::new(arch.inner, orch_sleigh, rom_box)
        .map_err(into_strider_err)?;

    Ok(PyStriderRun {
        inner,
        cc: cc_built,
        arch,
        mem,
    })
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyLifter>()?;
    m.add_class::<PyAnalyzeOutcome>()?;
    m.add_class::<PyStriderRun>()?;
    m.add_function(wrap_pyfunction!(strider, m)?)?;
    Ok(())
}
