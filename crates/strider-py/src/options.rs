//! Python opts structs mirroring `strider_cfg::CfgOptions` and
//! `strider_lift::LiftOptions`, with the same `cfg` nesting.

use std::collections::HashMap;

use pyo3::prelude::*;

use crate::cc::PyCallingConvention;
use crate::opt::PyOptimizerPipeline;
use crate::strider_cls::{parse_alias_mode, reject_zero_max_size};

/// CFG-shaping knobs, keyword-only.  (The orchestrator-internal
/// `known_targets` feedback field is deliberately not exposed: the
/// indirect-branch resolution loop inside `analyze` owns it.)
#[pyclass(name = "CfgOptions", module = "strider.cfg")]
#[derive(Clone, Copy, Default)]
pub struct PyCfgOptions {
    /// When set, an unconditional branch targeting at or past
    /// `entry + function_max_size` is treated as a tail call, bounding the
    /// lift. Must be `> 0`.
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
    /// Raises `ValueError` for `function_max_size=0`; omit the argument
    /// for an unbounded lift.
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

/// Lift, optimize, and CFG knobs for one `analyze` call.
///
/// Raises `ValueError` for an unrecognised `alias_mode`, or for a nested
/// `function_max_size=0`.
#[pyclass(name = "LifterOptions", module = "strider.lift")]
pub struct PyLifterOptions {
    /// Nested CFG-shape knobs.
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
    /// SP-aware alias precision for every memory pass.
    /// `"stack_global_disjoint"` (default) trusts that stack and
    /// global/constant memory never overlap; `"strict"` is the
    /// always-sound floor.
    #[pyo3(get)]
    pub alias_mode: String,
    /// When set, `analyze` runs this pipeline instead of the built-in
    /// default, for this call only.
    #[pyo3(get)]
    pub pipeline: Option<Py<PyOptimizerPipeline>>,
}

impl PyLifterOptions {
    /// All-defaults fallback for `opts=None`.  The nested `CfgOptions` is
    /// fresh too, never a shared mutable instance.
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
        // Eager, so a bad `alias_mode` fails here rather than deep inside
        // `analyze`.
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

    /// These options with `cfg` replaced and every other field carried over.
    ///
    /// The supported way to override the nested `CfgOptions`, since the fields
    /// are read-only. Rebuilding `LifterOptions(...)` by hand silently drops
    /// any option added later back to its default at that call site.
    fn with_cfg(&self, py: Python<'_>, cfg: Py<PyCfgOptions>) -> Self {
        Self {
            cfg,
            compact: self.compact,
            per_address_ccs: self.per_address_ccs.clone(),
            calls_clobber: self.calls_clobber,
            assume_distinct_sp_bases_disjoint: self.assume_distinct_sp_bases_disjoint,
            alias_mode: self.alias_mode.clone(),
            pipeline: self.pipeline.as_ref().map(|p| p.clone_ref(py)),
        }
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

pub fn register_cfg(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCfgOptions>()?;
    Ok(())
}

pub fn register_lift(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyLifterOptions>()?;
    Ok(())
}
