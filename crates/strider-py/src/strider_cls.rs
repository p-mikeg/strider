use pyo3::prelude::*;

use crate::arch::PySleighArch;
use crate::cc::PyCallingConvention;
use crate::cfg::PyCfg;
use crate::errors::into_strider_err;
use crate::function::PyFunction;
use crate::options::{PyCfgOptions, PyLifterOptions};
use crate::reader::{AnyMemReader, MemInput, PyLoadedElf};

fn cleared() -> PyErr {
    into_strider_err(anyhow::anyhow!(
        "this Lifter was cleared by the garbage collector and holds no engine"
    ))
}

/// Refuses a custom convention or user-op ABI built against another arch's
/// register table. Presets sharing a table (`arm` / `arm_thumb`) pass.
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

/// `addr` names the per-address override, for errors.
fn build_cc(
    cc: &PyCallingConvention,
    regs: &rsleigh::SleighRegs,
    target: strider_target::SleighArch,
    addr: Option<u64>,
) -> PyResult<strider_target::BuiltCallingConvention> {
    match addr {
        None => check_source_arch(
            cc.source_arch,
            target,
            format_args!("this custom CallingConvention"),
        ),
        Some(addr) => check_source_arch(
            cc.source_arch,
            target,
            format_args!("the custom per-address CallingConvention at {addr:#x}"),
        ),
    }?;
    match &cc.inner {
        crate::cc::CcImpl::Preset(preset) => preset.build(regs).map_err(|e| {
            into_strider_err(match addr {
                None => e,
                Some(addr) => e.context(format!("per-address CC at {addr:#x} unresolved")),
            })
        }),
        crate::cc::CcImpl::Custom(built) => Ok(*built.clone()),
    }
}

pub(crate) fn build_per_address_ccs(
    per_address_ccs_py: &std::collections::HashMap<u64, PyCallingConvention>,
    regs: &rsleigh::SleighRegs,
    target: strider_target::SleighArch,
) -> PyResult<rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>> {
    per_address_ccs_py
        .iter()
        .map(|(&addr, py_cc)| Ok((addr, build_cc(py_cc, regs, target, Some(addr))?)))
        .collect::<PyResult<_>>()
}

/// Raises `StriderError`, not PyO3's `RuntimeError`, on a conflict with a
/// call in flight. `analyze`, `build_cfg` and `add_elf` borrow exclusively, so
/// they exclude every other `Lifter` method; `ElfLifter`'s ELF reads do not
/// come here.
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
         add_elf exclude every other Lifter method, from any thread or a \
         `read()` callback; ElfLifter symbol and byte reads still work); build a \
         separate Lifter for concurrent work"
    ))
}

/// `entry` as an [`crate::bigint::Addr`]: `TypeError` for a non-int, a `str`
/// naming the one method that takes a symbol.
fn entry_addr(entry: &Bound<'_, PyAny>) -> PyResult<u64> {
    if entry.is_instance_of::<pyo3::types::PyString>() {
        return Err(pyo3::exceptions::PyTypeError::new_err(
            "`entry` must be an address (int); only ElfLifter.analyze takes a \
             symbol name (str)",
        ));
    }
    let py = entry.py();
    entry
        .extract::<crate::bigint::Addr>()
        .map(|a| a.0)
        .map_err(|e| {
            let msg = format!("`entry`: {}", e.value_bound(py));
            if e.is_instance_of::<pyo3::exceptions::PyTypeError>(py) {
                pyo3::exceptions::PyTypeError::new_err(msg)
            } else {
                into_strider_err(anyhow::anyhow!(msg))
            }
        })
}

/// A size fixes the extent, so `allow_code_before_start_addr` cannot join it.
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

/// None without `given`, else `given`, which must hold every claim in force
/// and may not add a lift-read claim.
fn claims_to_add(
    function: &PyFunction,
    given: Option<&crate::options::PyAssumptionOptions>,
) -> PyResult<strider_orchestrator::opt::AssumptionOptions> {
    let Some(given) = given else {
        return Ok(strider_orchestrator::opt::AssumptionOptions::none());
    };
    let given = given.inner.clone();
    let (omitted, lift_claims): (Vec<String>, Vec<&str>) = {
        let read = function.read()?;
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

/// `Function.optimize`: `pipeline` (default when `None`) over `function` in
/// place.
pub(crate) fn optimize_function(
    lifter: &Bound<'_, PyLifter>,
    function: &PyFunction,
    pipeline: Option<strider_orchestrator::opt::OptimizerPipeline>,
    assumptions: Option<&crate::options::PyAssumptionOptions>,
) -> PyResult<()> {
    let py = lifter.py();
    let this = try_borrow_lifter(lifter)?;
    // A mapping shortened under us SIGBUSes on the rom read.
    crate::reader::check_mem_unchanged(py, &[&this.mem_obj, &this.rom_obj])?;
    let claims = claims_to_add(function, assumptions)?;
    let rom = this.strider()?.rom();
    let pipeline = pipeline.unwrap_or_else(strider_orchestrator::opt::default_pipeline);
    let ctx = strider_orchestrator::opt::OptCtx::new(rom);
    with_pending_control_flow(|| {
        function.edit(|f| {
            f.assume(&claims);
            let ran = pipeline.run(f, &ctx);
            ran.map_err(|e| anyhow::anyhow!("optimize failed: {e:#}"))
        })
    })
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

/// Seated at the CFG level, so they apply whether or not the classifier runs.
pub(crate) fn seat_known_targets(
    known_targets: &std::collections::HashMap<u64, crate::options::KnownTarget>,
) -> crate::options::SeatedTargets {
    known_targets
        .iter()
        .map(|(&addr, answer)| {
            let seated = match answer {
                crate::options::KnownTarget::Return => strider_cfg::ResolvedTargets::LinkRegister,
                crate::options::KnownTarget::Targets(targets) => {
                    strider_cfg::ResolvedTargets::seeded(targets.iter().copied())
                }
            };
            (strider_cfg::PcodeInsnAddr::at_machine_start(addr), seated)
        })
        .collect()
}

/// What an `ElfLifter` call adds to its options.
struct ElfSeat {
    /// A symbol's recorded size, bounding a lift the options leave open.
    size: Option<std::num::NonZeroU64>,
    data_ranges: Option<strider_cfg::DataRanges>,
    fragment_ranges: Option<strider_cfg::DataRanges>,
    local_entries: Option<std::sync::Arc<strider_cfg::LocalEntries>>,
}

impl ElfSeat {
    fn read(
        elf: &PyLoadedElf,
        opts: &PyCfgOptions,
        per_address_ccs: bool,
        entry: u64,
        size: Option<u64>,
    ) -> PyResult<Self> {
        let data_ranges = opts.data_ranges.ranges().is_empty();
        let fragment_ranges = opts.fragment_ranges.ranges().is_empty();
        let keyed = per_address_ccs || !opts.no_return_targets.is_empty();
        Ok(Self {
            size: size.and_then(std::num::NonZeroU64::new),
            data_ranges: data_ranges.then(|| elf.data_ranges()).transpose()?,
            fragment_ranges: fragment_ranges
                .then(|| elf.cold_fragments(entry))
                .transpose()?,
            local_entries: keyed.then(|| elf.local_entries()).transpose()?,
        })
    }
}

/// No `..default()`, so a new field must be handled.
fn cfg_options_from<'a>(
    opts: &'a PyCfgOptions,
    call_other_overrides: &'a strider_target::call_other_abi::CallOtherOverrides,
    elf: Option<&'a ElfSeat>,
) -> strider_cfg::CfgOptions<'a> {
    strider_cfg::CfgOptions {
        bound: match (opts.bound, elf.and_then(|e| e.size)) {
            (strider_cfg::FunctionBound::Open { .. }, Some(size)) => {
                strider_cfg::FunctionBound::Size(size)
            }
            (bound, _) => bound,
        },
        known_targets: &opts.seated,
        // Only the resolve loop derives answers of its own.
        derived_targets: &strider_cfg::NO_TARGETS,
        no_return_targets: &opts.no_return_targets,
        local_entries: elf
            .and_then(|e| e.local_entries.as_deref())
            .unwrap_or(&strider_cfg::NO_LOCAL_ENTRIES),
        call_other_overrides,
        data_ranges: elf
            .and_then(|e| e.data_ranges.clone())
            .unwrap_or_else(|| opts.data_ranges.clone()),
        fragment_ranges: elf
            .and_then(|e| e.fragment_ranges.clone())
            .unwrap_or_else(|| opts.fragment_ranges.clone()),
    }
}

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

/// Run `f`, then drain the pending-exception cell, so a later call cannot
/// inherit a stash. A stashed callback exception wins over `f`'s result (see
/// [`crate::errors::pending_over`]).
pub(crate) fn with_pending_control_flow<T>(f: impl FnOnce() -> PyResult<T>) -> PyResult<T> {
    let out = f();
    // A surviving callback cause would chain onto an unrelated later error.
    crate::errors::clear_callback_cause();
    match (crate::pattern::take_pending_query_error(), out) {
        (Some(pending), Err(out)) => Err(Python::with_gil(|py| {
            crate::errors::pending_over(py, pending, out)
        })),
        (Some(pending), Ok(_)) => Err(pending),
        (None, out) => out,
    }
}

/// Cached so `isinstance` is stable.
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

/// Lifts, optimises and resolves functions for one architecture. Build one
/// with `strider.lift.lifter(arch, mem, rom=None)`.
#[pyclass(name = "Lifter", module = "strider.lift", subclass, weakref)]
pub struct PyLifter {
    /// `None` once `__clear__` has released it.
    inner: Option<strider_orchestrator::Strider<AnyMemReader>>,
    /// `None` only after `__clear__`.
    mem_obj: Option<std::sync::Arc<Py<PyAny>>>,
    rom_obj: Option<std::sync::Arc<Py<PyAny>>>,
    /// `mem` or `rom` is a Python callback.
    reader_calls_python: bool,
    /// Kept between calls: a `Sleigh::clone` is a whole `Sleigh::new` plus a
    /// commit replay.
    sweep_sleigh: std::cell::RefCell<Option<SweepSleigh>>,
    /// Bumped by `analyze` and `build_cfg`, which can commit context, to
    /// retire a cached sweep engine.
    context_gen: std::cell::Cell<u64>,
}

/// A forward-only sweep: a passed address is answered from `answers`, since
/// re-decoding it could see a mode a later instruction pinned.
struct SweepSleigh {
    context_gen: u64,
    entry: u64,
    sleigh: rsleigh::Sleigh<AnyMemReader>,
    /// `sleigh`'s own.
    space_ids: rsleigh::SpaceIds,
    /// `machine addr -> joined p-code`. Unbounded: an evicted entry would
    /// force a re-sweep from `entry`.
    answers: std::collections::HashMap<u64, String>,
    cursor: u64,
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

    fn target(&self) -> PyResult<strider_target::SleighArch> {
        Ok(self.strider()?.arch())
    }

    fn bump_context_gen(&self) {
        self.context_gen.set(self.context_gen.get().wrapping_add(1));
    }
}

fn build_lifter(
    arch: PySleighArch,
    mem: Bound<'_, PyAny>,
    rom: Option<Bound<'_, PyAny>>,
) -> PyResult<PyLifter> {
    let mem = mem.extract::<MemInput>()?;
    let rom = rom.map(|r| r.extract::<MemInput>()).transpose()?;
    Ok(PyLifter {
        reader_calls_python: mem.calls_python() || rom.as_ref().is_some_and(MemInput::calls_python),
        mem_obj: Some(std::sync::Arc::clone(&mem.obj)),
        rom_obj: rom.as_ref().map(|r| std::sync::Arc::clone(&r.obj)),
        inner: Some(build_strider(arch, mem, rom)?),
        sweep_sleigh: std::cell::RefCell::new(None),
        context_gen: std::cell::Cell::new(0),
    })
}

#[pymethods]
impl PyLifter {
    /// The `SleighArch` this handle decodes with. Raises while an `analyze`,
    /// `build_cfg` or `add_elf` is in flight.
    #[getter]
    fn arch(slf: &Bound<'_, Self>) -> PyResult<crate::arch::PySleighArch> {
        let inner = try_borrow_lifter(slf)?.target()?;
        Ok(crate::arch::PySleighArch { inner })
    }

    /// A handle for `arch` reading code from `mem`, with optional read-only
    /// memory `rom`.
    #[new]
    #[pyo3(signature = (arch, mem, rom = None))]
    fn new(
        arch: PySleighArch,
        mem: Bound<'_, PyAny>,
        rom: Option<Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        build_lifter(arch, mem, rom)
    }

    /// The `mem` this handle was built with.
    fn reader(slf: &Bound<'_, Self>) -> PyResult<Py<PyAny>> {
        try_borrow_lifter(slf)?
            .mem_obj
            .as_ref()
            .map(|o| o.clone_ref(slf.py()))
            .ok_or_else(|| into_strider_err(anyhow::anyhow!("reader is unavailable")))
    }

    /// The `rom` this handle was built with, or `None`.
    fn rom(slf: &Bound<'_, Self>) -> PyResult<Option<Py<PyAny>>> {
        let this = try_borrow_lifter(slf)?;
        Ok(this.rom_obj.as_ref().map(|o| o.clone_ref(slf.py())))
    }

    fn __repr__(slf: Bound<'_, Self>) -> PyResult<String> {
        Ok(format!("{}(...)", slf.get_type().name()?))
    }

    /// Else a cycle from a `read()` callback back to the `Lifter` leaks.
    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        if let Some(o) = &self.mem_obj {
            visit.call(&**o)?;
        }
        if let Some(o) = &self.rom_obj {
            visit.call(&**o)?;
        }
        Ok(())
    }

    fn __clear__(&mut self) {
        // The engine, the sweep engine and `mem_obj` share a callback's `Arc`.
        self.inner = None;
        self.sweep_sleigh.get_mut().take();
        self.mem_obj = None;
        self.rom_obj = None;
    }

    /// Test-only: rebuild over a new reader, to close a collectable cycle.
    #[cfg(feature = "test-hooks")]
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

    /// Merge the ELF at `path` into `elf` and rebuild, under one exclusive
    /// borrow so a handle in use refuses before anything is merged.
    #[pyo3(name = "_merge_elf")]
    fn merge_elf(
        slf: &Bound<'_, Self>,
        elf: &Bound<'_, crate::reader::PyLoadedElf>,
        path: &str,
        apply_relocations: bool,
        relocated_got_is_rom: bool,
    ) -> PyResult<()> {
        let mut this = try_borrow_lifter_mut(slf)?;
        let arch = PySleighArch {
            inner: this.target()?,
        };
        let (mem, rom) = {
            let mut elf = elf.try_borrow_mut()?;
            elf.add_elf(path, apply_relocations, relocated_got_is_rom)?;
            (elf.reader(), elf.ro_reader())
        };
        let py = slf.py();
        *this = build_lifter(
            arch,
            Bound::new(py, mem)?.into_any(),
            Some(Bound::new(py, rom)?.into_any()),
        )?;
        Ok(())
    }

    /// Every Sleigh user-op name of this arch, indexed by user-op id.
    fn user_op_names(slf: &Bound<'_, Self>) -> PyResult<Vec<String>> {
        Ok(try_borrow_lifter(slf)?.strider()?.user_op_names().to_vec())
    }

    /// The `CallOtherAbi` for user op `name`: the `opts` entry, else the
    /// built-in one, else `None` (a lift reaching it fails). Raises
    /// `StriderError` for an `opts` entry built for another arch.
    #[pyo3(signature = (name, opts=None))]
    fn call_other_abi(
        slf: &Bound<'_, Self>,
        name: &str,
        opts: Option<Py<PyCfgOptions>>,
    ) -> PyResult<Option<crate::call_other_abi::PyCallOtherAbi>> {
        let arch = try_borrow_lifter(slf)?.target()?;
        if let Some(opts) = opts {
            let opts = opts.get();
            opts.call_other_overrides(arch)?;
            if let Some(abi) = opts.call_other_abis.get(name) {
                return Ok(Some(abi.clone()));
            }
        }
        Ok(crate::call_other_abi::PyCallOtherAbi::builtin(
            arch.preset(),
            name,
        ))
    }

    /// The control-flow graph of the function at `entry`, without lifting.
    /// Raises `StriderError` on failure. Holds the GIL throughout.
    #[pyo3(signature = (entry, opts=None))]
    fn build_cfg(
        slf: Py<Self>,
        py: Python<'_>,
        entry: &Bound<'_, PyAny>,
        opts: Option<Py<PyCfgOptions>>,
    ) -> PyResult<PyCfg> {
        Self::build_cfg_at(slf, py, entry, opts, None)
    }

    /// `ElfLifter.build_cfg` over `elf`.
    #[pyo3(name = "_build_cfg_elf", signature = (elf, entry, opts=None))]
    fn build_cfg_elf(
        slf: Py<Self>,
        py: Python<'_>,
        elf: PyRef<'_, PyLoadedElf>,
        entry: &Bound<'_, PyAny>,
        opts: Option<Py<PyCfgOptions>>,
    ) -> PyResult<PyCfg> {
        Self::build_cfg_at(slf, py, entry, opts, Some(elf))
    }

    /// Lift, optimise and resolve the function at address `entry`. Returns
    /// even when incomplete: check `Cfg.incomplete`.
    #[pyo3(signature = (entry, cc=None, opts=None))]
    fn analyze(
        slf: Py<Self>,
        py: Python<'_>,
        entry: &Bound<'_, PyAny>,
        cc: Option<PyCallingConvention>,
        opts: Option<Py<PyLifterOptions>>,
    ) -> PyResult<PyObject> {
        Self::analyze_at(slf, py, entry_addr(entry)?, cc, opts, None)
    }

    /// `ElfLifter.analyze` over `elf`.
    #[pyo3(name = "_analyze_elf", signature = (elf, entry, cc, opts=None))]
    fn analyze_elf(
        slf: Py<Self>,
        py: Python<'_>,
        elf: PyRef<'_, PyLoadedElf>,
        entry: &Bound<'_, PyAny>,
        cc: PyCallingConvention,
        opts: Option<Py<PyLifterOptions>>,
    ) -> PyResult<PyObject> {
        let (addr, size) = if let Ok(name) = entry.downcast::<pyo3::types::PyString>() {
            let sym = elf.symbol(&name.to_cow()?)?;
            (sym.address, sym.size)
        } else if entry.is_instance_of::<pyo3::types::PyInt>() {
            (entry_addr(entry)?, None)
        } else {
            return Err(pyo3::exceptions::PyTypeError::new_err(format!(
                "`entry` must be a symbol name (str) or address (int), got {}",
                entry.get_type().getattr("__name__")?
            )));
        };
        Self::analyze_at(slf, py, addr, Some(cc), opts, Some((elf, size)))
    }

    /// The p-code (ops joined `"; "`, empty for none) of the instruction at
    /// `addr`, decoding linearly from `entry`.
    fn pcode_at(
        slf: &Bound<'_, Self>,
        entry: &Bound<'_, PyAny>,
        addr: crate::bigint::Addr,
    ) -> PyResult<String> {
        let (entry, addr) = (entry_addr(entry)?, addr.0);
        let py = slf.py();
        let this = try_borrow_lifter(slf)?;
        crate::reader::check_mem_unchanged(py, &[&this.mem_obj])?;
        let entry_mode = this.target()?.entry_mode_context(entry);
        let start = if entry_mode.is_some() {
            entry & !1
        } else {
            entry
        };
        if addr < start {
            return Err(into_strider_err(anyhow::anyhow!(
                "pcode_at: addr {addr:#x} is before entry {entry:#x}"
            )));
        }
        // A clone replaying the pinned commits, so the persistent Sleigh's
        // context stays clean and the sweep decodes in the analysis's mode.
        let context_gen = this.context_gen.get();
        // Taken out of the cell: a Python `read` can re-enter `pcode_at`.
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
                let mut sleigh = sleigh.clone();
                if let Some((var, value)) = entry_mode {
                    sleigh
                        .set_context_at(start, var, value)
                        .map_err(|e| into_strider_err(anyhow::anyhow!("pcode_at: {e:#}")))?;
                }
                SweepSleigh {
                    context_gen,
                    entry,
                    space_ids: sleigh.space_ids(),
                    sleigh,
                    answers: std::collections::HashMap::new(),
                    cursor: start,
                }
            }
        };
        let out = with_pending_control_flow(|| {
            if let Some(text) = sweep.answers.get(&addr) {
                return Ok(text.clone());
            }
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
                // Holds the GIL, so Ctrl-C lands only here.
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

impl PyLifter {
    fn build_cfg_at(
        slf: Py<Self>,
        py: Python<'_>,
        entry: &Bound<'_, PyAny>,
        opts: Option<Py<PyCfgOptions>>,
        elf: Option<PyRef<'_, PyLoadedElf>>,
    ) -> PyResult<PyCfg> {
        let opts = match opts {
            Some(o) => o,
            None => Py::new(py, PyCfgOptions::default())?,
        };
        let opts_ref = opts.get();
        let entry = entry_addr(entry)?;
        let seat = elf
            .map(|elf| ElfSeat::read(&elf, opts_ref, false, entry, None))
            .transpose()?;
        let bound = slf.bind(py);
        let target = {
            let lifter = try_borrow_lifter(bound)?;
            crate::reader::check_mem_unchanged(py, &[&lifter.mem_obj])?;
            lifter.target()?
        };
        let overrides = opts_ref.call_other_overrides(target)?;
        let cfg_opts = cfg_options_from(opts_ref, &overrides, seat.as_ref());
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

    /// `elf` carries the ELF and a symbol's size, for an `ElfLifter`.
    fn analyze_at(
        slf: Py<Self>,
        py: Python<'_>,
        entry: u64,
        cc: Option<PyCallingConvention>,
        opts: Option<Py<PyLifterOptions>>,
        elf: Option<(PyRef<'_, PyLoadedElf>, Option<u64>)>,
    ) -> PyResult<PyObject> {
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
        let opts = opts.get();
        let cfg = opts.cfg.get();
        // Materialise the pipeline override BEFORE dropping the GIL below.
        let custom_pipeline = opts
            .pipeline
            .as_ref()
            .map(|p| p.borrow(py).build_pipeline());
        let seat = elf
            .map(|(elf, size)| {
                let keyed = opts
                    .per_address_ccs
                    .as_ref()
                    .is_some_and(|t| !t.py.is_empty());
                ElfSeat::read(&elf, cfg, keyed, entry, size)
            })
            .transpose()?;

        let (target, cc_built, per_address_built) = {
            let lifter = try_borrow_lifter(slf.bind(py))?;
            crate::reader::check_mem_unchanged(py, &[&lifter.mem_obj, &lifter.rom_obj])?;
            let regs = lifter.strider()?.sleigh_regs();
            let target = lifter.target()?;
            let cc_built = build_cc(&cc, regs, target, None)?;
            let per_address_built = opts.built_per_address_ccs(regs, target)?;
            (target, cc_built, per_address_built)
        };

        let overrides = cfg.call_other_overrides(target)?;
        let analyze_opts = strider_orchestrator::AnalyzeOptions {
            cfg: cfg_options_from(cfg, &overrides, seat.as_ref()),
            per_address_ccs: &per_address_built,
            resolve_indirect_branches: opts.resolve_indirect_branches,
            assumptions: opts.assumptions.get().inner.clone(),
            compact: opts.compact,
            entry_values: &[],
        };
        // A Python reader keeps the GIL so finalisation waits for the analysis.
        let result = with_pending_control_flow(|| {
            let mut lifter = try_borrow_lifter_mut(slf.bind(py))?;
            lifter.bump_context_gen();
            let reader_calls_python = lifter.reader_calls_python;
            // A plain `&mut Strider`, not the `!Ungil` `PyRefMut`.
            let inner = lifter.strider_mut()?;
            let outcome = if reader_calls_python {
                inner.analyze(entry, &cc_built, &analyze_opts, custom_pipeline)
            } else {
                py.allow_threads(|| inner.analyze(entry, &cc_built, &analyze_opts, custom_pipeline))
            };
            outcome.map_err(into_strider_err)
        })?;
        let unresolved = machine_addrs(
            result
                .incomplete()
                .of(strider_cfg::Reason::UnresolvedBranch),
        );
        let strider_orchestrator::AnalyzeResult { cfg, function } = result;
        let cfg_obj = Py::new(py, PyCfg::new(cfg, slf.clone_ref(py), target))?;

        let py_function = Py::new(py, PyFunction::new(function, cfg_obj.clone_ref(py)))?;
        let result = analyze_result_type(py)?
            .bind(py)
            .call1((cfg_obj, py_function, unresolved))?;
        Ok(result.unbind())
    }
}

/// A `Lifter` for `arch` reading code from `mem`, with optional read-only
/// memory `rom`.
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
