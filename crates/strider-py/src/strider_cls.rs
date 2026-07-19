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

/// Error on `Some(0)`; zero is not a meaningful bound.
pub(crate) fn reject_zero_max_size(function_max_size: Option<u64>) -> PyResult<()> {
    if matches!(function_max_size, Some(0)) {
        return Err(pyo3::exceptions::PyValueError::new_err(
            "function_max_size must be > 0 (zero is meaningless — omit the argument for unbounded)",
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

pub(crate) fn unresolved_machine_addrs(branches: &[strider_cfg::PcodeInsnAddr]) -> Vec<u64> {
    branches.iter().map(|addr| addr.machine_addr.addr).collect()
}

/// Drain the pending control-flow cell: a `KeyboardInterrupt` /
/// `SystemExit` a Python callback stashed rather than raised (raising
/// would have been destroyed by the next callback) is surfaced here.
pub(crate) fn check_pending_control_flow() -> PyResult<()> {
    if let Some(err) = crate::pattern::take_pending_control_flow() {
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

/// What `Lifter.analyze` returns: the CFG, the lifted and optimised IR,
/// and the addresses of any indirect branch that stayed unresolved.
///
/// Named fields, but it also unpacks as
/// `cfg, function, unresolved = lifter.analyze(...)`.
#[pyclass(name = "AnalyzeResult", module = "strider.lift", unsendable)]
pub struct PyAnalyzeResult {
    /// The FINAL resolve/re-lift iteration's CFG, the one `function` was
    /// actually lifted from.
    #[pyo3(get)]
    cfg: Py<PyCfg>,
    #[pyo3(get)]
    function: Py<PyFunction>,
    /// Machine addresses of indirect branches that could not be resolved.
    /// A non-empty list is NOT an error.
    #[pyo3(get)]
    unresolved: Vec<u64>,
}

#[pymethods]
impl PyAnalyzeResult {
    /// Expose the strong `Py<>` handles to the cyclic GC so a cycle routed
    /// through a result object stays collectable instead of leaking.
    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        visit.call(&self.cfg)?;
        visit.call(&self.function)
    }

    fn __len__(&self) -> usize {
        3
    }

    /// Positional access in field order.  Accepts negative indices.
    fn __getitem__(&self, py: Python<'_>, idx: isize) -> PyResult<PyObject> {
        let idx = if idx < 0 { idx + 3 } else { idx };
        match idx {
            0 => Ok(self.cfg.to_object(py)),
            1 => Ok(self.function.to_object(py)),
            2 => Ok(self.unresolved.to_object(py)),
            _ => Err(pyo3::exceptions::PyIndexError::new_err(
                "AnalyzeResult index out of range (expected 0..3)",
            )),
        }
    }

    /// Iterate the three fields in order.
    fn __iter__(&self, py: Python<'_>) -> PyResult<PyObject> {
        let items = pyo3::types::PyTuple::new_bound(
            py,
            [
                self.cfg.to_object(py),
                self.function.to_object(py),
                self.unresolved.to_object(py),
            ],
        );
        Ok(items.as_any().iter()?.unbind().into())
    }

    fn __repr__(&self) -> String {
        format!(
            "AnalyzeResult(cfg=..., function=..., unresolved={} addr(s))",
            self.unresolved.len()
        )
    }
}

/// The lift+optimise+resolve handle.  Construct via
/// `strider.lifter(arch, mem, rom=None)`; call `build_cfg` for a
/// structural-only CFG, or `analyze(entry, cc, ...)` for the full lift.
#[pyclass(name = "Lifter", module = "strider.lift", unsendable, subclass)]
pub struct PyLifter {
    /// Owns the Sleigh, cached register table and optional rom.
    inner: strider_orchestrator::Strider<AnyMemReader>,
    /// The same Python reader/rom callback objects the adapters hold, so
    /// `__traverse__` can make the otherwise-buried lifter to reader edge
    /// visible to the cyclic GC.  Empty for the owned-data path.
    py_deps: Vec<std::sync::Arc<Py<PyAny>>>,
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
    /// The owned `Sleigh`.
    pub(crate) fn sleigh(&self) -> &rsleigh::Sleigh<AnyMemReader> {
        self.inner.sleigh()
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

#[pymethods]
impl PyLifter {
    /// Construct a lift handle over `mem` for `arch`, with optional
    /// read-only `rom` for constant-load folding.
    #[new]
    #[pyo3(signature = (arch, mem, rom = None))]
    fn new(arch: PySleighArch, mem: MemInput, rom: Option<MemInput>) -> PyResult<Self> {
        let py_deps = collect_py_deps(&mem, rom.as_ref());
        Ok(PyLifter {
            inner: build_strider(arch, mem, rom)?,
            py_deps,
        })
    }

    /// Without this, a cycle from a user's `read()`-callback object back
    /// to the `Lifter` runs through the Sleigh, where the collector can't
    /// see it, and leaks.
    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        for dep in &self.py_deps {
            visit.call(&**dep)?;
        }
        Ok(())
    }

    fn __clear__(&mut self) {
        self.py_deps.clear();
    }

    /// INTERNAL. Rebuild this handle's Sleigh and orchestrator state from
    /// `arch`/`mem`/`rom`, so a newly merged-in ELF becomes visible.
    #[pyo3(name = "_rebuild", signature = (arch, mem, rom = None))]
    fn rebuild(
        &mut self,
        arch: PySleighArch,
        mem: MemInput,
        rom: Option<MemInput>,
    ) -> PyResult<()> {
        let py_deps = collect_py_deps(&mem, rom.as_ref());
        self.inner = build_strider(arch, mem, rom)?;
        self.py_deps = py_deps;
        Ok(())
    }

    /// Build a control-flow graph for the function at `entry`: no lift, no
    /// optimisation, no indirect-branch resolution.  Every
    /// `BranchIndirect` is left as an `UnresolvedIndirectBranch`
    /// terminator, since resolution is `analyze`'s job.
    ///
    /// `opts` is a `CfgOptions`, defaulting to all-defaults.  Raises
    /// `StriderError` on a build failure.
    #[pyo3(signature = (entry, opts=None))]
    fn build_cfg(
        slf: Py<Self>,
        py: Python<'_>,
        entry: u64,
        opts: Option<Py<PyCfgOptions>>,
    ) -> PyResult<PyCfg> {
        let (function_max_size, allow_code_before_start_addr) = match opts {
            Some(o) => {
                let o = o.borrow(py);
                (o.function_max_size, o.allow_code_before_start_addr)
            }
            None => (None, false),
        };
        let cfg_opts = strider_cfg::CfgOptions {
            allow_code_before_start_addr,
            fn_max_size: function_max_size,
            ..strider_cfg::CfgOptions::default()
        };
        let inner = {
            let mut lifter = slf.borrow_mut(py);
            lifter
                .inner
                .build_cfg(entry, &cfg_opts)
                .map_err(into_strider_err)?
        };
        Ok(PyCfg::new(inner, slf))
    }

    /// Lift the function at `entry`, optimise it to a fixed point, resolve
    /// its indirect branches, and return an `AnalyzeResult` carrying
    /// `.cfg`, `.function` and `.unresolved` (it also unpacks as a
    /// 3-tuple).  `cfg` is the FINAL resolve/re-lift iteration's CFG, the
    /// one `function` was lifted from.
    ///
    /// Args:
    ///     entry: Address of the function to analyse.  A `str` symbol name
    ///         needs an `ElfLifter`; on a plain `Lifter` it raises
    ///         `StriderError`.
    ///     cc: Calling convention for this analysis, with
    ///         per-target-address overrides via `opts.per_address_ccs`.
    ///         Required on a plain `Lifter`; `ElfLifter` defaults it to the
    ///         ELF-derived CC.
    ///     opts: A `LifterOptions`, defaulting to all-defaults.  A set
    ///         `opts.pipeline` replaces the built-in default optimizer
    ///         pipeline for this call only.
    ///
    /// Raises `ValueError` for a nested `function_max_size == 0` or an
    /// unrecognised `alias_mode`, and `StriderError` on lift failure.
    #[pyo3(signature = (entry, cc=None, opts=None))]
    fn analyze(
        slf: Py<Self>,
        py: Python<'_>,
        entry: &Bound<'_, PyAny>,
        cc: Option<PyCallingConvention>,
        opts: Option<Py<PyLifterOptions>>,
    ) -> PyResult<PyAnalyzeResult> {
        let entry: u64 = entry.extract().map_err(|_| {
            into_strider_err(anyhow::anyhow!(
                "`entry` must be an address (int); a symbol name (str) needs \
                 an ElfLifter — build one with strider.lift.load_elf(path)"
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
        let (function_max_size, allow_code_before_start_addr) = {
            let cfg = opts_ref.cfg.borrow(py);
            (cfg.function_max_size, cfg.allow_code_before_start_addr)
        };
        let compact = opts_ref.compact;
        let per_address_ccs_py = opts_ref.per_address_ccs.clone().unwrap_or_default();
        let calls_clobber = opts_ref.calls_clobber;
        let assume_distinct_sp_bases_disjoint = opts_ref.assume_distinct_sp_bases_disjoint;
        // Already validated at `LifterOptions` construction time.
        let alias_mode = parse_alias_mode(&opts_ref.alias_mode)?;
        // Materialise the pipeline override BEFORE dropping the GIL below.
        let custom_pipeline = opts_ref
            .pipeline
            .as_ref()
            .map(|p| p.borrow(py).drain_into_pipeline(false))
            .transpose()?;
        drop(opts_ref);

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

        // The fixed-point loop runs without the GIL, but only on the
        // default path: `custom_pipeline`'s boxed `dyn Optimizer` trait
        // objects aren't `Send`, so a closure capturing it fails
        // `allow_threads`'s `Ungil` bound even though every concrete pass
        // inside is callback-free and would be sound to move. Holding the
        // GIL for that uncommon path beats threading a `Send` bound
        // through strider-opt's dyn traits.
        let result = {
            let mut lifter = slf.borrow_mut(py);
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
        Ok(PyAnalyzeResult {
            cfg: cfg_obj,
            function: py_function,
            unresolved,
        })
    }

    /// Run an optimizer pipeline over `function`'s IR in place.  Useful
    /// after a manual `Function.rewrite(...)` to re-converge the graph, or
    /// to layer extra passes on an already-analyzed function.
    ///
    /// `pipeline=None` runs the canonical default, the same one `analyze`
    /// drives internally.  A given `OptimizerPipeline` is DRAINED, so
    /// rebuild it before reusing it.
    ///
    /// Bumps `function`'s generation, invalidating outstanding
    /// `Node`/`Match` handles.
    ///
    /// A custom `pipeline` runs without a rom image, so any `LoadReadOnly`
    /// pass in it short-circuits silently.
    #[pyo3(signature = (function, pipeline=None))]
    fn optimize(
        &self,
        function: &PyFunction,
        pipeline: Option<&crate::opt::PyOptimizerPipeline>,
    ) -> PyResult<()> {
        match pipeline {
            Some(p) => {
                let real_pipeline = p.drain_into_pipeline(false)?;
                function.run_pipeline_in_place(real_pipeline, "optimize")
            }
            None => {
                let pipe = strider_orchestrator::opt::default_pipeline();
                function.run_pipeline_in_place(pipe, "optimize")
            }
        }
    }

    /// Render the depth-`depth` neighborhood (inputs and outputs) around
    /// IR node `center` to a standalone Graphviz DOT string, one DOT node
    /// per IR node (a DOT node id IS the IR node id) with `center`
    /// highlighted.
    ///
    /// A node whose degree exceeds `hub_cap` is shown but not expanded
    /// through.  `max_nodes` caps the total, nearest first.
    #[pyo3(signature = (function, center, depth=5, hub_cap=12, max_nodes=60))]
    fn neighborhood_dot(
        &self,
        function: &PyFunction,
        center: u32,
        depth: usize,
        hub_cap: usize,
        max_nodes: usize,
    ) -> PyResult<String> {
        let sleigh = self.sleigh();
        let guard = function.read_inner().map_err(into_strider_err)?;
        let nid = guard
            .graph()
            .node_id_from_u32(center)
            .ok_or_else(|| into_strider_err(anyhow::anyhow!("invalid node id {center}")))?;
        let dumper = guard.dot_dumper(sleigh).map_err(into_strider_err)?;
        dumper
            .neighborhood_dot(nid, depth, hub_cap, max_nodes)
            .map_err(|e| into_strider_err(anyhow::anyhow!(e)))
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

    /// Decode LINEARLY from `entry`, one machine instruction at a time,
    /// replaying context-register state as a real lift would, until the
    /// cursor reaches `addr`; return that instruction's p-code (ops joined
    /// `"; "`, empty for an instruction that lifts to none, e.g.
    /// `endbr64`).
    ///
    /// A stand-alone sweep, so it works for an `addr` outside any analysed
    /// CFG.  It does NOT follow control flow: `addr` must be reachable via
    /// the linear stream from `entry`.
    ///
    /// Raises `StriderError` if `addr < entry`, or if the sweep steps PAST
    /// `addr` without landing on it (not an instruction boundary on the
    /// linear path).
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
                     {addr:#x} (misaligned — {addr:#x} is not a machine-instruction \
                     boundary on the linear path from entry)"
                )));
            }
            cur = next;
        }
    }

    /// Start the interactive explorer for `target`, a `Function` or a
    /// `Cfg`.  Prints the local URL to stdout and BLOCKS serving requests
    /// on this thread until interrupted.  `host`/`port`/`depth` mirror
    /// `explore.visualize`'s kwargs (`port=0` picks an ephemeral port).
    ///
    /// **Calling this off the main thread requires `explore.shutdown`.**
    /// A thread still parked in this call when the interpreter exits aborts
    /// the process, so stop the server with
    /// `strider.explore.shutdown(port)` and join the thread before exiting.
    /// A `Function`/`Cfg` created INSIDE such a thread must not outlive it
    /// either; it cannot be dropped from another thread and leaks with an
    /// unraisable warning.
    #[pyo3(signature = (target, host="127.0.0.1".to_string(), port=0, depth=5))]
    fn visualize(
        slf: Py<Self>,
        py: Python<'_>,
        target: Py<PyAny>,
        host: String,
        port: u16,
        depth: usize,
    ) -> PyResult<()> {
        let explore = py.import_bound("strider.explore")?;
        let kwargs = pyo3::types::PyDict::new_bound(py);
        kwargs.set_item("host", host)?;
        kwargs.set_item("port", port)?;
        kwargs.set_item("depth", depth)?;
        explore.call_method("visualize", (slf, target), Some(&kwargs))?;
        Ok(())
    }
}

/// Construct a lift+optimise+resolve handle for `arch`.  `mem` backs
/// instruction fetch (a `BufferReader` or `MemReader` subclass); `rom`,
/// if given, is folded by the `LoadReadOnly` pass.
///
/// The calling convention is NOT fixed here: it is a required argument of
/// every `analyze` call.
///
/// For an ELF prefer `strider.load_elf(path)`.
///
/// Raises `StriderError` on Sleigh-construction failure.
#[pyfunction]
#[pyo3(name = "lifter", signature = (arch, mem, rom = None))]
pub fn lifter(arch: PySleighArch, mem: MemInput, rom: Option<MemInput>) -> PyResult<PyLifter> {
    let py_deps = collect_py_deps(&mem, rom.as_ref());
    Ok(PyLifter {
        inner: build_strider(arch, mem, rom)?,
        py_deps,
    })
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyLifter>()?;
    m.add_class::<PyAnalyzeResult>()?;
    m.add_function(wrap_pyfunction!(lifter, m)?)?;
    Ok(())
}
