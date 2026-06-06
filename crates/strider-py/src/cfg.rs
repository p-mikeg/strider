//! `PyCfg` — wraps `strider_lift::cfg::Cfg` and exposes dot rendering.
//!
//! `build_cfg` borrows the inner `Sleigh` of its `PySleigh` argument
//! mutably for the duration of the build (the cfg builder takes a
//! `&mut Sleigh`); the wrapper stays usable across builds and any
//! other consumer with no "in use" bookkeeping.  `PyCfg` keeps a shared
//! `Py<PySleigh>` handle (the same wrapper the caller passed in) and
//! borrows the Sleigh from it on demand for dot rendering and
//! register-name resolution.

use std::path::Path;

use pyo3::prelude::*;

use crate::dot::dot_style_for;
use crate::errors::into_strider_err;
use crate::reader::AnyMemReader;
use crate::sleigh::PySleigh;

/// Control-flow graph of a single function, produced by `build_cfg`.
/// Renderable to Graphviz dot / dark-themed HTML for inspection.
#[pyclass(name = "Cfg", module = "strider")]
pub struct PyCfg {
    pub(crate) inner: strider_lift::cfg::Cfg,
    /// Shared handle to the `PySleigh` that built `inner`.  The `Cfg` is
    /// a pure data structure and no longer owns the Sleigh; `build_cfg`
    /// puts the Sleigh back into this wrapper so the caller can keep
    /// using it.  Dot rendering and the IR lift (`Strider.analyze`)
    /// borrow the Sleigh through this handle to resolve register names.
    pub(crate) sleigh: Py<PySleigh>,
}

/// Build a control-flow graph for the function at `entry`.
///
/// Borrows the inner `Sleigh` of the `sleigh` argument mutably for the
/// duration of the build; the `Sleigh` object stays usable afterwards
/// for the next CFG build, IR lift, dot rendering, etc.  The returned
/// `Cfg` keeps a shared handle to that same `sleigh` wrapper for dot
/// rendering.  The low-level `build_cfg` does no indirect-branch
/// resolution: every `BranchIndirect` is left as an
/// `UnresolvedIndirectBranch` terminator.  Indirect-branch resolution
/// happens only in the high-level `strider.run` orchestrator, whose
/// rebuild-driven fixed-point loop classifies each site against the
/// optimised IR.
///
/// Args:
///     sleigh: A `Sleigh` built for the target arch + memory.
///     entry: Address of the function to analyse.
///     allow_code_before_start_addr: Permit lifting instructions before
///         `entry` (default `False`).
///     function_max_size: Optional byte bound on how far past `entry`
///         the lifter may decode.
///
/// Raises `StriderError` on a lift failure.
#[pyfunction(signature = (sleigh, entry, allow_code_before_start_addr=false, function_max_size=None))]
pub fn build_cfg(
    py: Python<'_>,
    sleigh: Py<PySleigh>,
    entry: u64,
    allow_code_before_start_addr: bool,
    function_max_size: Option<u64>,
) -> PyResult<PyCfg> {
    let opts = strider_lift::LiftOptions {
        allow_code_before_start_addr,
        fn_max_size: function_max_size,
        ..strider_lift::LiftOptions::default()
    };

    let inner = {
        let mut sleigh_borrow = sleigh.borrow_mut(py);
        let arch = sleigh_borrow.arch;

        // Use `for_arch` so the CallOther classifier sees the actual arch
        // preset.  (Earlier `Builder::new` ctors silently defaulted to
        // `X86_64` and mis-classified arch-specific user-ops on non-x86
        // targets; that ctor is no longer exposed.)
        //
        // No indirect-branch resolver is installed: the low-level
        // `build_cfg` leaves every `BranchIndirect` as an
        // `UnresolvedIndirectBranch`.  Resolution is the high-level
        // `strider.run` orchestrator's job.
        strider_lift::cfg::Builder::for_arch(&arch, &mut sleigh_borrow.inner, entry, &opts)
            .build()
            .map_err(into_strider_err)?
    };

    Ok(PyCfg { inner, sleigh })
}

impl PyCfg {
    /// Borrow the shared `Sleigh` handle and run `f` with the inner
    /// `rsleigh::Sleigh`.
    fn with_sleigh<R>(
        &self,
        py: Python<'_>,
        f: impl FnOnce(&rsleigh::Sleigh<AnyMemReader>) -> PyResult<R>,
    ) -> PyResult<R> {
        let sleigh_borrow = self.sleigh.borrow(py);
        f(&sleigh_borrow.inner)
    }
}

#[pymethods]
impl PyCfg {
    /// Render the CFG to a standalone HTML file at `path`.  `style`
    /// selects the dot theme (default `"dark_cfg"`).
    #[pyo3(signature = (path, style=None))]
    fn to_html(&self, py: Python<'_>, path: &str, style: Option<&str>) -> PyResult<()> {
        let style = style.unwrap_or("dark_cfg");
        self.with_sleigh(py, |sleigh| {
            let d = dot::GraphDot::new(self.inner.dot_dumper(sleigh), dot_style_for(Some(style)));
            d.dump_as_html(Path::new(path)).map_err(into_strider_err)
        })
    }
    /// Render the CFG to a Graphviz `.dot` file at `path`.
    #[pyo3(signature = (path,))]
    fn to_dot(&self, py: Python<'_>, path: &str) -> PyResult<()> {
        self.with_sleigh(py, |sleigh| {
            let d =
                dot::GraphDot::new(self.inner.dot_dumper(sleigh), dot_style_for(Some("dark_cfg")));
            d.dump_as_dot(Path::new(path)).map_err(into_strider_err)
        })
    }
    /// Return the CFG rendered as an HTML string (default `"dark_cfg"`
    /// style) instead of writing it to a file.
    #[pyo3(signature = (style=None))]
    fn html_str(&self, py: Python<'_>, style: Option<&str>) -> PyResult<String> {
        let style = style.unwrap_or("dark_cfg");
        self.with_sleigh(py, |sleigh| {
            let d = dot::GraphDot::new(self.inner.dot_dumper(sleigh), dot_style_for(Some(style)));
            d.as_html_from_dot().map_err(into_strider_err)
        })
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCfg>()?;
    m.add_function(wrap_pyfunction!(build_cfg, m)?)?;
    Ok(())
}
