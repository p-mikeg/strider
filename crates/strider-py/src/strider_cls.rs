//! `PyLifter` (Python `strider.Lifter`) — the single lift+optimise+
//! resolve handle, constructed via `strider.lifter(arch, mem, rom=None)`.
//!
//! Wraps `strider_orchestrator::Strider<AnyMemReader>` (which owns the
//! Sleigh + cached register table + optional ROM) and exposes:
//! * `build_cfg(entry, ...)` — structural-only: build a `Cfg`, no lift/
//!   optimisation/indirect-branch resolution.
//! * `analyze(entry, cc, ...)` — the full lift+optimise+resolve
//!   fixed-point loop, returning `(Function, unresolved_addrs)`.
//!
//! `cc` is a per-`analyze`-call argument, not handle state: the handle
//! owns no default calling convention.  This collapses the four former
//! entry points (`strider.strider`/`Strider`, `strider.run`, and the old
//! low-level `Lifter`/`AnalyzeOutcome`) into one.

use pyo3::prelude::*;
use strider_orchestrator::opt::AliasMode;

use crate::arch::PySleighArch;
use crate::cc::PyCallingConvention;
use crate::cfg::PyCfg;
use crate::errors::into_strider_err;
use crate::function::PyFunction;
use crate::reader::{AnyMemReader, MemInput};

/// Resolve a `PyCallingConvention` against an already-fetched register
/// table into a `BuiltCallingConvention` (preset → resolve; custom →
/// already-resolved clone).
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
/// are already resolved at construction).
pub(crate) fn build_per_address_ccs(
    per_address_ccs_py: std::collections::HashMap<u64, PyCallingConvention>,
    regs: &rsleigh::SleighRegs,
) -> PyResult<rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>> {
    per_address_ccs_py
        .into_iter()
        .map(|(addr, py_cc)| {
            let built = match py_cc.inner {
                crate::cc::CcImpl::Preset(preset) => preset.build(regs).map_err(|e| {
                    into_strider_err(anyhow::anyhow!(
                        "per-address CC at {addr:#x} unresolved: {e:?}"
                    ))
                })?,
                crate::cc::CcImpl::Custom(built) => *built,
            };
            Ok((addr, built))
        })
        .collect::<PyResult<_>>()
}

/// Reject `function_max_size=0` at the Python boundary with a typed
/// `ValueError` (zero is meaningless — the Rust builder would silently
/// coerce it to unbounded).
pub(crate) fn reject_zero_max_size(function_max_size: Option<u64>) -> PyResult<()> {
    if matches!(function_max_size, Some(0)) {
        return Err(pyo3::exceptions::PyValueError::new_err(
            "function_max_size must be > 0 (zero is meaningless — omit the argument for unbounded)",
        ));
    }
    Ok(())
}

/// Parse the `alias_mode` kwarg string into the optimizer's [`AliasMode`]
/// precision knob.  `"stack_global_disjoint"` (the default) trusts that
/// stack and global/constant memory never overlap; `"strict"` is the
/// always-sound floor.  Any other value is a typed `ValueError`.
pub(crate) fn parse_alias_mode(s: &str) -> PyResult<strider_orchestrator::opt::AliasMode> {
    match s {
        "stack_global_disjoint" => Ok(AliasMode::StackGlobalDisjoint),
        "strict" => Ok(AliasMode::Strict),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "alias_mode must be \"stack_global_disjoint\" or \"strict\", got {other:?}"
        ))),
    }
}

/// Build an orchestrator-owned `Sleigh` from `arch` over `reader` with the
/// shared user-visible error message.
pub(crate) fn build_orch_sleigh(
    arch: &PySleighArch,
    reader: AnyMemReader,
) -> PyResult<rsleigh::Sleigh<AnyMemReader>> {
    rsleigh::Sleigh::new(arch.inner.sla_spec(), arch.inner.pspec(), reader)
        .map_err(|e| into_strider_err(anyhow::anyhow!("Sleigh::new failed: {e:?}")))
}

/// Build the orchestrator `Strider` handle over `mem` for `arch`, with
/// optional read-only `rom`.  Shared by `PyLifter::new` (the `#[new]`
/// constructor / the `lifter()` free function) and `PyLifter::rebuild`
/// (the `ElfLifter.add_elf` reconstruction seam), so both paths build
/// the handle identically.
pub(crate) fn build_strider(
    arch: PySleighArch,
    mem: MemInput,
    rom: Option<MemInput>,
) -> PyResult<strider_orchestrator::Strider<AnyMemReader>> {
    let reader = mem.into_any();
    let sleigh = build_orch_sleigh(&arch, reader)?;
    let rom_box: Option<Box<dyn strider_orchestrator::opt::ReadOnlyMemory>> =
        rom.map(MemInput::into_box);
    strider_orchestrator::Strider::new(arch.inner, sleigh, rom_box).map_err(into_strider_err)
}

/// Build the orchestrator `LiftOptions` from the CFG-shaping knobs plus
/// the resolved per-address CCs and `compact` flag.
pub(crate) fn orch_lift_opts(
    function_max_size: Option<u64>,
    allow_code_before_start_addr: bool,
    per_address_ccs: rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>,
    compact: bool,
) -> strider_orchestrator::LiftOptions {
    strider_orchestrator::LiftOptions {
        cfg: strider_cfg::CfgOptions {
            fn_max_size: function_max_size,
            allow_code_before_start_addr,
            ..strider_cfg::CfgOptions::default()
        },
        per_address_ccs,
        compact,
    }
}

/// Map the orchestrator's unresolved indirect-branch sites onto their
/// machine addresses for the Python-facing
/// `unresolved_indirect_branches` list.
pub(crate) fn unresolved_machine_addrs(branches: &[strider_cfg::PcodeInsnAddr]) -> Vec<u64> {
    branches.iter().map(|addr| addr.machine_addr.addr).collect()
}

/// Drain the thread-local pending control-flow cell: if a Python callback
/// (e.g. a custom `ReadOnlyMemory.read`) stashed a `KeyboardInterrupt` /
/// `SystemExit` while the GIL was released, surface it here so PyO3
/// propagates it to the Python caller.
pub(crate) fn check_pending_control_flow() -> PyResult<()> {
    if let Some(err) = crate::pattern::take_pending_control_flow() {
        return Err(err);
    }
    Ok(())
}

/// Wrap a fallible operation that may have called into Python (CFG build,
/// lift, analyze): when it fails, prefer a stashed control-flow exception
/// (`KeyboardInterrupt` / `SystemExit`) over the operation's own error so
/// a Ctrl-C inside e.g. a `MemReader.read` during instruction fetch is
/// surfaced as the interrupt rather than being downgraded to the
/// `StriderError` the read failure produced.  On success the pending cell
/// is left untouched (drained by the later `check_pending_control_flow`).
pub(crate) fn prefer_pending_control_flow<T>(result: PyResult<T>) -> PyResult<T> {
    match result {
        Ok(v) => Ok(v),
        Err(e) => {
            check_pending_control_flow()?;
            Err(e)
        }
    }
}

/// The single lift+optimise+resolve handle.  Construct via
/// `strider.lifter(arch, mem, rom=None)`; call `build_cfg` for a
/// structural-only CFG, or `analyze(entry, cc, ...)` for the full
/// fixed-point lift.
///
/// `unsendable`: the inner `Strider<AnyMemReader>` owns a `Sleigh` whose
/// `MemReader` may be a non-`Send` Python-callback / `BufferReader`
/// reader.  Like every Python-thread-bound wrapper here, it is only ever
/// touched while holding the GIL.
///
/// `subclass`: lets the Python high-level facade define `ElfLifter` as a
/// PURE-PYTHON subclass (`class ElfLifter(Lifter): ...` in `_api.py`) —
/// `ElfLifter` adds the ELF symbol backend + a name-aware `analyze`
/// override entirely in Python, reusing this Rust struct's `#[new]`
/// constructor via `super().__new__(cls, arch, mem, rom=rom)` (the
/// standard PyO3 "extra Python state on a Rust base" recipe).
#[pyclass(name = "Lifter", module = "strider", unsendable, subclass)]
pub struct PyLifter {
    /// The orchestrator handle: owns the Sleigh, the cached register
    /// table, and the optional rom.  Both `build_cfg` and `analyze` are
    /// driven off the same instance (and thus the same Sleigh), so a
    /// snapshot `Cfg` built after `analyze` reuses the already-warm
    /// per-address decode cache rather than decoding through a second
    /// Sleigh.
    inner: strider_orchestrator::Strider<AnyMemReader>,
}

impl PyLifter {
    /// The owned `Sleigh`, for register-name resolution (dot rendering).
    /// Shared by `PyCfg`/`PyFunction`'s dot-dumper paths, which hold a
    /// back-reference to this handle rather than the Sleigh directly.
    pub(crate) fn sleigh(&self) -> &rsleigh::Sleigh<AnyMemReader> {
        self.inner.sleigh()
    }
}

#[pymethods]
impl PyLifter {
    /// Construct the single lift+optimise+resolve handle over `mem` for
    /// `arch`, with optional read-only `rom` for constant-load folding.
    /// Equivalent to (and shares its implementation with) the
    /// `strider.lifter(arch, mem, rom=None)` free function; both exist so
    /// Python code can spell either `strider.lifter(...)` or
    /// `strider.Lifter(...)`, and so a Python subclass (`ElfLifter`) can
    /// build the base via `super().__new__(cls, arch, mem, rom=rom)`.
    #[new]
    #[pyo3(signature = (arch, mem, rom = None))]
    fn new(arch: PySleighArch, mem: MemInput, rom: Option<MemInput>) -> PyResult<Self> {
        Ok(PyLifter {
            inner: build_strider(arch, mem, rom)?,
        })
    }

    /// Replace this handle's inner orchestrator state in place — the
    /// `ElfLifter.add_elf` (Python subclass) reconstruction seam.
    ///
    /// The existing Sleigh was built from a point-in-time snapshot of
    /// `mem`/`rom` (see `PyBufferReaderView`), so it does NOT observe
    /// later region growth; `ElfLifter.add_elf` extends the ELF's
    /// backing regions then calls this to rebuild the Sleigh/Strider
    /// from the merged map so a subsequent `analyze` sees the newly
    /// merged ELF.  Leading underscore marks it as an internal seam for
    /// the Python high-level facade, not a general-purpose API.
    ///
    /// INTERNAL API, not a public surface: this is a `#[pymethods]`
    /// entry on `PyLifter` itself, so — because `PyLifter` is
    /// `#[pyclass(subclass)]` and `ElfLifter` is a *pure-Python*
    /// subclass (route (a): a Python subclass over a Rust
    /// `#[pyclass(subclass)]` base, which requires any method the
    /// subclass calls on itself to be Python-callable) — it is
    /// unavoidably callable on *any* `strider.lifter(...)` handle, not
    /// just on an `ElfLifter`.  It exists solely so
    /// `ElfLifter.add_elf` (`strider/_api.py`) can rebuild this
    /// handle's inner Sleigh/Strider state in place after merging in a
    /// new ELF's regions.  This is accepted as intentional
    /// shared-internal surface given route (a) — it is NOT a fixable
    /// leak without changing the subclassing strategy.  The leading
    /// underscore is the only enforcement: general `Lifter` handles
    /// (constructed via `strider.lifter(...)` / `strider.Lifter(...)`)
    /// must NOT call `_rebuild` themselves.  Deliberately left out of
    /// `strider/__init__.pyi` (underscore-private → no stub entry).
    #[pyo3(name = "_rebuild", signature = (arch, mem, rom = None))]
    fn rebuild(&mut self, arch: PySleighArch, mem: MemInput, rom: Option<MemInput>) -> PyResult<()> {
        self.inner = build_strider(arch, mem, rom)?;
        Ok(())
    }

    /// Build a control-flow graph for the function at `entry` — no lift,
    /// no optimisation, no indirect-branch resolution.  Every
    /// `BranchIndirect` is left as an `UnresolvedIndirectBranch`
    /// terminator (resolution is `analyze`'s job).  The returned `Cfg`
    /// keeps a back-reference to this `Lifter` so dot rendering can
    /// resolve register names through the owned Sleigh.
    ///
    /// Raises `ValueError` for `function_max_size == 0` and
    /// `StriderError` on a build failure.
    #[pyo3(signature = (entry, allow_code_before_start_addr=false, function_max_size=None))]
    fn build_cfg(
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
                .build_cfg(entry, &opts)
                .map_err(into_strider_err)?
        };
        Ok(PyCfg { inner, lifter: slf })
    }

    /// Lift the function at `entry`, optimise it to a fixed point,
    /// resolve its indirect branches, and return `(function,
    /// unresolved_addrs)`.
    ///
    /// `cc` is the function-default calling convention for THIS call
    /// (the handle stores no default); per-target-address overrides are
    /// supplied via `per_address_ccs` (preset or custom CCs accepted).
    ///
    /// Args:
    ///     entry: Address of the function to analyse.
    ///     cc: Calling convention for this analysis.
    ///     function_max_size: Optional byte bound past `entry`; must be > 0.
    ///     allow_code_before_start_addr: Permit lifting before `entry`.
    ///     compact: Compact the IR arena after analysis (default `True`).
    ///     per_address_ccs: Per-target-address calling-convention overrides.
    ///     calls_clobber: Treat a call on a stack-arg load's memory chain as
    ///         shadowing the slot (default `False`).
    ///     assume_distinct_sp_bases_disjoint: Assume a store rooted at a
    ///         different SP base than the entry SP is disjoint from the
    ///         incoming-arg slots (default `False`).
    ///     alias_mode: SP-aware alias precision for every memory pass —
    ///         `"stack_global_disjoint"` (default) trusts that stack and
    ///         global/constant memory never overlap; `"strict"` is the
    ///         always-sound floor.
    ///
    /// Raises `ValueError` for `function_max_size == 0` or an unrecognised
    /// `alias_mode`, and `StriderError` on lift/analysis failure.
    #[pyo3(signature = (
        entry,
        cc,
        *,
        function_max_size = None,
        allow_code_before_start_addr = false,
        compact = true,
        per_address_ccs = None,
        calls_clobber = false,
        assume_distinct_sp_bases_disjoint = false,
        alias_mode = "stack_global_disjoint",
    ))]
    #[allow(clippy::too_many_arguments)]
    fn analyze(
        slf: Py<Self>,
        py: Python<'_>,
        entry: u64,
        cc: PyCallingConvention,
        function_max_size: Option<u64>,
        allow_code_before_start_addr: bool,
        compact: bool,
        per_address_ccs: Option<std::collections::HashMap<u64, PyCallingConvention>>,
        calls_clobber: bool,
        assume_distinct_sp_bases_disjoint: bool,
        alias_mode: &str,
    ) -> PyResult<(Py<PyFunction>, Vec<u64>)> {
        reject_zero_max_size(function_max_size)?;
        let alias_mode = parse_alias_mode(alias_mode)?;
        let per_address_ccs_py = per_address_ccs.unwrap_or_default();

        // Resolve the CC + per-address overrides against the handle's
        // cached register table before constructing the options.
        let (cc_built, per_address_built) = {
            let lifter = slf.borrow(py);
            let regs = lifter.inner.sleigh_regs();
            let cc_built = build_cc(&cc, regs)?;
            let per_address_built = build_per_address_ccs(per_address_ccs_py, regs)?;
            (cc_built, per_address_built)
        };

        let lift_opts = orch_lift_opts(
            function_max_size,
            allow_code_before_start_addr,
            per_address_built,
            compact,
        );
        let opt_opts = strider_orchestrator::opt::OptOptions {
            alias_mode,
            arg_alias: strider_orchestrator::opt::MemAliasOptions {
                calls_clobber,
                assume_distinct_sp_bases_disjoint,
            },
        };

        // Run the fixed-point loop without the GIL (the handle owns the
        // Sleigh + rom + cached regs for the whole run).
        let result = {
            let mut lifter = slf.borrow_mut(py);
            // Reborrow the inner `Strider` as a plain reference BEFORE the
            // closure: capturing `lifter.inner` directly (rather than
            // `lifter` itself) keeps the closure's captured type a plain
            // `&mut Strider<AnyMemReader>` instead of the GIL-bound
            // `PyRefMut`, which embeds a `!Send` `Python<'_>` marker that
            // would otherwise fail `allow_threads`'s `Ungil` bound.
            let inner = &mut lifter.inner;
            prefer_pending_control_flow(
                py.allow_threads(|| inner.analyze(entry, &cc_built, &lift_opts, &opt_opts, None))
                    .map_err(into_strider_err),
            )?
        };
        let function = result.function;
        // Surface the unresolved indirect-branch sites as machine addresses
        // so the Python caller can assert full resolution.
        let unresolved = unresolved_machine_addrs(&result.unresolved_indirect_branches);

        // Surface any control-flow exception (KeyboardInterrupt /
        // SystemExit) a Python callback stashed during the GIL-released
        // loop.
        check_pending_control_flow()?;

        // Build a snapshot `Cfg` off the SAME handle (same Sleigh, warm
        // decode cache) so the returned `Function` can resolve register
        // names for dot rendering.
        let cfg_obj = {
            let opts = strider_cfg::CfgOptions {
                allow_code_before_start_addr,
                fn_max_size: function_max_size,
                ..strider_cfg::CfgOptions::default()
            };
            let inner = {
                let mut lifter = slf.borrow_mut(py);
                lifter
                    .inner
                    .build_cfg(entry, &opts)
                    .map_err(into_strider_err)?
            };
            Py::new(
                py,
                PyCfg {
                    inner,
                    lifter: slf.clone_ref(py),
                },
            )?
        };

        let py_function = Py::new(py, PyFunction::new(function, cfg_obj))?;
        Ok((py_function, unresolved))
    }
}

/// Construct the single lift+optimise+resolve handle over `mem` for
/// `arch`, with optional read-only `rom` for constant-load folding.
///
/// `mem` backs instruction fetch (a `BufferReader`/`MemReader` subclass);
/// `rom` (if given) is folded by the `LoadReadOnly` optimizer pass.  The
/// calling convention is NOT fixed here — it is a required argument of
/// every `analyze` call, so one handle can analyse functions under
/// different conventions (e.g. per-target-address overrides via
/// `per_address_ccs`, or simply different `cc`s across calls).
///
/// For an ELF, prefer `strider.load_elf(path)` → `ElfStrider`, which
/// wires `mem`/`rom` from the loaded sections and adds symbol lookups.
///
/// Raises `StriderError` on Sleigh-construction failure.
#[pyfunction]
#[pyo3(name = "lifter", signature = (arch, mem, rom = None))]
pub fn lifter(arch: PySleighArch, mem: MemInput, rom: Option<MemInput>) -> PyResult<PyLifter> {
    Ok(PyLifter {
        inner: build_strider(arch, mem, rom)?,
    })
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyLifter>()?;
    m.add_function(wrap_pyfunction!(lifter, m)?)?;
    Ok(())
}
