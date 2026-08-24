use std::path::Path;

use pyo3::prelude::*;
use strider_orchestrator::opt::AliasMode;

use crate::arch::PySleighArch;
use crate::cc::PyCallingConvention;
use crate::cfg::PyCfg;
use crate::dot::dot_style_for;
use crate::errors::into_strider_err;
use crate::function::PyFunction;
use crate::options::{PyCfgOptions, PyLifterOptions};
use crate::reader::{AnyMemReader, MemInput};

pub(crate) fn build_cc(
    cc: &PyCallingConvention,
    regs: &rsleigh::SleighRegs,
) -> PyResult<strider_target::BuiltCallingConvention> {
    match &cc.inner {
        crate::cc::CcImpl::Preset(preset) => preset.build(regs).map_err(into_strider_err),
        crate::cc::CcImpl::Custom(built) => Ok(*built.clone()),
    }
}

pub(crate) fn build_per_address_ccs(
    per_address_ccs_py: std::collections::HashMap<u64, PyCallingConvention>,
    regs: &rsleigh::SleighRegs,
) -> PyResult<rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>> {
    per_address_ccs_py
        .into_iter()
        .map(|(addr, py_cc)| {
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

/// Error on `Some(0)`; zero is not a meaningful bound.
pub(crate) fn reject_zero_max_size(function_max_size: Option<u64>) -> PyResult<()> {
    if matches!(function_max_size, Some(0)) {
        return Err(pyo3::exceptions::PyValueError::new_err(
            "function_max_size must be > 0; omit the argument for an unbounded lift",
        ));
    }
    Ok(())
}

/// `"stack_global_disjoint"` (the default) trusts that stack and
/// global/constant memory never overlap; `"strict"` is the always-sound
/// floor.
pub(crate) fn parse_alias_mode(s: &str) -> PyResult<strider_orchestrator::opt::AliasMode> {
    match s {
        "stack_global_disjoint" => Ok(AliasMode::StackGlobalDisjoint),
        "strict" => Ok(AliasMode::Strict),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "alias_mode must be \"stack_global_disjoint\" or \"strict\", got {other:?}"
        ))),
    }
}

pub(crate) fn build_orch_sleigh(
    arch: &PySleighArch,
    reader: AnyMemReader,
) -> PyResult<rsleigh::Sleigh<AnyMemReader>> {
    rsleigh::Sleigh::new(arch.inner.sla_spec(), arch.inner.pspec(), reader)
        .map_err(|e| into_strider_err(anyhow::anyhow!("Sleigh::new failed: {e:?}")))
}

/// The `OptOptions` an analysis runs under, read off a `LifterOptions`. Shared
/// so `optimize` and `analyze` agree about alias mode and the assumption knobs.
fn opt_options_from(
    py: Python<'_>,
    opts: &PyLifterOptions,
) -> PyResult<strider_orchestrator::opt::OptOptions> {
    let assumptions = {
        let a = opts.assumptions.borrow(py);
        strider_orchestrator::opt::AssumptionOptions {
            distinct_sp_bases_disjoint: a.distinct_sp_bases_disjoint,
            callee_preserves_stack_args: a.callee_preserves_stack_args,
            noalias_allocators: a.noalias_allocators.iter().copied().collect(),
            escape_analysis: a.escape_analysis,
        }
    };
    Ok(strider_orchestrator::opt::OptOptions {
        // Already validated at `LifterOptions` construction time.
        alias_mode: parse_alias_mode(&opts.alias_mode)?,
        assume_incoming_args_survive_calls: opts.assume_incoming_args_survive_calls,
        assumptions,
        resolve_indirect_branches: opts.resolve_indirect_branches,
    })
}

/// Build the orchestrator handle for `arch` over `mem` and optional `rom`.
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

pub(crate) fn orch_lift_opts(
    function_max_size: Option<u64>,
    allow_code_before_start_addr: bool,
    per_address_ccs: rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>,
    compact: bool,
    known_targets: &std::collections::HashMap<u64, crate::options::KnownTarget>,
    call_other_abis: &std::collections::HashMap<String, crate::call_other_abi::PyCallOtherAbi>,
) -> strider_orchestrator::LiftOptions {
    // A caller-supplied answer seats at the CFG level, so it applies whether or
    // not the classifier runs.
    let known = known_targets
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
        .collect();
    strider_orchestrator::LiftOptions {
        cfg: strider_cfg::CfgOptions {
            fn_max_size: function_max_size,
            allow_code_before_start_addr,
            known_targets: known,
            call_other_overrides: strider_target::call_other_abi::CallOtherOverrides::new(
                call_other_abis
                    .iter()
                    .map(|(name, abi)| (name.clone(), abi.to_override()))
                    .collect(),
            ),
        },
        per_address_ccs,
        compact,
    }
}

pub(crate) fn unresolved_machine_addrs(branches: &[strider_cfg::PcodeInsnAddr]) -> Vec<u64> {
    branches.iter().map(|addr| addr.machine_addr.addr).collect()
}

/// Drain the pending control-flow cell: a `KeyboardInterrupt` /
/// `SystemExit` a Python callback stashed rather than raised (raising
/// would have been destroyed by the next callback) is surfaced here.
pub(crate) fn check_pending_control_flow() -> PyResult<()> {
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
                 that could not be resolved; a non-empty list is not an error.",
            )?;
            PyResult::Ok(nt.unbind())
        })
        .map(|obj| obj.clone_ref(py))
}

/// Lifts, optimises and resolves functions for one architecture.  Build
/// one with `strider.lift.lifter(arch, mem, rom=None)`; the calling
/// convention is an argument of every `analyze` call.
#[pyclass(name = "Lifter", module = "strider.lift", unsendable, subclass)]
pub struct PyLifter {
    /// Owns the Sleigh, cached register table and optional rom.
    inner: strider_orchestrator::Strider<AnyMemReader>,
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
    pub(crate) fn sleigh(&self) -> &rsleigh::Sleigh<AnyMemReader> {
        self.inner.sleigh()
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
            let sleigh = self.sleigh();
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
        let sleigh = self.sleigh();
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

/// Shared construction: extract the code/rom sources, collect their Python
/// objects for the GC traversal, and build the orchestrator handle.
fn build_lifter(
    arch: PySleighArch,
    mem: Bound<'_, PyAny>,
    rom: Option<Bound<'_, PyAny>>,
) -> PyResult<PyLifter> {
    let mem_input = mem.extract::<MemInput>()?;
    let rom_input = rom.as_ref().map(|r| r.extract::<MemInput>()).transpose()?;
    let py_deps = collect_py_deps(&mem_input, rom_input.as_ref());
    Ok(PyLifter {
        inner: build_strider(arch, mem_input, rom_input)?,
        py_deps,
        mem_obj: Some(mem.unbind()),
        rom_obj: rom.map(Bound::unbind),
    })
}

#[pymethods]
impl PyLifter {
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
        *self = build_lifter(arch, mem, rom)?;
        Ok(())
    }

    /// Every Sleigh user-op name this architecture can emit, indexed by
    /// user-op id.  These are the names `CfgOptions(call_other_abis=...)`
    /// classifies.
    fn user_op_names(&self) -> Vec<String> {
        self.inner.user_op_names().to_vec()
    }

    /// How `name` is classified: the `opts` entry for it when there is one,
    /// else the built-in table, else `None` for a name strider has no answer
    /// for (which fails the lift of any function containing it).
    #[pyo3(signature = (name, opts=None))]
    fn call_other_abi(
        &self,
        py: Python<'_>,
        name: &str,
        opts: Option<Py<PyCfgOptions>>,
    ) -> Option<crate::call_other_abi::PyCallOtherAbi> {
        if let Some(opts) = opts
            && let Some(abi) = opts.borrow(py).call_other_abis.get(name)
        {
            return Some(abi.clone());
        }
        crate::call_other_abi::PyCallOtherAbi::builtin(self.inner.arch().preset(), name)
    }

    /// Build the control-flow graph of the function at `entry`, without
    /// lifting or optimising.  Raises `StriderError` on a build failure.
    #[pyo3(signature = (entry, opts=None))]
    fn build_cfg(
        slf: Py<Self>,
        py: Python<'_>,
        entry: u64,
        opts: Option<Py<PyCfgOptions>>,
    ) -> PyResult<PyCfg> {
        let (function_max_size, allow_code_before_start_addr, call_other_abis) = match opts {
            Some(o) => {
                let o = o.borrow(py);
                (
                    o.function_max_size,
                    o.allow_code_before_start_addr,
                    o.call_other_abis.clone(),
                )
            }
            None => (None, false, std::collections::HashMap::new()),
        };
        let cfg_opts = strider_cfg::CfgOptions {
            allow_code_before_start_addr,
            fn_max_size: function_max_size,
            call_other_overrides: strider_target::call_other_abi::CallOtherOverrides::new(
                call_other_abis
                    .iter()
                    .map(|(name, abi)| (name.clone(), abi.to_override()))
                    .collect(),
            ),
            ..strider_cfg::CfgOptions::default()
        };
        let inner = with_pending_control_flow(|| {
            let mut lifter = try_borrow_lifter_mut(&slf, py)?;
            lifter
                .inner
                .build_cfg(entry, &cfg_opts)
                .map_err(into_strider_err)
        })?;
        Ok(PyCfg::new(inner, slf))
    }

    /// Lift, optimise and resolve the function at `entry`, returning an
    /// `AnalyzeResult` (`cfg`, `function`, `unresolved`; also unpacks as a
    /// 3-tuple).
    ///
    /// A plain `Lifter` needs an address and a `cc`; it raises
    /// `StriderError` for a symbol name or a missing `cc` (`ElfLifter`
    /// accepts a symbol name and supplies a default `cc`).  Raises
    /// `ValueError` for a nested `function_max_size == 0` or an
    /// unrecognised `alias_mode`, and `StriderError` on lift failure.
    #[pyo3(signature = (entry, cc=None, opts=None))]
    fn analyze(
        slf: Py<Self>,
        py: Python<'_>,
        entry: &Bound<'_, PyAny>,
        cc: Option<PyCallingConvention>,
        opts: Option<Py<PyLifterOptions>>,
    ) -> PyResult<PyObject> {
        let entry: u64 = entry.extract().map_err(|_| {
            into_strider_err(anyhow::anyhow!(
                "`entry` must be an address (int); a symbol name (str) needs \
                 an ElfLifter. Build one with strider.lift.load_elf(path)"
            ))
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
        let (function_max_size, allow_code_before_start_addr, known_targets, call_other_abis) = {
            let cfg = opts_ref.cfg.borrow(py);
            (
                cfg.function_max_size,
                cfg.allow_code_before_start_addr,
                cfg.known_targets.clone(),
                cfg.call_other_abis.clone(),
            )
        };
        let compact = opts_ref.compact;
        let per_address_ccs_py = opts_ref.per_address_ccs.clone().unwrap_or_default();
        let opt_opts = opt_options_from(py, &opts_ref)?;
        // Materialise the pipeline override BEFORE dropping the GIL below.
        let custom_pipeline = opts_ref
            .pipeline
            .as_ref()
            .map(|p| p.borrow(py).build_pipeline());
        drop(opts_ref);

        let (cc_built, per_address_built) = {
            let lifter = try_borrow_lifter(&slf, py)?;
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
            &known_targets,
            &call_other_abis,
        );
        // The fixed-point loop runs without the GIL on the default path
        // only: `custom_pipeline`'s boxed `dyn Optimizer`s aren't `Send`, so
        // a closure capturing one fails `allow_threads`'s `Ungil` bound.
        let result = {
            let mut lifter = try_borrow_lifter_mut(&slf, py)?;
            // Reborrow before the closure so its captured type is a plain
            // `&mut Strider`, not the GIL-bound `PyRefMut`, which embeds a
            // `!Send` `Python<'_>` marker and would fail `Ungil`.
            let inner = &mut lifter.inner;
            match custom_pipeline {
                Some(pipeline) => prefer_pending_control_flow(
                    inner
                        .analyze(entry, &cc_built, &lift_opts, &opt_opts, Some(pipeline))
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
        let unresolved = unresolved_machine_addrs(&result.unresolved_indirect_branches);

        // Surface anything a Python callback stashed while the GIL was
        // released.
        check_pending_control_flow()?;

        let cfg_obj = Py::new(py, PyCfg::new(cfg, slf.clone_ref(py)))?;

        let py_function = Py::new(py, PyFunction::new(function, cfg_obj.clone_ref(py)))?;
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
    /// it does inside `analyze`.  Invalidates outstanding `Node` / `Match`
    /// handles for `function`.
    #[pyo3(signature = (function, pipeline=None, opts=None))]
    fn optimize(
        &self,
        py: Python<'_>,
        function: &PyFunction,
        pipeline: Option<&crate::opt::PyOptimizerPipeline>,
        opts: Option<Py<PyLifterOptions>>,
    ) -> PyResult<()> {
        let opts = match opts {
            Some(o) => o,
            None => Py::new(py, PyLifterOptions::new_default(py)?)?,
        };
        let options = opt_options_from(py, &opts.borrow(py))?;
        let rom = self.inner.rom();
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
    fn reg(&self, name: &str) -> Option<crate::sleigh::PyVn> {
        self.inner
            .sleigh_regs()
            .name_to_vn(name)
            .map(crate::sleigh::PyVn::from_inner)
    }

    /// The reverse of `reg`.  Returns `None` when `vn` names no register
    /// (a non-REGISTER space, or an offset/size not in the table); never
    /// raises.
    fn reg_name(&self, vn: &crate::sleigh::PyVn) -> Option<&str> {
        self.inner.sleigh_regs().vn_to_name(vn.inner)
    }

    /// Decode from `entry` one instruction at a time until `addr`, and
    /// return that instruction's p-code (ops joined `"; "`, empty for an
    /// instruction that lifts to none).
    ///
    /// A stand-alone sweep, so it works for an `addr` outside any analysed
    /// CFG.  `addr` must be reachable through the linear instruction stream
    /// from `entry`; raises `StriderError` otherwise.
    fn pcode_at(&self, entry: u64, addr: u64) -> PyResult<String> {
        if addr < entry {
            return Err(into_strider_err(anyhow::anyhow!(
                "pcode_at: addr {addr:#x} is before entry {entry:#x}"
            )));
        }
        // `Sleigh::lift_one` carries context-register state across calls, so
        // sweeping through the persistent Sleigh would dirty it for a later
        // `analyze`/`build_cfg`. A clone inherits no context state.
        let mut sleigh = self.sleigh().clone();
        with_pending_control_flow(|| {
            let mut cur = entry;
            loop {
                let (text, len) = crate::pcode::lift_one_text(&mut sleigh, cur)?;
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
        })
    }

    /// Start the interactive explorer for `target`, a `Function` or a `Cfg`.
    /// It renders the NEIGHBORHOOD around a node you pick (inputs and outputs
    /// out to `depth` hops), never the whole graph, so it scales to large
    /// functions. Prints the local URL and blocks on this thread until Ctrl-C.
    ///
    /// Off the main thread you MUST pair this with
    /// `strider.explore.shutdown(port)` and a thread join before the
    /// interpreter exits, or the process aborts.
    #[pyo3(signature = (target, host="127.0.0.1".to_string(), port=0, depth=None))]
    fn visualize(
        &self,
        py: Python<'_>,
        target: Py<PyAny>,
        host: String,
        port: u16,
        depth: Option<usize>,
    ) -> PyResult<()> {
        let explore = py.import_bound("strider.explore")?;
        let kwargs = pyo3::types::PyDict::new_bound(py);
        kwargs.set_item("host", host)?;
        kwargs.set_item("port", port)?;
        // `None` leaves the renderer's own default, which the explorer reads
        // off the binding signature and shows as the control's default.
        kwargs.set_item("depth", depth)?;
        explore.call_method("visualize", (target,), Some(&kwargs))?;
        Ok(())
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
