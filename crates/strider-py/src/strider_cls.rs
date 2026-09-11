use std::path::Path;

use pyo3::prelude::*;

use crate::arch::PySleighArch;
use crate::cc::PyCallingConvention;
use crate::cfg::PyCfg;
use crate::dot::dot_style_for;
use crate::errors::into_strider_err;
use crate::function::PyFunction;
use crate::options::{PyCfgOptions, PyLifterOptions};
use crate::reader::{AnyMemReader, MemInput};

/// Pins USE to the creating thread, leaving the value free to move and to be
/// dropped anywhere.
///
/// `Strider` is already `Send`: rsleigh declares it for `SleighCtxLowLevel`,
/// so this type carries no `unsafe` and `T: Send` stays compiler-checked.
/// What is not safe is decoding from two threads, because `lift_one` carries
/// context-register state (ARM/Thumb, x86 segment, MIPS16) across calls and
/// the GIL serialises those calls without stopping them interleaving. `owner`
/// rejects the second thread.
///
/// Moving is what matters: pinned, a `Function` dropped on a worker thread
/// leaks its whole `Lifter`, because PyO3 will not run an `unsendable`
/// destructor off-thread.
///
/// Not generic: the only thing pinned is the `Strider` behind a `Lifter`, and
/// the message below names it, so a second instantiation would have to say
/// something this cannot.
pub(crate) struct ThreadPinned {
    owner: std::thread::ThreadId,
    value: strider_orchestrator::Strider<AnyMemReader>,
}

impl ThreadPinned {
    fn new(value: strider_orchestrator::Strider<AnyMemReader>) -> Self {
        Self {
            owner: std::thread::current().id(),
            value,
        }
    }

    pub(crate) fn check(&self) -> PyResult<()> {
        if std::thread::current().id() == self.owner {
            return Ok(());
        }
        Err(into_strider_err(anyhow::anyhow!(
            "this Lifter was built on another thread and decodes there only; \
             build one on this thread, or move the work back to the thread that \
             created it"
        )))
    }

    pub(crate) fn get(&self) -> PyResult<&strider_orchestrator::Strider<AnyMemReader>> {
        self.check()?;
        Ok(&self.value)
    }

    pub(crate) fn get_mut(&mut self) -> PyResult<&mut strider_orchestrator::Strider<AnyMemReader>> {
        self.check()?;
        Ok(&mut self.value)
    }
}

/// A `custom(...)` convention or user-op ABI froze varnodes resolved against
/// one arch's register table; the same name denotes a different varnode on
/// another arch (x86-64 `EDI` is `%[0x38]:4`, x86 `EDI` is `%[0x1c]:4`), so
/// consuming it there silently mismatches every register.
fn check_source_arch(
    source_arch: Option<&str>,
    target_arch: &str,
    what: std::fmt::Arguments<'_>,
) -> PyResult<()> {
    match source_arch {
        Some(source) if source != target_arch => Err(into_strider_err(anyhow::anyhow!(
            "{what} was built against a {source:?} Sleigh but is being used \
             with a {target_arch:?} Lifter; its register varnodes are frozen \
             at construction, so rebuild it from a {target_arch:?} Sleigh"
        ))),
        _ => Ok(()),
    }
}

pub(crate) fn build_cc(
    cc: &PyCallingConvention,
    regs: &rsleigh::SleighRegs,
    target_arch: &str,
) -> PyResult<strider_target::BuiltCallingConvention> {
    check_source_arch(
        cc.source_arch,
        target_arch,
        format_args!("this custom CallingConvention"),
    )?;
    if cc.no_return {
        return Err(into_strider_err(anyhow::anyhow!(
            "a `no_return()` CallingConvention describes a CALLEE that never \
             returns, which is meaningless for the function being analysed; \
             pass it through LifterOptions(per_address_ccs={{addr: cc}}) instead"
        )));
    }
    match &cc.inner {
        crate::cc::CcImpl::Preset(preset) => preset.build(regs).map_err(into_strider_err),
        crate::cc::CcImpl::Custom(built) => Ok(*built.clone()),
    }
}

pub(crate) fn build_per_address_ccs(
    per_address_ccs_py: std::collections::HashMap<u64, PyCallingConvention>,
    regs: &rsleigh::SleighRegs,
    target_arch: &str,
) -> PyResult<rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>> {
    per_address_ccs_py
        .into_iter()
        .map(|(addr, py_cc)| {
            check_source_arch(
                py_cc.source_arch,
                target_arch,
                format_args!("the custom per-address CallingConvention at {addr:#x}"),
            )?;
            let mut built = match py_cc.inner {
                crate::cc::CcImpl::Preset(preset) => preset.build(regs).map_err(|e| {
                    into_strider_err(anyhow::anyhow!(
                        "per-address CC at {addr:#x} unresolved: {e:?}"
                    ))
                })?,
                crate::cc::CcImpl::Custom(built) => *built,
            };
            // Applied after ABI resolution so it works uniformly for
            // preset and custom CCs.
            built.no_return = py_cc.no_return;
            Ok((addr, built))
        })
        .collect::<PyResult<_>>()
}

/// Borrow the handle, erroring on a re-entrant call instead of panicking.
///
/// `Py::borrow` panics when the handle is already borrowed, and the panic
/// cannot unwind out of rsleigh's `extern "C"` instruction-fetch callback:
/// a `MemReader.read` re-entering the handle would abort the process.
fn try_borrow_lifter<'py>(
    slf: &'py Py<PyLifter>,
    py: Python<'py>,
) -> PyResult<pyo3::PyRef<'py, PyLifter>> {
    slf.try_borrow(py).map_err(|_| reentrant_lifter_err())
}

fn try_borrow_lifter_mut<'py>(
    slf: &'py Py<PyLifter>,
    py: Python<'py>,
) -> PyResult<pyo3::PyRefMut<'py, PyLifter>> {
    slf.try_borrow_mut(py).map_err(|_| reentrant_lifter_err())
}

pub(crate) fn reentrant_lifter_err() -> PyErr {
    into_strider_err(anyhow::anyhow!(
        "this Lifter is already in use by an in-progress analyze/build_cfg; \
         a `read()` callback cannot re-enter the same handle. Build a \
         separate Lifter for the nested analysis"
    ))
}

pub(crate) fn reject_zero_max_size(function_max_size: Option<u64>) -> PyResult<()> {
    if matches!(function_max_size, Some(0)) {
        return Err(pyo3::exceptions::PyValueError::new_err(
            "function_max_size must be > 0; omit the argument for an unbounded lift",
        ));
    }
    Ok(())
}

pub(crate) fn build_orch_sleigh(
    arch: &PySleighArch,
    reader: AnyMemReader,
) -> PyResult<rsleigh::Sleigh<AnyMemReader>> {
    rsleigh::Sleigh::new(arch.inner.sla_spec(), arch.inner.pspec(), reader)
        .map_err(|e| into_strider_err(anyhow::anyhow!("Sleigh::new failed: {e:?}")))
}

/// Shared so `optimize` and `analyze` agree about every assumption knob.
fn opt_options_from(
    py: Python<'_>,
    opts: &PyLifterOptions,
) -> PyResult<strider_orchestrator::opt::OptOptions> {
    let assumptions = {
        let a = opts.assumptions.borrow(py);
        strider_orchestrator::opt::AssumptionOptions {
            stack_global_disjoint: a.stack_global_disjoint,
            assume_incoming_args_survive_calls: a.assume_incoming_args_survive_calls,
            distinct_sp_bases_disjoint: a.distinct_sp_bases_disjoint,
            callee_preserves_stack_args: a.callee_preserves_stack_args,
            noalias_allocators: std::sync::Arc::new(a.noalias_allocators.iter().copied().collect()),
            escape_analysis: a.escape_analysis,
        }
    };
    Ok(strider_orchestrator::opt::OptOptions {
        assumptions,
        resolve_indirect_branches: opts.resolve_indirect_branches,
    })
}

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

/// Python dict keys are unique, so the duplicate-name rejection cannot fire
/// from this surface; it is surfaced rather than unwrapped so a future caller
/// that is not a dict gets the message instead of a panic.
fn overrides_from(
    call_other_abis: &std::collections::HashMap<String, crate::call_other_abi::PyCallOtherAbi>,
    target_arch: &str,
) -> PyResult<strider_target::call_other_abi::CallOtherOverrides> {
    for (name, abi) in call_other_abis {
        check_source_arch(
            abi.source_arch,
            target_arch,
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
pub(crate) fn seat_known_targets(
    known_targets: &std::collections::HashMap<u64, crate::options::KnownTarget>,
) -> rustc_hash::FxHashMap<strider_cfg::PcodeInsnAddr, strider_cfg::ResolvedTargets> {
    known_targets
        .iter()
        .map(|(&addr, answer)| {
            let seated = match answer {
                crate::options::KnownTarget::Return => strider_cfg::ResolvedTargets::LinkRegister,
                crate::options::KnownTarget::Targets(targets) => {
                    strider_cfg::ResolvedTargets::Multiple(
                        targets
                            .iter()
                            .map(|&t| strider_cfg::ResolvedTarget::new(t, None))
                            .collect(),
                    )
                }
            };
            (strider_cfg::PcodeInsnAddr::at_machine_start(addr), seated)
        })
        .collect()
}

/// Every `CfgOptions` field, spelled out. No `..default()` here: a field added
/// later has to be handled rather than silently dropped, which is how
/// `build_cfg` came to ignore `known_targets` entirely.
pub(crate) fn cfg_options_from(
    opts: &PyCfgOptions,
    target_arch: &str,
) -> PyResult<strider_cfg::CfgOptions> {
    Ok(strider_cfg::CfgOptions {
        fn_max_size: opts.function_max_size,
        allow_code_before_start_addr: opts.allow_code_before_start_addr,
        known_targets: seat_known_targets(&opts.known_targets),
        call_other_overrides: overrides_from(&opts.call_other_abis, target_arch)?,
    })
}

pub(crate) fn machine_addrs(addrs: &[strider_cfg::PcodeInsnAddr]) -> Vec<u64> {
    addrs.iter().map(|addr| addr.machine_addr.addr).collect()
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
                 `unresolved` holds the machine addresses of indirect branches \
                 that could not be resolved, and a non-empty list is not an \
                 error. Empty means fully resolved, NOT that the answer is \
                 complete: it is one of five incompleteness channels, the \
                 others being cfg.unverified_seeded_sites(), \
                 cfg.isa_mode_conflicts(), cfg.interior_branch_targets() and \
                 cfg.unmapped_branch_targets(). cfg.is_complete() tests all \
                 five.",
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
    /// Owns the Sleigh, cached register table and optional rom. Pinned to
    /// the creating thread for USE; still free to move and to be dropped.
    inner: ThreadPinned,
    /// The `SleighArch` preset this handle was built for, compared against
    /// the arch a `custom(...)` CC / `CallOtherAbi` froze its varnodes on.
    pub(crate) arch_name: &'static str,
    /// The arch itself, so `arch` can hand back a `SleighArch` and a caller can
    /// build a second handle over the same memory. `Copy`, so keeping it costs
    /// nothing over the name alone.
    arch: strider_target::SleighArch,
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
    /// The engine `pcode_at` sweeps with, kept between calls. `Sleigh::clone`
    /// is a whole `Sleigh::new` plus a commit replay, tens of milliseconds
    /// against the microseconds the one instruction it decodes costs, and
    /// walking an instruction stream is one call per address.
    sweep_sleigh: std::cell::RefCell<Option<SweepSleigh>>,
    /// Bumped by `analyze` and `build_cfg`, the only callers that reach
    /// `set_context_at`, so a cached sweep engine built before a commit is
    /// discarded rather than decoding in a mode the current one has moved past.
    context_gen: std::cell::Cell<u64>,
}

/// A `pcode_at` sweep engine plus what makes it reusable.
///
/// Decoding writes flow context back into the engine, so this is NOT a clean
/// clone after its first sweep. Re-sweeping the SAME entry re-derives the same
/// values it already holds, which is what makes reuse decode as a fresh clone
/// would; a different entry gets a fresh one.
struct SweepSleigh {
    context_gen: u64,
    entry: u64,
    sleigh: rsleigh::Sleigh<AnyMemReader>,
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
    /// Rejects a handle built for a different arch.
    ///
    /// The register and address-space tables a render reads are what differ:
    /// rendering an x86-64 function through an aarch64 handle names RAX `pc`
    /// and RCX `sp`, and emits no error at all, so the check has to be here.
    ///
    /// Takes the graph's arch NAME rather than its handle: reading it off that
    /// handle would borrow the one `analyze` holds mutably with the GIL
    /// released, which is the contention this whole path exists to avoid.
    pub(crate) fn check_arch_is(&self, graph_arch: &'static str) -> PyResult<()> {
        if self.arch_name == graph_arch {
            return Ok(());
        }
        Err(into_strider_err(anyhow::anyhow!(
            "lifter is for {}, but this graph was lifted with {}; a render \
             resolves names through the arch's own tables, so the two must match",
            self.arch_name,
            graph_arch
        )))
    }

    pub(crate) fn sleigh(&self) -> PyResult<&rsleigh::Sleigh<AnyMemReader>> {
        Ok(self.inner.get()?.sleigh())
    }

    /// Retires any cached `pcode_at` sweep engine, for a caller about to
    /// decode through the persistent Sleigh and so possibly commit context.
    fn bump_context_gen(&self) {
        self.context_gen.set(self.context_gen.get().wrapping_add(1));
    }

    /// The `Function::neighborhood_dot` `pretty=True` path: same node
    /// selection, register names resolved against this handle's Sleigh.
    pub(crate) fn dispatch_neighborhood_dot(
        &self,
        function: &PyFunction,
        center: u32,
        depth: usize,
        hub_cap: usize,
        max_nodes: usize,
        count_producers: bool,
    ) -> PyResult<String> {
        with_pending_control_flow(|| {
            let sleigh = self.sleigh()?;
            let guard = function.read_inner().map_err(into_strider_err)?;
            let nid = guard
                .graph()
                .node_id_from_u32(center)
                .ok_or_else(|| into_strider_err(anyhow::anyhow!("invalid node id {center}")))?;
            let dumper = guard.dot_dumper(sleigh).map_err(into_strider_err)?;
            dumper
                .neighborhood_dot(nid, depth, hub_cap, max_nodes, count_producers)
                .map_err(|e| into_strider_err(anyhow::anyhow!(e)))
        })
    }

    /// Pretty-render `function`, resolving register names against this
    /// handle's Sleigh.
    pub(crate) fn dispatch_dot(
        &self,
        function: &PyFunction,
        style: Option<&str>,
        op: DotOp<'_>,
    ) -> PyResult<DotResult> {
        let sleigh = self.sleigh()?;
        let guard = function.read_inner().map_err(into_strider_err)?;
        let dumper = guard.dot_dumper(sleigh).map_err(into_strider_err)?;
        let d = dot::GraphDot::new(dumper, dot_style_for(style)?);
        match op {
            DotOp::DumpHtml(p) => d
                .dump_as_html(Path::new(p))
                .map(|()| DotResult::Unit)
                .map_err(into_strider_err),
            DotOp::DumpDot(p) => d
                .dump_as_dot(Path::new(p))
                .map(|()| DotResult::Unit)
                .map_err(into_strider_err),
            DotOp::HtmlStr => d
                .as_html_from_dot()
                .map(DotResult::Html)
                .map_err(into_strider_err),
            DotOp::DotStr => d.as_dot().map(DotResult::Dot).map_err(into_strider_err),
        }
    }
}

pub(crate) enum DotOp<'a> {
    DumpHtml(&'a str),
    DumpDot(&'a str),
    HtmlStr,
    DotStr,
}

pub(crate) enum DotResult {
    Unit,
    Html(String),
    Dot(String),
}

fn build_lifter(
    arch: PySleighArch,
    mem: Bound<'_, PyAny>,
    rom: Option<Bound<'_, PyAny>>,
) -> PyResult<PyLifter> {
    let mem_input = mem.extract::<MemInput>()?;
    let rom_input = rom.as_ref().map(|r| r.extract::<MemInput>()).transpose()?;
    let py_deps = collect_py_deps(&mem_input, rom_input.as_ref());
    let arch_name = arch.preset_name;
    let arch_value = arch.inner;
    Ok(PyLifter {
        inner: ThreadPinned::new(build_strider(arch, mem_input, rom_input)?),
        arch_name,
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
    /// The `SleighArch` this handle decodes with.
    ///
    /// Enough, with `reader()` and `rom()`, to build a SECOND handle over the
    /// same memory. That is how a background renderer gets a decoder of its
    /// own: decoding is pinned to the creating thread, but the register and
    /// address-space tables a render reads are the same for any handle on the
    /// same arch.
    #[getter]
    fn arch(&self) -> crate::arch::PySleighArch {
        crate::arch::PySleighArch {
            inner: self.arch,
            preset_name: self.arch_name,
        }
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
    fn reader(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.mem_obj
            .as_ref()
            .map(|o| o.clone_ref(py))
            .ok_or_else(|| into_strider_err(anyhow::anyhow!("reader is unavailable")))
    }

    /// The `rom` (read-only memory for constant folding) this handle was built
    /// with, or `None` if none was supplied.
    fn rom(&self, py: Python<'_>) -> Option<Py<PyAny>> {
        self.rom_obj.as_ref().map(|o| o.clone_ref(py))
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
        self.py_deps.clear();
        self.mem_obj = None;
        self.rom_obj = None;
        // Holds a reader clone, which shares the `Arc<Py<PyAny>>` the deps
        // above traverse rather than a reference of its own.
        self.sweep_sleigh.get_mut().take();
    }

    /// INTERNAL. Rebuild this handle's Sleigh and orchestrator state from
    /// `arch`/`mem`/`rom`, so a newly merged-in ELF becomes visible.
    #[pyo3(name = "_rebuild", signature = (arch, mem, rom = None))]
    fn rebuild(
        &mut self,
        arch: PySleighArch,
        mem: Bound<'_, PyAny>,
        rom: Option<Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        // Rejected up front rather than owner-preserving, so `add_elf` fails
        // off-thread exactly as every other Sleigh-touching method does; a
        // rebuild that kept the old owner would leave the caller holding a
        // handle it still cannot use.
        self.inner.check()?;
        *self = build_lifter(arch, mem, rom)?;
        Ok(())
    }

    /// Every Sleigh user-op name this architecture can emit, indexed by
    /// user-op id.  These are the names `CfgOptions(call_other_abis=...)`
    /// classifies.
    fn user_op_names(&self) -> PyResult<Vec<String>> {
        Ok(self.inner.get()?.user_op_names().to_vec())
    }

    /// How `name` is classified: the `opts` entry for it when there is one,
    /// else the built-in table, else `None` for a name strider has no answer
    /// for (which fails the lift of any function containing it).  Raises
    /// `StriderError` off the handle's thread.
    #[pyo3(signature = (name, opts=None))]
    fn call_other_abi(
        &self,
        py: Python<'_>,
        name: &str,
        opts: Option<Py<PyCfgOptions>>,
    ) -> PyResult<Option<crate::call_other_abi::PyCallOtherAbi>> {
        if let Some(opts) = opts
            && let Some(abi) = opts.borrow(py).call_other_abis.get(name)
        {
            return Ok(Some(abi.clone()));
        }
        // `None` means "no answer for this name", so an off-thread failure
        // must raise rather than borrow that meaning.
        let preset = self.inner.get()?.arch().preset();
        Ok(crate::call_other_abi::PyCallOtherAbi::builtin(preset, name))
    }

    /// Build the control-flow graph of the function at `entry`, without
    /// lifting or optimising.  Raises `StriderError` on a build failure.
    ///
    /// Holds the GIL for the whole build, unlike `analyze`: decoding is short
    /// next to a full analysis, and it runs on the handle's own thread.
    #[pyo3(signature = (entry, opts=None))]
    fn build_cfg(
        slf: Py<Self>,
        py: Python<'_>,
        entry: u64,
        opts: Option<Py<PyCfgOptions>>,
    ) -> PyResult<PyCfg> {
        {
            let lifter = try_borrow_lifter(&slf, py)?;
            crate::reader::check_mem_unchanged(py, &lifter.mem_obj)?;
        }
        let arch_name = try_borrow_lifter(&slf, py)?.arch_name;
        let cfg_opts = match &opts {
            Some(o) => cfg_options_from(&o.borrow(py), arch_name)?,
            None => strider_cfg::CfgOptions::default(),
        };
        let inner = with_pending_control_flow(|| {
            let mut lifter = try_borrow_lifter_mut(&slf, py)?;
            lifter.bump_context_gen();
            lifter
                .inner
                .get_mut()?
                .build_cfg(entry, &cfg_opts)
                .map_err(into_strider_err)
        })?;
        // No classifier ran, so every seat the caller supplied is an answer
        // nothing here checked.
        let mut seeded: Vec<u64> = cfg_opts
            .known_targets
            .keys()
            .map(|addr| addr.machine_addr.addr)
            .collect();
        seeded.sort_unstable();
        Ok(PyCfg::new(py, inner, slf, seeded))
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
    /// An empty `unresolved` is not a complete answer: see `AnalyzeResult`
    /// for the five channels, and `Cfg.is_complete` to test them all.
    ///
    /// Runs the fixed-point loop with the GIL released, so other Python
    /// threads keep running. One consequence: a DAEMON thread sitting inside
    /// this call when the interpreter starts finalizing is killed while it
    /// holds no GIL, and the forced unwind out of the released region aborts
    /// the process. Analyse on non-daemon threads, and join them.
    #[pyo3(signature = (entry, cc=None, opts=None))]
    fn analyze(
        slf: Py<Self>,
        py: Python<'_>,
        entry: &Bound<'_, PyAny>,
        cc: Option<PyCallingConvention>,
        opts: Option<Py<PyLifterOptions>>,
    ) -> PyResult<PyObject> {
        // An int that does not fit `u64` is a different mistake from a str, and
        // saying "you need an ElfLifter" to someone holding one sends them the
        // wrong way.
        let entry: u64 = entry.extract().map_err(|_| {
            if entry.is_instance_of::<pyo3::types::PyInt>() {
                into_strider_err(anyhow::anyhow!(
                    "`entry` is out of range for an address: it must fit an \
                     unsigned 64-bit integer"
                ))
            } else {
                into_strider_err(anyhow::anyhow!(
                    "`entry` must be an address (int); a symbol name (str) needs \
                     an ElfLifter. Build one with strider.lift.load_elf(path)"
                ))
            }
        })?;
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
        let per_address_ccs_py = opts_ref.per_address_ccs.clone().unwrap_or_default();
        let opt_opts = opt_options_from(py, &opts_ref)?;
        // Materialise the pipeline override BEFORE dropping the GIL below.
        let custom_pipeline = opts_ref
            .pipeline
            .as_ref()
            .map(|p| p.borrow(py).build_pipeline());
        drop(opts_ref);

        let (arch_name, cc_built, per_address_built) = {
            let lifter = try_borrow_lifter(&slf, py)?;
            crate::reader::check_mem_unchanged(py, &lifter.mem_obj)?;
            crate::reader::check_mem_unchanged(py, &lifter.rom_obj)?;
            let regs = lifter.inner.get()?.sleigh_regs();
            let arch_name = lifter.arch_name;
            let cc_built = build_cc(&cc, regs, arch_name)?;
            let per_address_built = build_per_address_ccs(per_address_ccs_py, regs, arch_name)?;
            (arch_name, cc_built, per_address_built)
        };

        // `arch_name` is only known after the borrow above, so the CFG knobs
        // are read here rather than with the rest of the options: nothing
        // between the two points runs Python, and `CfgOptions` is get-only, so
        // it is the same object either way.
        let lift_opts = {
            let opts_ref = opts.borrow(py);
            let cfg_ref = opts_ref.cfg.borrow(py);
            strider_orchestrator::LiftOptions {
                cfg: cfg_options_from(&cfg_ref, arch_name)?,
                per_address_ccs: per_address_built,
                compact,
            }
        };
        // The fixed-point loop runs without the GIL either way: a pipeline's
        // boxed passes are `Send`, so a closure capturing one satisfies
        // `allow_threads`'s `Ungil` bound. Holding it would stall every other
        // Python thread for the length of an analysis, which is what an
        // explorer serving in the background would feel.
        let result = {
            let mut lifter = try_borrow_lifter_mut(&slf, py)?;
            lifter.bump_context_gen();
            // Reborrow before the closure so its captured type is a plain
            // `&mut Strider`, not the GIL-bound `PyRefMut`, which embeds a
            // `!Send` `Python<'_>` marker and would fail `Ungil`.
            let inner = lifter.inner.get_mut()?;
            match custom_pipeline {
                Some(pipeline) => prefer_pending_control_flow(
                    py.allow_threads(|| {
                        inner.analyze(entry, &cc_built, &lift_opts, &opt_opts, Some(pipeline))
                    })
                    .map_err(into_strider_err),
                )?,
                None => prefer_pending_control_flow(
                    py.allow_threads(|| {
                        inner.analyze(entry, &cc_built, &lift_opts, &opt_opts, None)
                    })
                    .map_err(into_strider_err),
                )?,
            }
        };
        let cfg = result.cfg;
        let function = result.function;
        let unresolved = machine_addrs(&result.unresolved_indirect_branches);

        // Surface anything a Python callback stashed while the GIL was
        // released.
        check_pending_control_flow()?;

        // From the result, not from `cfg`: four of the five accumulate over
        // the resolver's rounds and `cfg` is only the final one, and
        // `unverified_seeded` is read against the settled seed set, which
        // `cfg` does not carry.
        let reports = crate::cfg::CfgReports {
            unresolved: unresolved.clone(),
            unverified_seeded: machine_addrs(&result.unverified_seeded_sites),
            isa_mode_conflicts: machine_addrs(&result.isa_mode_conflicts),
            interior_branch_targets: machine_addrs(&result.interior_branch_targets),
            unmapped_branch_targets: machine_addrs(&result.unmapped_branch_targets),
        };
        let cfg_obj = Py::new(py, PyCfg::with_reports(py, cfg, slf.clone_ref(py), reports))?;

        let py_function = Py::new(
            py,
            PyFunction::new(
                function,
                cfg_obj.clone_ref(py),
                opt_opts.assumptions.clone(),
            ),
        )?;
        let result = analyze_result_type(py)?
            .bind(py)
            .call1((cfg_obj, py_function, unresolved))?;
        Ok(result.unbind())
    }

    /// Run an optimizer pipeline over `function` in place.  `pipeline=None`
    /// runs the default pipeline; a given `OptimizerPipeline` is copied, so it
    /// stays usable.  `opts=None` takes the `LifterOptions` defaults.
    ///
    /// Runs against this handle's rom, so `LoadReadOnly` folds here exactly as
    /// it does inside `analyze`.  Raises `StriderError` for a `function` some
    /// other handle produced: folding its constant-address loads against this
    /// rom would read a different binary's bytes.  Invalidates outstanding
    /// `Node` / `Match` handles for `function`.
    ///
    /// Holds the GIL for the whole run, unlike `analyze`: the run borrows the
    /// function out of its `RefCell`, and the `RefMut` is `!Send`, so the
    /// closure cannot satisfy `allow_threads`'s `Ungil` bound.
    #[pyo3(signature = (function, pipeline=None, opts=None))]
    fn optimize(
        slf: &Bound<'_, Self>,
        py: Python<'_>,
        function: &PyFunction,
        pipeline: Option<&crate::opt::PyOptimizerPipeline>,
        opts: Option<Py<PyLifterOptions>>,
    ) -> PyResult<()> {
        if !function.cfg.bind(py).try_borrow()?.lifter.bind(py).is(slf) {
            return Err(into_strider_err(anyhow::anyhow!(
                "this Function was lifted by a different Lifter; optimize folds \
                 its constant-address loads against the handle's own rom, so the \
                 two must be the same handle"
            )));
        }
        let this = slf.try_borrow().map_err(|_| reentrant_lifter_err())?;
        // `LoadReadOnly` reads the rom, and a mapping shortened under us takes
        // a SIGBUS there, which no Python `except` can catch.
        crate::reader::check_mem_unchanged(py, &this.mem_obj)?;
        crate::reader::check_mem_unchanged(py, &this.rom_obj)?;
        let opts = match opts {
            Some(o) => o,
            None => Py::new(py, PyLifterOptions::new_default(py)?)?,
        };
        let options = opt_options_from(py, &opts.borrow(py))?;
        let rom = this.inner.get()?.rom();
        with_pending_control_flow(|| match pipeline {
            Some(p) => function.run_pipeline_in_place(p.build_pipeline(), "optimize", rom, options),
            None => {
                let pipe = strider_orchestrator::opt::default_pipeline();
                function.run_pipeline_in_place(pipe, "optimize", rom, options)
            }
        })
    }

    /// Look up a register by Sleigh name, or `None` when the name is not
    /// in this arch's table.
    fn reg(&self, name: &str) -> PyResult<Option<crate::sleigh::PyVn>> {
        Ok(self
            .inner
            .get()?
            .sleigh_regs()
            .name_to_vn(name)
            .map(crate::sleigh::PyVn::from_inner))
    }

    /// The reverse of `reg`.  Returns `None` when `vn` names no register
    /// (a non-REGISTER space, or an offset/size not in the table); never
    /// raises.
    fn reg_name(&self, vn: &crate::sleigh::PyVn) -> PyResult<Option<&str>> {
        Ok(self.inner.get()?.sleigh_regs().vn_to_name(vn.inner))
    }

    /// Decode from `entry` one instruction at a time until `addr`, and
    /// return that instruction's p-code (ops joined `"; "`, empty for an
    /// instruction that lifts to none).
    ///
    /// A stand-alone sweep, so it works for an `addr` outside any analysed
    /// CFG.  `addr` must be reachable through the linear instruction stream
    /// from `entry`; raises `StriderError` otherwise.
    fn pcode_at(&self, py: Python<'_>, entry: u64, addr: u64) -> PyResult<String> {
        // A mapping shortened under us takes a SIGBUS on the read, which no
        // Python `except` can catch.
        crate::reader::check_mem_unchanged(py, &self.mem_obj)?;
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
        // pspec default. Past `MAX_CONTEXT_COMMITS` the replay is dropped and
        // the sweep falls back to those defaults, the same best-effort answer
        // `pcode_at` gives for an address no analysis has ever reached.
        self.inner.check()?;
        let context_gen = self.context_gen.get();
        // Taken OUT of the cell for the sweep: `lift_one` reaches a Python
        // `MemReader.read`, which can call back in here, and a borrow held
        // across that would panic inside an `extern "C"` frame.
        let cached = self
            .sweep_sleigh
            .try_borrow_mut()
            .ok()
            .and_then(|mut slot| slot.take())
            .filter(|s| s.context_gen == context_gen && s.entry == entry);
        let mut sweep = match cached {
            Some(s) => s,
            None => SweepSleigh {
                context_gen,
                entry,
                sleigh: self.sleigh()?.clone(),
            },
        };
        let out = with_pending_control_flow(|| {
            let sleigh = &mut sweep.sleigh;
            let mut cur = entry;
            loop {
                let (text, len) = crate::pcode::lift_one_text(sleigh, cur)?;
                if cur == addr {
                    return Ok(text);
                }
                if len == 0 {
                    return Err(into_strider_err(anyhow::anyhow!(
                        "pcode_at: lift_one at {cur:#x} reported a zero-length machine \
                         instruction; cannot advance toward {addr:#x}"
                    )));
                }
                let next = cur.checked_add(len as u64).ok_or_else(|| {
                    into_strider_err(anyhow::anyhow!(
                        "machine-address overflow advancing past {cur:#x}"
                    ))
                })?;
                if next > addr {
                    return Err(into_strider_err(anyhow::anyhow!(
                        "pcode_at: linear sweep from entry {entry:#x} stepped past target \
                         {addr:#x} (misaligned: {addr:#x} is not a machine-instruction \
                         boundary on the linear path from entry)"
                    )));
                }
                cur = next;
            }
        });
        if let Ok(mut slot) = self.sweep_sleigh.try_borrow_mut() {
            *slot = Some(sweep);
        }
        out
    }

    /// Start the interactive explorer for `target`, a `Function` or a `Cfg`.
    /// Prints the local URL and blocks on this thread until Ctrl-C;
    /// `background=True` serves on its own thread and RETURNS the bound port,
    /// so you can keep querying while the page is open.
    ///
    /// `host` / `port` choose where to bind, `port=0` taking whatever is free.
    /// The default binds loopback: any other `host` puts the graph of the
    /// binary you are analysing on the network, and no endpoint authenticates.
    /// `depth` is the neighborhood radius, used only when `whole=False`.
    ///
    /// Opens on the WHOLE graph: a neighborhood view hides nodes without
    /// saying so. `whole=False` opens on the neighborhood around a node you
    /// pick instead (inputs and outputs out to `depth` hops), which is what
    /// scales to large functions; the toolbar's `whole` toggle switches
    /// between them either way. A function of a few thousand nodes can keep
    /// the layout engine busy for a while, so prefer `whole=False` there.
    ///
    /// The neighborhood knobs open uncapped and are set from the page, where
    /// 0 means no limit on each.
    ///
    /// `strider.explore.shutdown(port)` stops a server and joins its thread.
    /// It is registered to run before the interpreter joins non-daemon
    /// threads, so an explorer left running does not hang or abort at exit.
    #[pyo3(signature = (target, host="127.0.0.1", port=0, depth=None, whole=true, background=false))]
    // One parameter per Python keyword; splitting them into a struct would just
    // move the same list somewhere the `#[pyo3(signature)]` cannot see it.
    #[allow(clippy::too_many_arguments)]
    fn visualize(
        &self,
        py: Python<'_>,
        target: Py<PyAny>,
        host: &str,
        port: u16,
        depth: Option<usize>,
        whole: bool,
        background: bool,
    ) -> PyResult<u16> {
        let explore = py.import_bound("strider.explore")?;
        let kwargs = pyo3::types::PyDict::new_bound(py);
        kwargs.set_item("host", host)?;
        kwargs.set_item("port", port)?;
        // `None` leaves the renderer's own default, which the explorer reads
        // off the binding signature and shows as the control's default.
        kwargs.set_item("depth", depth)?;
        kwargs.set_item("whole", whole)?;
        kwargs.set_item("background", background)?;
        explore
            .call_method("visualize", (target,), Some(&kwargs))?
            .extract()
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
