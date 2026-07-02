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

use std::path::Path;

use pyo3::prelude::*;
use strider_orchestrator::opt::AliasMode;

use crate::arch::PySleighArch;
use crate::cc::PyCallingConvention;
use crate::cfg::PyCfg;
use crate::dot::dot_style_for;
use crate::errors::into_strider_err;
use crate::function::PyFunction;
use crate::matcher::PyMatch;
use crate::node::PyNode;
use crate::options::{PyCfgOptions, PyLifterOptions};
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
    ///
    /// Also the base `fingerprint_pcode` clones from to mint a fresh,
    /// throwaway `Sleigh` per call (`AnyMemReader: Clone` makes
    /// `Sleigh<AnyMemReader>: Clone` build a brand-new engine instance
    /// from the same `(sla_spec, pspec)` over a cloned reader) — see
    /// that method's doc for why it must NOT lift through this
    /// persistent instance directly.
    pub(crate) fn sleigh(&self) -> &rsleigh::Sleigh<AnyMemReader> {
        self.inner.sleigh()
    }

    /// Build a `GraphDot` over `function`'s IR through this Lifter's own
    /// Sleigh and dispatch to `op`.  Centralises the borrow / dumper-
    /// construction ritual shared by `dump_html` / `dump_dot` /
    /// `html_str` — the Sleigh-needing pretty renders that moved here
    /// from `Function` (a bare `Function` has no Sleigh to resolve
    /// register names with).
    fn dispatch_dot(
        &self,
        function: &PyFunction,
        style: Option<&str>,
        op: DotOp<'_>,
    ) -> PyResult<DotResult> {
        let sleigh = self.sleigh();
        let guard = function.read_inner().map_err(into_strider_err)?;
        let dumper = guard.dot_dumper(sleigh).map_err(into_strider_err)?;
        let d = dot::GraphDot::new(dumper, dot_style_for(style));
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
        }
    }
}

/// Discriminator for [`PyLifter::dispatch_dot`].  Each variant carries
/// the per-op arguments the public accessor `dump_html` / `dump_dot` /
/// `html_str` would otherwise duplicate the sleigh-borrow / dumper-
/// construction ritual for.
enum DotOp<'a> {
    DumpHtml(&'a str),
    DumpDot(&'a str),
    HtmlStr,
}

/// Return shape of [`PyLifter::dispatch_dot`].  Returning a sum lets a
/// single helper cover both unit-returning dump methods and the
/// string-returning `html_str` without separate variants per dispatch.
enum DotResult {
    Unit,
    Html(String),
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
    /// `opts` (a `CfgOptions`, default all-defaults) mirrors
    /// `strider_cfg::CfgOptions`.  `StriderError` on a build failure.
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
        Ok(PyCfg { inner, lifter: slf })
    }

    /// Lift the function at `entry`, optimise it to a fixed point,
    /// resolve its indirect branches, and return `(function,
    /// unresolved_addrs)`.
    ///
    /// `cc` is the function-default calling convention for THIS call
    /// (the handle stores no default); per-target-address overrides are
    /// supplied via `opts.per_address_ccs` (preset or custom CCs accepted).
    ///
    /// Args:
    ///     entry: Address of the function to analyse.
    ///     cc: Calling convention for this analysis.
    ///     opts: A `LifterOptions` (default all-defaults) mirroring
    ///         `strider_lift::LiftOptions` plus the optimize-side knobs and
    ///         the per-function `pipeline` override.  When `opts.pipeline`
    ///         is set, THAT `OptimizerPipeline` runs instead of the
    ///         built-in default, for this call only.
    ///
    /// Raises `ValueError` for a nested `function_max_size == 0` or an
    /// unrecognised `alias_mode` (both raised eagerly by the `CfgOptions`/
    /// `LifterOptions` constructors), and `StriderError` on lift/analysis
    /// failure.
    #[pyo3(signature = (entry, cc, opts=None))]
    fn analyze(
        slf: Py<Self>,
        py: Python<'_>,
        entry: u64,
        cc: PyCallingConvention,
        opts: Option<Py<PyLifterOptions>>,
    ) -> PyResult<(Py<PyFunction>, Vec<u64>)> {
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
        // Materialise the per-function pipeline override (if any) BEFORE
        // dropping the GIL below; `None` means "run the built-in default".
        let custom_pipeline = opts_ref
            .pipeline
            .as_ref()
            .map(|p| p.borrow(py).drain_into_pipeline(false))
            .transpose()?;
        drop(opts_ref);

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
        // Sleigh + rom + cached regs for the whole run) — but only for the
        // default (no custom pipeline) path: `custom_pipeline`'s boxed
        // `dyn Optimizer`/`dyn PostOptimizer` trait objects don't implement
        // `Send` (no `Send` bound on those traits), so a closure that
        // *captures* it fails `allow_threads`'s `Ungil` bound even though
        // every concrete pass inside is Python-callback-free and would be
        // sound to move across the release. Rather than force `Send` onto
        // the trait objects, keep the GIL held for a custom-pipeline run —
        // it's the uncommon path, and correctness here is simpler than
        // threading a `Send` bound through `strider-opt`'s dyn traits.
        let result = {
            let mut lifter = slf.borrow_mut(py);
            // Reborrow the inner `Strider` as a plain reference BEFORE the
            // closure: capturing `lifter.inner` directly (rather than
            // `lifter` itself) keeps the closure's captured type a plain
            // `&mut Strider<AnyMemReader>` instead of the GIL-bound
            // `PyRefMut`, which embeds a `!Send` `Python<'_>` marker that
            // would otherwise fail `allow_threads`'s `Ungil` bound.
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

    /// Run an optimizer pipeline over `function`'s IR in place.
    ///
    /// `pipeline=None` (the default) builds and runs the canonical
    /// default pipeline — the same one `analyze` drives internally
    /// (`strider_orchestrator::opt::default_pipeline()`), equivalent to
    /// the former `Function.reoptimize()`.  Passing an `OptimizerPipeline`
    /// runs THAT pipeline instead (draining it — rebuild before reuse),
    /// equivalent to the former `Function.optimize(pipeline)`.
    ///
    /// Useful after a manual `Function.rewrite(...)` / `rewrite_all(...)`
    /// to re-converge the graph, or to layer extra passes on top of an
    /// already-analyzed function.  Mutates `function` in place and bumps
    /// its generation, invalidating outstanding `Node`/`Match` handles —
    /// see `PyFunction::run_pipeline_in_place`.
    ///
    /// A custom `pipeline` runs without a rom image (`OptCtx::new(None)`);
    /// any `LoadReadOnly` pass present short-circuits silently.  Callers
    /// that need rom-driven folding should route through
    /// `strider.lifter(arch, mem, rom=mem).analyze(...)` (or
    /// `strider.load_elf(...)`, which wires the rom automatically)
    /// instead.
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

    /// Render `function`'s IR graph to a standalone HTML file at `path`.
    /// `style` selects the dot theme (default `"dark"`).
    ///
    /// Lives on `Lifter` (not `Function`) because the pretty renderer
    /// inlines constants / adds virtual nodes / resolves register names,
    /// all of which need a `Sleigh` — a bare `Function` doesn't carry
    /// one, but the `Lifter` that produced it does.
    #[pyo3(signature = (function, path, style=None))]
    fn dump_html(&self, function: &PyFunction, path: &str, style: Option<&str>) -> PyResult<()> {
        self.dispatch_dot(function, style, DotOp::DumpHtml(path))
            .map(|_| ())
    }

    /// Render `function`'s IR graph to a Graphviz `.dot` file at `path`.
    #[pyo3(signature = (function, path))]
    fn dump_dot(&self, function: &PyFunction, path: &str) -> PyResult<()> {
        self.dispatch_dot(function, None, DotOp::DumpDot(path))
            .map(|_| ())
    }

    /// Return `function`'s IR graph rendered as an HTML string (default
    /// `"dark"` style) instead of writing it to a file.
    #[pyo3(signature = (function, style=None))]
    fn html_str(&self, function: &PyFunction, style: Option<&str>) -> PyResult<String> {
        match self.dispatch_dot(function, style, DotOp::HtmlStr)? {
            DotResult::Html(s) => Ok(s),
            DotResult::Unit => Err(into_strider_err(anyhow::anyhow!(
                "internal: DotOp::HtmlStr returned DotResult::Unit"
            ))),
        }
    }

    /// Return the asm-fingerprint of `node` as `(addr, text)` p-code
    /// pairs, sorted by address — the p-code companion to
    /// `Node.fingerprint()` (addr-only).  `node` is typically obtained
    /// via `Function.node(id)` or `Match.node(key)`, but a `Match` or a
    /// raw `u32` node id (e.g. `match.root`) are also accepted directly
    /// — the same three-way acceptance the old `Analysis.fingerprint_pcode`
    /// gave via its `_coerce_node_id` helper.  A raw int id doesn't carry
    /// its own function (a `Node`/`Match` does), so it must be paired
    /// with the explicit `function=` kwarg; omit `function` when `node`
    /// is already a `Node` or `Match`.
    ///
    /// Lives on `Lifter` (not `Function`/`Node`) because rendering an
    /// address to p-code text needs a `Sleigh`.  Builds a FRESH,
    /// throwaway `Sleigh` (cloned from this handle's own — same
    /// `sla_spec`/`pspec`, a cheap cloned reader, but a brand-new
    /// underlying engine instance) for every call rather than lifting
    /// through the Lifter's persistent Sleigh: `Sleigh::lift_one` carries
    /// context-register state across calls (ARM Thumb mode, x86 segment
    /// selectors, MIPS16 — see the module-level `rsleigh` doc in
    /// CLAUDE.md), so reusing the persistent instance would (a) lift each
    /// fingerprint address under whatever context the previous one left,
    /// since addresses are visited in sorted, not decode, order, and (b)
    /// POLLUTE the Lifter's Sleigh, corrupting subsequent `analyze()`
    /// / `build_cfg()` calls on the same handle.  The throwaway clone is
    /// discarded at the end of this call, so neither problem reaches the
    /// handle's persistent state.
    ///
    /// Returns `[]` for "structural" nodes that carry no fingerprint
    /// (Entry, InitialMemory, InitialVar, Region, phis).
    #[pyo3(signature = (node, function=None))]
    fn fingerprint_pcode(
        &mut self,
        py: Python<'_>,
        node: FingerprintNodeArg<'_>,
        function: Option<Py<PyFunction>>,
    ) -> PyResult<Vec<(u64, String)>> {
        let (function, node_id) = node.resolve(py, function)?;
        let py_node = PyNode::new(py, function, node_id)?;
        let addrs = py_node.fingerprint(py)?;
        // A fresh, independent Sleigh per call (see the doc comment
        // above) — `Sleigh::clone` builds a brand-new engine context
        // from this handle's `(sla_spec, pspec)` over a cloned reader,
        // so it starts with no inherited context-register state and its
        // mutations never reach `self.inner`'s persistent Sleigh.
        let mut sleigh = self.sleigh().clone();
        let mut out = Vec::with_capacity(addrs.len());
        for addr in addrs {
            let (text, _len) = crate::pcode::lift_one_text(&mut sleigh, addr)?;
            out.push((addr, text));
        }
        Ok(out)
    }
}

/// Polymorphic node-or-id argument for [`PyLifter::fingerprint_pcode`]: a
/// `Node` handle, a `Match` (its root node is used), or a raw `u32` node
/// id (e.g. `match.root`) — mirrors the three-way acceptance the old
/// `Analysis.fingerprint_pcode` gave via its `_coerce_node_id` helper.
#[derive(FromPyObject)]
enum FingerprintNodeArg<'py> {
    Node(PyRef<'py, PyNode>),
    Match(PyRef<'py, PyMatch>),
    Id(u32),
}

impl FingerprintNodeArg<'_> {
    /// Resolve to `(function, node_id)`.  A `Node`/`Match` already
    /// carries its own function reference; a raw `Id` has none, so it
    /// borrows the caller-supplied `function` (an error if omitted —
    /// there is no implicit "current function" on a `Lifter`, which can
    /// `analyze` many different functions over its lifetime).
    fn resolve(
        self,
        py: Python<'_>,
        function: Option<Py<PyFunction>>,
    ) -> PyResult<(Py<PyFunction>, u32)> {
        match self {
            FingerprintNodeArg::Node(n) => Ok((n.function.clone_ref(py), n.id)),
            FingerprintNodeArg::Match(m) => {
                Ok((m.function.clone_ref(py), m.inner.root().as_u32()))
            }
            FingerprintNodeArg::Id(id) => {
                let function = function.ok_or_else(|| {
                    pyo3::exceptions::PyValueError::new_err(
                        "fingerprint_pcode: a raw int node id requires function=<Function> to \
                         resolve which function's node arena it indexes (a Node or Match \
                         already carries its own function)",
                    )
                })?;
                Ok((function, id))
            }
        }
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
/// For an ELF, prefer `strider.load_elf(path)` → `ElfLifter`, which
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
