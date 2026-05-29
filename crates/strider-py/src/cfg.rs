//! `PyCfg` — wraps `strider_lift::cfg::Cfg` and exposes dot rendering.
//!
//! `build_cfg` moves the inner `Sleigh` out of its `PySleigh` argument
//! (the Sleigh moves into `strider_lift::cfg::Builder`, which hands it
//! back from `build()`), then puts that returned Sleigh BACK into the
//! caller's `PySleigh` before returning.  Net effect: the caller's
//! `Sleigh` object is usable again after `build_cfg` returns — building
//! a second CFG, a `Strider`, or any other consumer from the same
//! handle just works.  `PyCfg` does not own a `Sleigh` of its own;
//! instead it keeps a shared `Py<PySleigh>` handle (the same wrapper
//! the caller passed in) and borrows the Sleigh from it on demand for
//! dot rendering and register-name resolution.

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
/// Moves the inner `Sleigh` of the `sleigh` argument into the cfg
/// builder, then puts it back into the same `sleigh` wrapper before
/// returning — so the `Sleigh` object stays usable afterwards (build a
/// second CFG, a `Strider`, etc.).  The returned `Cfg` keeps a shared
/// handle to that same `sleigh` wrapper for dot rendering.  Installs
/// strider-analyze's indirect-branch resolver so `BranchIndirect` sites
/// are classified at cfg time.
///
/// Args:
///     sleigh: A `Sleigh` built for the target arch + memory.
///     entry: Address of the function to analyse.
///     allow_code_before_start_addr: Permit lifting instructions before
///         `entry` (default `False`).
///     function_max_size: Optional byte bound on how far past `entry`
///         the lifter may decode.
///
/// Raises `StriderError` on a lift failure or if the Sleigh is already
/// in use by another in-flight consumer.
#[pyfunction(signature = (sleigh, entry, allow_code_before_start_addr=false, function_max_size=None))]
pub fn build_cfg(
    py: Python<'_>,
    sleigh: Py<PySleigh>,
    entry: u64,
    allow_code_before_start_addr: bool,
    function_max_size: Option<u64>,
) -> PyResult<PyCfg> {
    let mut sleigh_borrow = sleigh.borrow_mut(py);
    let arch = sleigh_borrow.arch;
    let inner_sleigh = sleigh_borrow
        .take_inner()
        .ok_or_else(|| into_strider_err(anyhow::anyhow!("Sleigh already in use")))?;
    drop(sleigh_borrow);

    let mut opts_builder = strider_lift::cfg::OptionsBuilder::new();
    if allow_code_before_start_addr {
        opts_builder = opts_builder.allow_code_before_start_addr();
    }
    if let Some(max_size) = function_max_size {
        opts_builder = opts_builder.set_function_max_size(max_size);
    }
    let opts = opts_builder.build();

    // Use `for_arch` so the CallOther classifier sees the actual arch
    // preset.  (Earlier `Builder::new` ctors silently defaulted to
    // `X86_64` and mis-classified arch-specific user-ops on non-x86
    // targets; that ctor is no longer exposed.)
    //
    // Install the strider-analyze mini-IR resolver so the cfg-time
    // resolver classifies `BranchIndirect` rather than deferring every
    // site via `UnresolvedIndirectBranch`.
    let resolver: strider_lift::cfg::IndirectResolverFn<AnyMemReader> =
        std::sync::Arc::new(|insns, target_vn, sleigh, lr_vn, rom, endianness| {
            strider_analyze::indirect_resolver::resolve_indirect_target(
                insns, target_vn, sleigh, lr_vn, rom, endianness,
            )
        });
    let (inner, returned_sleigh) =
        strider_lift::cfg::Builder::for_arch(&arch, inner_sleigh, entry, opts)
            .with_indirect_resolver(resolver)
            .build()
            .map_err(into_strider_err)?;

    // Put the Sleigh the builder handed back into the caller's wrapper,
    // so the same `sleigh` object is usable again after this returns.
    sleigh.borrow_mut(py).put_inner(returned_sleigh);

    // Keep the SAME shared handle on the Cfg so dot rendering borrows
    // the (now restored) Sleigh from it.
    Ok(PyCfg { inner, sleigh })
}

impl PyCfg {
    /// Borrow the shared `Sleigh` handle and run `f` with the inner
    /// `rsleigh::Sleigh`.  Returns a `StriderError` (rather than
    /// panicking) when the Sleigh is currently moved out by some other
    /// in-flight consumer (`inner == None`).
    fn with_sleigh<R>(
        &self,
        py: Python<'_>,
        f: impl FnOnce(&rsleigh::Sleigh<AnyMemReader>) -> PyResult<R>,
    ) -> PyResult<R> {
        let sleigh_borrow = self.sleigh.borrow(py);
        let inner = sleigh_borrow
            .inner
            .as_ref()
            .ok_or_else(|| into_strider_err(anyhow::anyhow!("Sleigh is in use; cannot render CFG")))?;
        f(inner)
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
