//! `CfgOptions` / `LifterOptions` — Python opts structs that mirror the
//! Rust `strider_cfg::CfgOptions` / `strider_lift::LiftOptions` (nested
//! `cfg`), replacing the kwargs pile that used to sit directly on
//! `Lifter.build_cfg` / `Lifter.analyze`.  See
//! `docs/superpowers/specs/2026-07-03-py-opts-pipelines-design.md`.
//!
//! `LifterOptions.pipeline`, when set, overrides the optimizer pipeline
//! `Lifter.analyze` runs for THAT call only (never on `strider.lifter(...)`
//! itself) — the per-function override the design calls for.

use std::collections::HashMap;

use pyo3::prelude::*;

use crate::cc::PyCallingConvention;
use crate::opt::PyOptimizerPipeline;
use crate::strider_cls::{parse_alias_mode, reject_zero_max_size};

/// Mirrors `strider_cfg::CfgOptions` (the user-facing subset — the
/// orchestrator-internal `known_targets` feedback field is not exposed
/// to Python; it's populated purely by the indirect-branch resolution
/// loop inside `analyze`).
///
/// Construct with keyword-only arguments; every field defaults to the
/// Rust struct's own default (`function_max_size=None`,
/// `allow_code_before_start_addr=False`).
#[pyclass(name = "CfgOptions", module = "strider")]
#[derive(Clone, Copy, Default)]
pub struct PyCfgOptions {
    /// When set, any unconditional branch whose target lies at or past
    /// `entry + function_max_size` is treated as a tail call (bounds the
    /// lift). Must be `> 0` — `CfgOptions(function_max_size=0)` raises
    /// `ValueError`.
    #[pyo3(get)]
    pub function_max_size: Option<u64>,
    /// When `False` (the default), an unconditional branch targeting an
    /// address *below* the function start is treated as a tail call.
    /// When `True`, such branches are followed normally.
    #[pyo3(get)]
    pub allow_code_before_start_addr: bool,
}

#[pymethods]
impl PyCfgOptions {
    /// Raises `ValueError` for `function_max_size=0` (zero is meaningless
    /// — omit the argument for unbounded).
    #[new]
    #[pyo3(signature = (*, function_max_size = None, allow_code_before_start_addr = false))]
    fn new(function_max_size: Option<u64>, allow_code_before_start_addr: bool) -> PyResult<Self> {
        reject_zero_max_size(function_max_size)?;
        Ok(Self {
            function_max_size,
            allow_code_before_start_addr,
        })
    }

    fn __repr__(&self) -> String {
        format!(
            "CfgOptions(function_max_size={:?}, allow_code_before_start_addr={})",
            self.function_max_size, self.allow_code_before_start_addr
        )
    }
}

/// Mirrors `strider_lift::LiftOptions` (nested `cfg`, exactly like the
/// Rust struct) plus the optimize-side knobs historically flattened into
/// `analyze(**kwargs)`, plus the per-function optimizer-pipeline
/// override `pipeline`.
///
/// Raises `ValueError` for an unrecognised `alias_mode` (see
/// `strider_cls::parse_alias_mode`) or a nested `function_max_size=0`
/// (raised by `CfgOptions` itself).
#[pyclass(name = "LifterOptions", module = "strider")]
pub struct PyLifterOptions {
    /// The nested `CfgOptions` controlling CFG shape (`function_max_size`,
    /// `allow_code_before_start_addr`) — mirrors `strider_lift::LiftOptions.cfg`.
    #[pyo3(get)]
    pub cfg: Py<PyCfgOptions>,
    /// Compact the IR arena after analysis (default `True`).
    #[pyo3(get)]
    pub compact: bool,
    /// Per-target-address calling-convention overrides (preset or custom
    /// CCs accepted); `None`/omitted means no overrides.
    #[pyo3(get)]
    pub per_address_ccs: Option<HashMap<u64, PyCallingConvention>>,
    /// Treat a call on a stack-arg load's memory chain as shadowing the
    /// slot (default `False`).
    #[pyo3(get)]
    pub calls_clobber: bool,
    /// Assume a store rooted at a different SP base than the entry SP is
    /// disjoint from the incoming-arg slots (default `False`).
    #[pyo3(get)]
    pub assume_distinct_sp_bases_disjoint: bool,
    /// SP-aware alias precision for every memory pass —
    /// `"stack_global_disjoint"` (default) trusts that stack and
    /// global/constant memory never overlap; `"strict"` is the
    /// always-sound floor.
    #[pyo3(get)]
    pub alias_mode: String,
    /// Per-function optimizer-pipeline override: when set, `analyze` runs
    /// THIS `OptimizerPipeline` instead of the built-in default, for this
    /// call only. `None` (the default) means "run the built-in default".
    #[pyo3(get)]
    pub pipeline: Option<Py<PyOptimizerPipeline>>,
}

impl PyLifterOptions {
    /// Build a fresh all-defaults `LifterOptions` (a nested fresh
    /// `CfgOptions` too — never a shared mutable instance). Used as the
    /// `opts=None` sentinel fallback in `Lifter.analyze` /
    /// `ElfLifter.analyze`.
    pub(crate) fn new_default(py: Python<'_>) -> PyResult<Self> {
        Self::new(
            py,
            None,
            true,
            None,
            false,
            false,
            "stack_global_disjoint",
            None,
        )
    }
}

#[pymethods]
impl PyLifterOptions {
    #[new]
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (
        *,
        cfg = None,
        compact = true,
        per_address_ccs = None,
        calls_clobber = false,
        assume_distinct_sp_bases_disjoint = false,
        alias_mode = "stack_global_disjoint",
        pipeline = None,
    ))]
    fn new(
        py: Python<'_>,
        cfg: Option<Py<PyCfgOptions>>,
        compact: bool,
        per_address_ccs: Option<HashMap<u64, PyCallingConvention>>,
        calls_clobber: bool,
        assume_distinct_sp_bases_disjoint: bool,
        alias_mode: &str,
        pipeline: Option<Py<PyOptimizerPipeline>>,
    ) -> PyResult<Self> {
        // Validate eagerly so a bad `alias_mode` fails at construction,
        // not silently deferred until `analyze` runs.
        parse_alias_mode(alias_mode)?;
        let cfg = match cfg {
            Some(c) => c,
            // Fresh nested default, never a shared instance.
            None => Py::new(py, PyCfgOptions::default())?,
        };
        Ok(Self {
            cfg,
            compact,
            per_address_ccs,
            calls_clobber,
            assume_distinct_sp_bases_disjoint,
            alias_mode: alias_mode.to_string(),
            pipeline,
        })
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        let cfg_repr = self.cfg.borrow(py).__repr__();
        Ok(format!(
            "LifterOptions(cfg={}, compact={}, per_address_ccs={}, calls_clobber={}, \
             assume_distinct_sp_bases_disjoint={}, alias_mode={:?}, pipeline={})",
            cfg_repr,
            self.compact,
            if self.per_address_ccs.is_some() {
                "<...>"
            } else {
                "None"
            },
            self.calls_clobber,
            self.assume_distinct_sp_bases_disjoint,
            self.alias_mode,
            if self.pipeline.is_some() {
                "<...>"
            } else {
                "None"
            },
        ))
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCfgOptions>()?;
    m.add_class::<PyLifterOptions>()?;
    Ok(())
}
