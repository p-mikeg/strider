use pyo3::prelude::*;

use crate::arch::PySleighArch;
use crate::cc::PyCallingConvention;
use crate::cfg::PyCfg;
use crate::errors::into_strider_err;
use crate::function::PyFunction;
use crate::options::{PyCfgOptions, PyLifterOptions};
use crate::reader::{AnyMemReader, MemInput};

fn cleared() -> PyErr {
    into_strider_err(anyhow::anyhow!(
        "this Lifter was cleared by the garbage collector and holds no engine"
    ))
}

/// A `custom(...)` convention or user-op ABI froze varnodes resolved against
/// one arch's register table; the same name denotes a different varnode under
/// another sla (x86-64 `EDI` is `%[0x38]:4`, x86 `EDI` is `%[0x1c]:4`), so
/// consuming it there silently mismatches every register. Two presets with
/// one register table (`arm` / `arm_thumb` / `arm_be_kernel`) resolve alike.
/// The preset compare comes first: the table compare walks every register.
fn check_source_arch(
    source_arch: Option<strider_target::SleighArch>,
    target: strider_target::SleighArch,
    what: std::fmt::Arguments<'_>,
) -> PyResult<()> {
    match source_arch {
        Some(source)
            if source.preset() != target.preset()
                && source.names().regs() != target.names().regs() =>
        {
            let (source, target) = (source.preset().name(), target.preset().name());
            Err(into_strider_err(anyhow::anyhow!(
                "{what} was built against a {source:?} Sleigh but is being used \
                 with a {target:?} Lifter, whose register table is a different \
                 one; its register varnodes are frozen at construction, so \
                 rebuild it from a {target:?} Sleigh"
            )))
        }
        _ => Ok(()),
    }
}

pub(crate) fn build_cc(
    cc: &PyCallingConvention,
    regs: &rsleigh::SleighRegs,
    target: strider_target::SleighArch,
) -> PyResult<strider_target::BuiltCallingConvention> {
    check_source_arch(
        cc.source_arch,
        target,
        format_args!("this custom CallingConvention"),
    )?;
    match &cc.inner {
        crate::cc::CcImpl::Preset(preset) => preset.build(regs).map_err(into_strider_err),
        crate::cc::CcImpl::Custom(built) => Ok(*built.clone()),
    }
}

/// Borrowed: the table is read once per analyse and consuming it would cost a
/// copy of every `CallingConvention` in it.
pub(crate) fn build_per_address_ccs(
    per_address_ccs_py: &std::collections::HashMap<u64, PyCallingConvention>,
    regs: &rsleigh::SleighRegs,
    target: strider_target::SleighArch,
) -> PyResult<rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>> {
    per_address_ccs_py
        .iter()
        .map(|(&addr, py_cc)| {
            check_source_arch(
                py_cc.source_arch,
                target,
                format_args!("the custom per-address CallingConvention at {addr:#x}"),
            )?;
            let built = match &py_cc.inner {
                crate::cc::CcImpl::Preset(preset) => preset.build(regs).map_err(|e| {
                    into_strider_err(anyhow::anyhow!(
                        "per-address CC at {addr:#x} unresolved: {e:?}"
                    ))
                })?,
                crate::cc::CcImpl::Custom(built) => *built.clone(),
            };
            Ok((addr, built))
        })
        .collect::<PyResult<_>>()
}

/// Borrow the handle, raising the re-entrant `StriderError` when the borrow
/// conflicts with a call in flight on it, from this thread or another:
/// `analyze`, `build_cfg` and `add_elf` borrow exclusively, the rest shared.
///
/// Every method borrows through this: a `&self` receiver's failed borrow is
/// PyO3's bare `RuntimeError`, and `Py::borrow` panics, which surfaces as
/// `PanicException`, a `BaseException` no `except Exception` catches.
fn try_borrow_lifter<'py>(slf: &Bound<'py, PyLifter>) -> PyResult<pyo3::PyRef<'py, PyLifter>> {
    slf.try_borrow().map_err(|_| reentrant_lifter_err())
}

fn try_borrow_lifter_mut<'py>(
    slf: &Bound<'py, PyLifter>,
) -> PyResult<pyo3::PyRefMut<'py, PyLifter>> {
    slf.try_borrow_mut().map_err(|_| reentrant_lifter_err())
}

fn reentrant_lifter_err() -> PyErr {
    into_strider_err(anyhow::anyhow!(
        "this Lifter is already in use by another call (analyze, build_cfg and \
         add_elf exclude every other call, from any thread or a `read()` \
         callback); build a separate Lifter for concurrent work"
    ))
}

/// `entry` as an address, the same way for every entry point that takes one:
/// `build_cfg` answered a bare `OverflowError` where this names the cause.
///
/// An int that does not fit is a different mistake from a str, and saying "you
/// need an ElfLifter" to someone holding one sends them the wrong way.
fn entry_addr(entry: &Bound<'_, PyAny>) -> PyResult<u64> {
    entry.extract().map_err(|_| {
        if entry.is_instance_of::<pyo3::types::PyInt>() {
            into_strider_err(anyhow::anyhow!(
                "`entry` is out of range for an address: it must fit an \
                 unsigned 64-bit integer"
            ))
        } else {
            into_strider_err(anyhow::anyhow!(
                "`entry` must be an address (int); a symbol name (str) is \
                 accepted by ElfLifter.analyze only. Build one with \
                 strider.lift.load_elf(path)"
            ))
        }
    })
}

/// `function_max_size` and `allow_code_before_start_addr` as one bound: a size
/// fixes the extent, so the flag cannot join it.
pub(crate) fn function_bound(
    function_max_size: Option<u64>,
    allow_code_before_start_addr: bool,
) -> PyResult<strider_cfg::FunctionBound> {
    let Some(size) = function_max_size else {
        return Ok(strider_cfg::FunctionBound::Open {
            code_before_start: allow_code_before_start_addr,
        });
    };
    let size = std::num::NonZeroU64::new(size).ok_or_else(|| {
        pyo3::exceptions::PyValueError::new_err(
            "function_max_size must be > 0; omit the argument for an unbounded lift",
        )
    })?;
    if allow_code_before_start_addr {
        return Err(pyo3::exceptions::PyValueError::new_err(
            "function_max_size fixes the function's extent; drop \
             allow_code_before_start_addr=True, which cannot reach below it",
        ));
    }
    Ok(strider_cfg::FunctionBound::Size(size))
}

/// The claims an `optimize` adds to `function`'s: none without `given`, and
/// otherwise `given`, which must hold every claim already in force and may not
/// add `msr_register_keeps_mode` or `fp_rounding_mode_is_nearest`, which only the
/// lift reads.
fn claims_to_add(
    function: &PyFunction,
    given: Option<&crate::options::PyAssumptionOptions>,
) -> PyResult<strider_orchestrator::opt::AssumptionOptions> {
    let Some(given) = given else {
        return Ok(strider_orchestrator::opt::AssumptionOptions::none());
    };
    let given = given.inner.clone();
    let (omitted, lift_claims): (Vec<String>, Vec<&str>) = {
        let read = function.read_inner().map_err(into_strider_err)?;
        let in_force = read.assumptions();
        let lift_claims = [
            (
                "msr_register_keeps_mode",
                given.msr_register_keeps_mode && !in_force.msr_register_keeps_mode,
            ),
            (
                "fp_rounding_mode_is_nearest",
                given.fp_rounding_mode_is_nearest && !in_force.fp_rounding_mode_is_nearest,
            ),
        ]
        .into_iter()
        .filter_map(|(name, adds)| adds.then_some(name))
        .collect();
        let omitted = in_force
            .omitted_by(&given)
            .into_iter()
            .map(|name| {
                if name != "noalias_allocators" {
                    return name.to_owned();
                }
                let mut missing: Vec<u64> = in_force
                    .noalias_allocators
                    .difference(&given.noalias_allocators)
                    .copied()
                    .collect();
                missing.sort_unstable();
                let missing: Vec<String> = missing.iter().map(|a| format!("{a:#x}")).collect();
                format!("{name} ({})", missing.join(", "))
            })
            .collect();
        (omitted, lift_claims)
    };
    if !omitted.is_empty() {
        return Err(into_strider_err(anyhow::anyhow!(
            "optimize: `assumptions` leaves out claims in force on this \
             Function: {}. Pass `function.assumptions | <your set>` to add \
             claims, or analyse again to drop one",
            omitted.join(", ")
        )));
    }
    if !lift_claims.is_empty() {
        let (verb, them) = if lift_claims.len() == 1 {
            ("is", "it")
        } else {
            ("are", "them")
        };
        return Err(into_strider_err(anyhow::anyhow!(
            "optimize: {} {verb} read by the lift; analyse again with {them}",
            lift_claims.join(" and ")
        )));
    }
    Ok(given)
}

/// `Function.optimize`: `pipeline` (the default when `None`) over `function`
/// in place, against `lifter`'s rom, after adding the claims `assumptions`
/// holds (see [`claims_to_add`]).
pub(crate) fn optimize_function(
    lifter: &Bound<'_, PyLifter>,
    function: &PyFunction,
    pipeline: Option<&crate::opt::PyOptimizerPipeline>,
    assumptions: Option<&crate::options::PyAssumptionOptions>,
) -> PyResult<()> {
    let py = lifter.py();
    let this = try_borrow_lifter(lifter)?;
    // `LoadReadOnly` reads the rom, and a mapping shortened under us takes
    // a SIGBUS there, which no Python `except` can catch.
    crate::reader::check_mem_unchanged(py, &this.mem_obj)?;
    crate::reader::check_mem_unchanged(py, &this.rom_obj)?;
    let claims = claims_to_add(function, assumptions)?;
    let rom = this.strider()?.rom();
    let pipeline = match pipeline {
        Some(p) => p.build_pipeline(),
        None => strider_orchestrator::opt::default_pipeline(),
    };
    with_pending_control_flow(|| function.run_pipeline_in_place(pipeline, "optimize", rom, &claims))
}

pub(crate) fn build_strider(
    arch: PySleighArch,
    mem: MemInput,
    rom: Option<MemInput>,
) -> PyResult<strider_orchestrator::Strider<AnyMemReader>> {
    let rom_box: Option<Box<dyn strider_orchestrator::opt::ReadOnlyMemory>> =
        rom.map(MemInput::into_box);
    strider_orchestrator::Strider::new(arch.inner, mem.into_any(), rom_box)
        .map_err(into_strider_err)
}

/// Python dict keys are unique, so the duplicate-name rejection cannot fire
/// from this surface; it is surfaced rather than unwrapped so a future caller
/// that is not a dict gets the message instead of a panic.
pub(crate) fn overrides_from(
    call_other_abis: &std::collections::HashMap<String, crate::call_other_abi::PyCallOtherAbi>,
    target: strider_target::SleighArch,
) -> PyResult<strider_target::call_other_abi::CallOtherOverrides> {
    for (name, abi) in call_other_abis {
        check_source_arch(
            abi.source_arch,
            target,
            format_args!("the custom CallOtherAbi for {name:?}"),
        )?;
    }
    strider_target::call_other_abi::CallOtherOverrides::new(
        call_other_abis
            .iter()
            .map(|(name, abi)| (name.clone(), abi.to_override()))
            .collect(),
    )
    .map_err(pyo3::exceptions::PyValueError::new_err)
}

/// Seat caller-supplied indirect-branch answers at the CFG level, so they apply
/// whether or not the classifier runs.
///
/// One target is the caller's own dispatch, not a table read: a `Single`,
/// which out of range is a tail call where a table's arm defers the site.
pub(crate) fn seat_known_targets(
    known_targets: &std::collections::HashMap<u64, crate::options::KnownTarget>,
) -> crate::options::SeatedTargets {
    known_targets
        .iter()
        .map(|(&addr, answer)| {
            let seated = match answer {
                crate::options::KnownTarget::Return => strider_cfg::ResolvedTargets::LinkRegister,
                crate::options::KnownTarget::Targets(targets) => {
                    let table = strider_cfg::ResolvedTargets::table(
                        targets
                            .iter()
                            .map(|&t| strider_cfg::ResolvedTarget::new(t, None)),
                    );
                    let only = match table.arms() {
                        [only] => Some(*only),
                        _ => None,
                    };
                    only.map_or(table, strider_cfg::ResolvedTargets::Single)
                }
            };
            (strider_cfg::PcodeInsnAddr::at_machine_start(addr), seated)
        })
        .collect()
}

/// Every `CfgOptions` field, spelled out. No `..default()` here: a field added
/// later has to be handled rather than silently dropped, which is how
/// `build_cfg` came to ignore `known_targets` entirely.
/// `call_other_overrides` is `opts`' own, from
/// [`PyCfgOptions::call_other_overrides`].
pub(crate) fn cfg_options_from<'a>(
    opts: &'a PyCfgOptions,
    call_other_overrides: &'a strider_target::call_other_abi::CallOtherOverrides,
) -> strider_cfg::CfgOptions<'a> {
    strider_cfg::CfgOptions {
        bound: opts.bound,
        known_targets: &opts.seated,
        // Only the resolve loop derives answers of its own.
        derived_targets: &strider_cfg::NO_TARGETS,
        no_return_targets: &opts.no_return_targets.set,
        call_other_overrides,
        data_ranges: opts.data_ranges.clone(),
    }
}

/// `addr` is not a machine-instruction boundary on the linear path from
/// `entry`, which a sweep only discovers by stepping over it.
fn stepped_past(entry: u64, addr: u64) -> PyErr {
    into_strider_err(anyhow::anyhow!(
        "pcode_at: linear sweep from entry {entry:#x} stepped past target {addr:#x} \
         (misaligned: {addr:#x} is not a machine-instruction boundary on the linear \
         path from entry)"
    ))
}

/// Sorted and deduped: two p-code sites in one machine instruction are one
/// address.
pub(crate) fn machine_addrs(
    addrs: impl IntoIterator<Item = strider_cfg::PcodeInsnAddr>,
) -> Vec<u64> {
    let mut out: Vec<u64> = addrs
        .into_iter()
        .map(|addr| addr.machine_addr.addr)
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Drain the pending-exception cell: an exception a Python callback stashed
/// rather than raised (a `KeyboardInterrupt` / `SystemExit` from a `read`, or
/// anything a `.when()` predicate raised) is surfaced here.  Raising it
/// directly would have been destroyed by the next callback.
pub(crate) fn check_pending_control_flow() -> PyResult<()> {
    // Same boundary drops a swallowed callback cause: any `StriderError` that
    // wanted it was synthesized before this runs, and a survivor would chain
    // itself onto an unrelated later error and pin its traceback.
    crate::errors::clear_callback_cause();
    if let Some(err) = crate::pattern::take_pending_query_error() {
        return Err(err);
    }
    Ok(())
}

/// On failure, prefer a stashed control-flow exception over the
/// operation's own error.  Success leaves the cell for
/// [`check_pending_control_flow`].
pub(crate) fn prefer_pending_control_flow<T>(result: PyResult<T>) -> PyResult<T> {
    match result {
        Ok(v) => Ok(v),
        Err(e) => {
            check_pending_control_flow()?;
            Err(e)
        }
    }
}

/// Run `f` with both drains: a stashed control-flow exception wins over
/// `f`'s own error, and a stash survived by a successful `f` still surfaces.
///
/// Every entry point that can reach a Python callback, a reader's `read` or a
/// pattern's `.when()`, goes through this (or `analyze`'s open-coded
/// equivalent), so a later, unrelated call cannot inherit a stashed
/// exception.
pub(crate) fn with_pending_control_flow<T>(f: impl FnOnce() -> PyResult<T>) -> PyResult<T> {
    let out = prefer_pending_control_flow(f())?;
    check_pending_control_flow()?;
    Ok(out)
}

/// The `collections.namedtuple` type backing `strider.lift.AnalyzeResult`,
/// created once and cached so its identity (hence `isinstance`) is stable
/// across calls.
static ANALYZE_RESULT_TYPE: pyo3::sync::GILOnceCell<PyObject> = pyo3::sync::GILOnceCell::new();

fn analyze_result_type(py: Python<'_>) -> PyResult<PyObject> {
    ANALYZE_RESULT_TYPE
        .get_or_try_init(py, || {
            let nt = py
                .import_bound("collections")?
                .getattr("namedtuple")?
                .call1(("AnalyzeResult", ("cfg", "function", "unresolved")))?;
            nt.setattr("__module__", "strider.lift")?;
            nt.setattr(
                "__doc__",
                "What Lifter.analyze returns: (cfg, function, unresolved). \
                 `unresolved` holds the machine addresses cfg.incomplete names \
                 \"unresolved_branch\". Empty means fully resolved, NOT that \
                 the answer is complete: cfg.incomplete lists every reason.",
            )?;
            PyResult::Ok(nt.unbind())
        })
        .map(|obj| obj.clone_ref(py))
}

/// Lifts, optimises and resolves functions for one architecture.  Build
/// one with `strider.lift.lifter(arch, mem, rom=None)`; the calling
/// convention is an argument of every `analyze` call.
#[pyclass(name = "Lifter", module = "strider.lift", subclass)]
pub struct PyLifter {
    /// Owns the Sleigh and optional rom; `None` once `__clear__` has released
    /// it. A call borrows the handle for its whole run: `analyze`, `build_cfg`
    /// and `add_elf` exclusively, so any other call while one is in flight, on
    /// this thread or another, raises; the rest share it and may overlap each
    /// other.
    inner: Option<strider_orchestrator::Strider<AnyMemReader>>,
    /// The arch this handle was built for: what `arch` hands back, and whose
    /// register table a `custom(...)` CC / `CallOtherAbi` must share.
    pub(crate) arch: strider_target::SleighArch,
    /// The same Python reader/rom callback objects the adapters hold, so
    /// `__traverse__` can make the otherwise-buried lifter to reader edge
    /// visible to the cyclic GC.  Empty for the owned-data path.
    py_deps: Vec<std::sync::Arc<Py<PyAny>>>,
    /// The exact `mem` object the handle was built from, returned by
    /// `reader()`. `None` only after `__clear__` during GC.
    mem_obj: Option<Py<PyAny>>,
    /// The exact `rom` object the handle was built from, returned by `rom()`.
    /// `None` when no rom was supplied, or after `__clear__` during GC.
    rom_obj: Option<Py<PyAny>>,
    /// The engine `pcode_at` sweeps with, kept between calls with everything
    /// it has decoded: `Sleigh::clone` is a whole `Sleigh::new` plus a commit
    /// replay, and walking an instruction stream is one call per address.
    sweep_sleigh: std::cell::RefCell<Option<SweepSleigh>>,
    /// Bumped by `analyze` and `build_cfg`, the only callers that reach
    /// `set_context_at`, so a cached sweep engine built before a commit is
    /// discarded rather than decoding in a mode the current one has moved past.
    context_gen: std::cell::Cell<u64>,
}

/// A `pcode_at` sweep engine plus everything it has already decoded.
///
/// Decoding writes flow context back into the engine, so this is NOT a clean
/// clone after its first sweep, and re-decoding an address it has already
/// passed would answer in whatever mode a LATER instruction pinned (an ARM
/// backward `blx` pins Thumb at its target). So an address the sweep already
/// decoded is answered out of `answers` and the engine only ever moves forward
/// from `cursor`, which is what makes an answer independent of the order the
/// calls came in. A different entry, or a context commit on the owning
/// `Lifter`, gets a fresh engine.
struct SweepSleigh {
    context_gen: u64,
    entry: u64,
    sleigh: rsleigh::Sleigh<AnyMemReader>,
    /// `sleigh`'s own, which its LOAD / STORE space ids resolve against.
    space_ids: rsleigh::SpaceIds,
    /// `machine addr -> joined p-code`, for every address this sweep decoded.
    ///
    /// Unbounded, so RSS grows with the number of instructions swept: one
    /// rendered-p-code `String` per address. It cannot
    /// simply be capped: an address already passed is answered from here
    /// precisely BECAUSE re-decoding it would answer in whatever mode a later
    /// instruction pinned, so an evicted entry has to drop the engine with it
    /// and re-sweep from `entry`, and a cap alone turns a sequential sweep
    /// into O(range^2). The bounded form is a window plus a reset on a miss
    /// below `cursor`, which also has to stay distinguishable from the
    /// genuinely-mid-instruction case `stepped_past` reports. A fresh
    /// `Lifter` releases it meanwhile.
    answers: std::collections::HashMap<u64, String>,
    /// Where the engine will decode next: `entry` before the first sweep, one
    /// past the last instruction decoded after that.
    cursor: u64,
}

fn collect_py_deps(mem: &MemInput, rom: Option<&MemInput>) -> Vec<std::sync::Arc<Py<PyAny>>> {
    let mut deps = Vec::new();
    if let Some(o) = mem.py_callback() {
        deps.push(o);
    }
    if let Some(o) = rom.and_then(MemInput::py_callback) {
        deps.push(o);
    }
    deps
}

impl PyLifter {
    fn strider(&self) -> PyResult<&strider_orchestrator::Strider<AnyMemReader>> {
        self.inner.as_ref().ok_or_else(cleared)
    }

    fn strider_mut(&mut self) -> PyResult<&mut strider_orchestrator::Strider<AnyMemReader>> {
        self.inner.as_mut().ok_or_else(cleared)
    }

    fn sleigh(&self) -> PyResult<&rsleigh::Sleigh<AnyMemReader>> {
        Ok(self.strider()?.sleigh())
    }

    /// Retires any cached `pcode_at` sweep engine, for a caller about to
    /// decode through the persistent Sleigh and so possibly commit context.
    fn bump_context_gen(&self) {
        self.context_gen.set(self.context_gen.get().wrapping_add(1));
    }
}

fn build_lifter(
    arch: PySleighArch,
    mem: Bound<'_, PyAny>,
    rom: Option<Bound<'_, PyAny>>,
) -> PyResult<PyLifter> {
    let mem_input = mem.extract::<MemInput>()?;
    let rom_input = rom.as_ref().map(|r| r.extract::<MemInput>()).transpose()?;
    let py_deps = collect_py_deps(&mem_input, rom_input.as_ref());
    let arch_value = arch.inner;
    Ok(PyLifter {
        inner: Some(build_strider(arch, mem_input, rom_input)?),
        arch: arch_value,
        py_deps,
        mem_obj: Some(mem.unbind()),
        rom_obj: rom.map(Bound::unbind),
        sweep_sleigh: std::cell::RefCell::new(None),
        context_gen: std::cell::Cell::new(0),
    })
}

#[pymethods]
impl PyLifter {
    /// The `SleighArch` this handle decodes with. Enough, with `reader()` and
    /// `rom()`, to build a second handle over the same memory, which is how
    /// two threads analyse in parallel; read all three before an `analyze`,
    /// `build_cfg` or `add_elf` on this handle is in flight, since they raise
    /// while one is.
    #[getter]
    fn arch(slf: &Bound<'_, Self>) -> PyResult<crate::arch::PySleighArch> {
        let inner = try_borrow_lifter(slf)?.arch;
        Ok(crate::arch::PySleighArch { inner })
    }

    /// Build a handle for `arch` reading code from `mem`, with `rom` as the
    /// optional read-only memory for constant folding.
    #[new]
    #[pyo3(signature = (arch, mem, rom = None))]
    fn new(
        arch: PySleighArch,
        mem: Bound<'_, PyAny>,
        rom: Option<Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        build_lifter(arch, mem, rom)
    }

    /// The code source (`BufferReader` or `MemReader`) this handle was built
    /// with.
    fn reader(slf: &Bound<'_, Self>) -> PyResult<Py<PyAny>> {
        try_borrow_lifter(slf)?
            .mem_obj
            .as_ref()
            .map(|o| o.clone_ref(slf.py()))
            .ok_or_else(|| into_strider_err(anyhow::anyhow!("reader is unavailable")))
    }

    /// The `rom` (read-only memory for constant folding) this handle was built
    /// with, or `None` if none was supplied.
    fn rom(slf: &Bound<'_, Self>) -> PyResult<Option<Py<PyAny>>> {
        let this = try_borrow_lifter(slf)?;
        Ok(this.rom_obj.as_ref().map(|o| o.clone_ref(slf.py())))
    }

    fn __repr__(slf: Bound<'_, Self>) -> PyResult<String> {
        let name: String = slf.get_type().getattr("__name__")?.extract()?;
        Ok(format!("{name}(...)"))
    }

    /// Without this, a cycle from a user's `read()`-callback object back
    /// to the `Lifter` runs through the Sleigh, where the collector can't
    /// see it, and leaks.
    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        for dep in &self.py_deps {
            visit.call(&**dep)?;
        }
        if let Some(o) = &self.mem_obj {
            visit.call(o)?;
        }
        if let Some(o) = &self.rom_obj {
            visit.call(o)?;
        }
        Ok(())
    }

    fn __clear__(&mut self) {
        // The engine's adapters and the sweep engine's reader clone share the
        // `Arc<Py<PyAny>>`s in `py_deps`, so the reference drops only with all
        // three.
        self.py_deps.clear();
        self.inner = None;
        self.sweep_sleigh.get_mut().take();
        self.mem_obj = None;
        self.rom_obj = None;
    }

    /// INTERNAL, for tests. Rebuild this handle's Sleigh and orchestrator
    /// state from `arch`/`mem`/`rom`.
    #[pyo3(name = "_rebuild", signature = (arch, mem, rom = None))]
    fn rebuild(
        slf: &Bound<'_, Self>,
        arch: PySleighArch,
        mem: Bound<'_, PyAny>,
        rom: Option<Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let mut this = try_borrow_lifter_mut(slf)?;
        this.strider()?;
        *this = build_lifter(arch, mem, rom)?;
        Ok(())
    }

    /// INTERNAL. Merge the ELF at `path` into `elf` and rebuild over the
    /// merged image, under one exclusive borrow: a handle in use refuses
    /// before anything is merged, so `symbol` and `read` never see an ELF
    /// the decoder does not. The rebuild follows the merge because an engine
    /// reads a snapshot of the regions; it rebuilds an arch that already
    /// built over the same readers, so only allocation can fail it.
    #[pyo3(name = "_merge_elf")]
    fn merge_elf(
        slf: &Bound<'_, Self>,
        elf: &Bound<'_, crate::reader::PyLoadedElf>,
        path: &str,
        apply_relocations: bool,
        relocated_got_is_rom: bool,
    ) -> PyResult<()> {
        let mut this = try_borrow_lifter_mut(slf)?;
        this.strider()?;
        let (mem, rom) = {
            let mut elf = elf.try_borrow_mut()?;
            elf.add_elf(path, apply_relocations, relocated_got_is_rom)?;
            (elf.reader(), elf.ro_reader())
        };
        let py = slf.py();
        let arch = PySleighArch { inner: this.arch };
        *this = build_lifter(
            arch,
            Bound::new(py, mem)?.into_any(),
            Some(Bound::new(py, rom)?.into_any()),
        )?;
        Ok(())
    }

    /// Every Sleigh user-op name this architecture can emit, indexed by
    /// user-op id.  These are the names `CfgOptions(call_other_abis=...)`
    /// classifies.
    fn user_op_names(slf: &Bound<'_, Self>) -> PyResult<Vec<String>> {
        Ok(try_borrow_lifter(slf)?.strider()?.user_op_names().to_vec())
    }

    /// How `name` is classified: the `opts` entry for it when there is one,
    /// else the built-in table, else `None` for a name strider has no answer
    /// for (which fails the lift of any function containing it).
    #[pyo3(signature = (name, opts=None))]
    fn call_other_abi(
        slf: &Bound<'_, Self>,
        name: &str,
        opts: Option<Py<PyCfgOptions>>,
    ) -> PyResult<Option<crate::call_other_abi::PyCallOtherAbi>> {
        let preset = try_borrow_lifter(slf)?.arch.preset();
        if let Some(opts) = opts
            && let Some(abi) = opts.borrow(slf.py()).call_other_abis.get(name)
        {
            return Ok(Some(abi.clone()));
        }
        Ok(crate::call_other_abi::PyCallOtherAbi::builtin(preset, name))
    }

    /// Build the control-flow graph of the function at `entry`, without
    /// lifting or optimising.  Raises `StriderError` on a build failure.
    ///
    /// Holds the GIL for the whole build, unlike `analyze`: decoding is short
    /// next to a full analysis.
    #[pyo3(signature = (entry, opts=None))]
    fn build_cfg(
        slf: Py<Self>,
        py: Python<'_>,
        entry: &Bound<'_, PyAny>,
        opts: Option<Py<PyCfgOptions>>,
    ) -> PyResult<PyCfg> {
        let entry = entry_addr(entry)?;
        let bound = slf.bind(py);
        {
            let lifter = try_borrow_lifter(bound)?;
            crate::reader::check_mem_unchanged(py, &lifter.mem_obj)?;
        }
        let target = try_borrow_lifter(bound)?.arch;
        // The borrow outlives `cfg_opts`, which holds the seeded table rather
        // than a copy of it.
        let opts_ref = opts.as_ref().map(|o| o.borrow(py));
        let overrides = match &opts_ref {
            Some(o) => Some(o.call_other_overrides(target)?),
            None => None,
        };
        let cfg_opts = match (&opts_ref, &overrides) {
            (Some(o), Some(overrides)) => cfg_options_from(o, overrides),
            _ => strider_cfg::CfgOptions::default(),
        };
        let inner = with_pending_control_flow(|| {
            let mut lifter = try_borrow_lifter_mut(bound)?;
            lifter.bump_context_gen();
            lifter
                .strider_mut()?
                .build_cfg(entry, &cfg_opts)
                .map_err(into_strider_err)
        })?;
        Ok(PyCfg::new(inner, slf, target))
    }

    /// Lift, optimise and resolve the function at `entry`, returning an
    /// `AnalyzeResult` (`cfg`, `function`, `unresolved`; also unpacks as a
    /// 3-tuple).
    ///
    /// A plain `Lifter` needs an address and a `cc`; it raises
    /// `StriderError` for a symbol name or a missing `cc` (`ElfLifter`
    /// accepts a symbol name and supplies a default `cc`), and on lift
    /// failure.
    ///
    /// Returns even when it cannot vouch for every path: `Cfg.incomplete`
    /// lists every address it cannot, and an empty `unresolved` is not a
    /// complete answer. Check `Cfg.incomplete` before trusting an absence
    /// ("no store to X"): code behind a listed address may be missing.
    ///
    /// Runs the fixed-point loop with the GIL released, so other Python
    /// threads keep running, except where `mem` or `rom` is a Python object,
    /// whose adapter takes the GIL back on every read anyway; there the GIL
    /// is held. One consequence of the release: a DAEMON thread sitting
    /// inside this call when the interpreter starts finalizing is killed
    /// while it holds no GIL, and the forced unwind out of the released
    /// region aborts the process. Analyse on non-daemon threads, and join
    /// them.
    #[pyo3(signature = (entry, cc=None, opts=None))]
    fn analyze(
        slf: Py<Self>,
        py: Python<'_>,
        entry: &Bound<'_, PyAny>,
        cc: Option<PyCallingConvention>,
        opts: Option<Py<PyLifterOptions>>,
    ) -> PyResult<PyObject> {
        let entry = entry_addr(entry)?;
        let cc = cc.ok_or_else(|| {
            into_strider_err(anyhow::anyhow!(
                "`cc` is required on a plain Lifter (the handle stores no \
                 default); only ElfLifter derives one from the ELF header"
            ))
        })?;
        let opts = match opts {
            Some(o) => o,
            None => Py::new(py, PyLifterOptions::new_default(py)?)?,
        };
        let opts_ref = opts.borrow(py);
        let compact = opts_ref.compact;
        let resolve_indirect_branches = opts_ref.resolve_indirect_branches;
        let assumptions = opts_ref.assumptions.borrow(py).inner.clone();
        // Materialise the pipeline override BEFORE dropping the GIL below.
        let custom_pipeline = opts_ref
            .pipeline
            .as_ref()
            .map(|p| p.borrow(py).build_pipeline());
        drop(opts_ref);

        let (target, cc_built, per_address_built) = {
            let lifter = try_borrow_lifter(slf.bind(py))?;
            crate::reader::check_mem_unchanged(py, &lifter.mem_obj)?;
            crate::reader::check_mem_unchanged(py, &lifter.rom_obj)?;
            let regs = lifter.strider()?.sleigh_regs();
            let target = lifter.arch;
            let cc_built = build_cc(&cc, regs, target)?;
            let per_address_built = opts.borrow(py).built_per_address_ccs(regs, target)?;
            (target, cc_built, per_address_built)
        };

        // `target` is only known after the borrow above, so the CFG knobs
        // are read here rather than with the rest of the options: nothing
        // between the two points runs Python, and `CfgOptions` is get-only, so
        // it is the same object either way.
        // The two borrows outlive `analyze_opts`, which holds the seeded table
        // rather than a copy of it; both are read-only, and `CfgOptions` is
        // get-only, so nothing can take them `&mut` meanwhile.
        let opts_ref = opts.borrow(py);
        let cfg_ref = opts_ref.cfg.borrow(py);
        let overrides = cfg_ref.call_other_overrides(target)?;
        let analyze_opts = strider_orchestrator::AnalyzeOptions {
            cfg: cfg_options_from(&cfg_ref, &overrides),
            per_address_ccs: &per_address_built,
            resolve_indirect_branches,
            assumptions,
            compact,
        };
        // The fixed-point loop runs without the GIL where it can: a pipeline's
        // boxed passes are `Send`, so a closure capturing one satisfies
        // `allow_threads`'s `Ungil` bound. Holding it would stall every other
        // Python thread for the length of an analysis, which is what an
        // explorer serving in the background would feel.
        //
        // Not where the reader is a Python object. Such an adapter takes the
        // GIL back on EVERY memory read, so dropping it buys almost no
        // parallelism there, and doing so lets a worker thread call into the
        // interpreter while the main thread finalises it: `Python::with_gil`
        // after `Py_Finalize` aborts or segfaults the process, which no
        // `StriderError` can catch. Holding the GIL instead makes
        // finalisation wait for the analysis, the way any other Python call
        // does. `py_deps` is empty exactly on the owned-data path.
        let result = {
            let mut lifter = try_borrow_lifter_mut(slf.bind(py))?;
            lifter.bump_context_gen();
            let reader_calls_python = !lifter.py_deps.is_empty();
            // Reborrow before the closure so its captured type is a plain
            // `&mut Strider`, not the GIL-bound `PyRefMut`, which embeds a
            // `!Send` `Python<'_>` marker and would fail `Ungil`.
            let inner = lifter.strider_mut()?;
            let outcome = if reader_calls_python {
                inner.analyze(entry, &cc_built, &analyze_opts, custom_pipeline)
            } else {
                py.allow_threads(|| inner.analyze(entry, &cc_built, &analyze_opts, custom_pipeline))
            };
            prefer_pending_control_flow(outcome.map_err(into_strider_err))?
        };
        let unresolved = machine_addrs(
            result
                .incomplete()
                .of(strider_cfg::Reason::UnresolvedBranch),
        );
        let strider_orchestrator::AnalyzeResult { cfg, function } = result;

        // Surface anything a Python callback stashed while the GIL was
        // released.
        check_pending_control_flow()?;

        let cfg_obj = Py::new(py, PyCfg::new(cfg, slf.clone_ref(py), target))?;

        let py_function = Py::new(py, PyFunction::new(function, cfg_obj.clone_ref(py)))?;
        let result = analyze_result_type(py)?
            .bind(py)
            .call1((cfg_obj, py_function, unresolved))?;
        Ok(result.unbind())
    }

    /// Decode from `entry` one instruction at a time until `addr`, and
    /// return that instruction's p-code (ops joined `"; "`, empty for an
    /// instruction that lifts to none).
    ///
    /// Each call resumes where the last one from the same `entry` stopped, so
    /// walking a whole instruction stream decodes it once.
    ///
    /// A stand-alone sweep, so it works for an `addr` outside any analysed
    /// CFG.  `addr` must be reachable through the linear instruction stream
    /// from `entry`; raises `StriderError` otherwise.
    ///
    /// Also raises once this `Lifter` has committed more decode modes than the
    /// sweep engine can carry over, which on an ISA-mode arch is the only
    /// alternative to answering in the wrong mode.
    fn pcode_at(slf: &Bound<'_, Self>, entry: u64, addr: u64) -> PyResult<String> {
        let py = slf.py();
        let this = try_borrow_lifter(slf)?;
        // A mapping shortened under us takes a SIGBUS on the read, which no
        // Python `except` can catch.
        crate::reader::check_mem_unchanged(py, &this.mem_obj)?;
        if addr < entry {
            return Err(into_strider_err(anyhow::anyhow!(
                "pcode_at: addr {addr:#x} is before entry {entry:#x}"
            )));
        }
        // `Sleigh::lift_one` carries context-register state across calls, so
        // sweeping through the persistent Sleigh would dirty it for a later
        // `analyze`/`build_cfg`. The clone is that one-way isolation; it is not
        // a clean engine, and must not be one. `Sleigh::clone` replays the
        // pinned `set_context_at` commits, which is what makes this sweep decode
        // an address in the ISA mode the CFG decoded it in rather than in the
        // pspec default. Past `MAX_CONTEXT_COMMITS` the log is gone and the
        // clone would pin nothing, so the sweep would answer in the pspec
        // default for an address the analysis decoded in another mode:
        // `clone_would_replay_context` is what tells the two apart, and the
        // answer it cannot vouch for is refused rather than returned.
        let context_gen = this.context_gen.get();
        // Taken OUT of the cell for the sweep: `lift_one` reaches a Python
        // `MemReader.read`, which can call back in here, and a borrow held
        // across that would raise `PanicException` from the nested call.
        let cached = this
            .sweep_sleigh
            .try_borrow_mut()
            .ok()
            .and_then(|mut slot| slot.take())
            .filter(|s| s.context_gen == context_gen && s.entry == entry);
        let mut sweep = match cached {
            Some(s) => s,
            None => {
                let sleigh = this.sleigh()?;
                if !sleigh.clone_would_replay_context() {
                    return Err(into_strider_err(anyhow::anyhow!(
                        "pcode_at: this Lifter has pinned more decode modes than a sweep \
                         engine can carry over, so {addr:#x} would decode in the pspec \
                         default mode rather than the one the analysis used; build a \
                         fresh Lifter to sweep from"
                    )));
                }
                let sleigh = sleigh.clone();
                SweepSleigh {
                    context_gen,
                    entry,
                    space_ids: sleigh.space_ids(),
                    sleigh,
                    answers: std::collections::HashMap::new(),
                    cursor: entry,
                }
            }
        };
        let out = with_pending_control_flow(|| {
            if let Some(text) = sweep.answers.get(&addr) {
                return Ok(text.clone());
            }
            // Already swept past it without landing on it, so the linear path
            // from entry does not start an instruction there.
            if addr < sweep.cursor {
                return Err(stepped_past(entry, addr));
            }
            let SweepSleigh {
                sleigh,
                space_ids,
                answers,
                cursor,
                ..
            } = &mut sweep;
            let mut decoded: u64 = 0;
            loop {
                // The sweep holds the GIL for its whole run, so Ctrl-C reaches
                // the caller only where this asks for it.
                decoded += 1;
                if decoded.is_multiple_of(1024) {
                    py.check_signals()?;
                }
                let cur = *cursor;
                let (text, len) = crate::pcode::lift_one_text(sleigh, space_ids, cur)?;
                answers.insert(cur, text);
                if len > 0 {
                    *cursor = cur.checked_add(len as u64).ok_or_else(|| {
                        into_strider_err(anyhow::anyhow!(
                            "machine-address overflow advancing past {cur:#x}"
                        ))
                    })?;
                }
                if cur == addr {
                    return Ok(answers[&addr].clone());
                }
                if len == 0 {
                    return Err(into_strider_err(anyhow::anyhow!(
                        "pcode_at: lift_one at {cur:#x} reported a zero-length machine \
                         instruction; cannot advance toward {addr:#x}"
                    )));
                }
                if *cursor > addr {
                    return Err(stepped_past(entry, addr));
                }
            }
        });
        if let Ok(mut slot) = this.sweep_sleigh.try_borrow_mut() {
            *slot = Some(sweep);
        }
        out
    }
}

/// Create a lifter for `arch` that can lift and analyze functions.  `mem`
/// supplies the code bytes; `rom` is the optional read-only memory for
/// constant folding.
#[pyfunction]
#[pyo3(name = "lifter", signature = (arch, mem, rom = None))]
pub fn lifter(
    arch: PySleighArch,
    mem: Bound<'_, PyAny>,
    rom: Option<Bound<'_, PyAny>>,
) -> PyResult<PyLifter> {
    build_lifter(arch, mem, rom)
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyLifter>()?;
    m.add("AnalyzeResult", analyze_result_type(m.py())?)?;
    m.add_function(wrap_pyfunction!(lifter, m)?)?;
    Ok(())
}
