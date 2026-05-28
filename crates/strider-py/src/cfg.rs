//! `PyCfg` — wraps `strider_lift::cfg::Cfg` and exposes dot rendering.
//!
//! `build_cfg` consumes the inner `Sleigh` of its `PySleigh` argument
//! (the Sleigh moves into `strider_lift::cfg::Builder`, which hands it
//! back from `build()`; `PyCfg` keeps it in its own `sleigh` field since
//! the `Cfg` itself no longer owns it).  The PySleigh wrapper is left "empty" — `inner = None` —
//! after a successful build.  Subsequent uses of the same PySleigh after
//! `build_cfg` will raise `StriderError("Sleigh already in use")`.  `Sleigh.regs`
//! was eagerly cached at construction time so callers can still build
//! a `Strider` from the same PySleigh after `build_cfg`.

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
    /// The Sleigh handle that built `inner`.  The `Cfg` is a pure data
    /// structure and no longer owns it; we keep it here so dot rendering
    /// and the IR lift (`Strider.analyze`) can resolve register names.
    pub(crate) sleigh: rsleigh::Sleigh<AnyMemReader>,
}

/// Build a control-flow graph for the function at `entry`.
///
/// Consumes the inner `Sleigh` of the `sleigh` argument (it moves into
/// the cfg builder); the `Sleigh` wrapper is left "in use" afterwards
/// and reusing it raises `StriderError`.  Installs strider-analyze's
/// indirect-branch resolver so `BranchIndirect` sites are classified at
/// cfg time.
///
/// Args:
///     sleigh: A `Sleigh` built for the target arch + memory.
///     entry: Address of the function to analyse.
///     allow_code_before_start_addr: Permit lifting instructions before
///         `entry` (default `False`).
///     function_max_size: Optional byte bound on how far past `entry`
///         the lifter may decode.
///
/// Raises `StriderError` on a lift failure or if the Sleigh is already in use.
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
    let (inner, sleigh) = strider_lift::cfg::Builder::for_arch(&arch, inner_sleigh, entry, opts)
        .with_indirect_resolver(resolver)
        .build()
        .map_err(into_strider_err)?;

    Ok(PyCfg { inner, sleigh })
}

#[pymethods]
impl PyCfg {
    /// Render the CFG to a standalone HTML file at `path`.  `style`
    /// selects the dot theme (default `"dark_cfg"`).
    #[pyo3(signature = (path, style=None))]
    fn to_html(&self, path: &str, style: Option<&str>) -> PyResult<()> {
        let style = style.unwrap_or("dark_cfg");
        let d = dot::GraphDot::new(self.inner.dot_dumper(&self.sleigh), dot_style_for(Some(style)));
        d.dump_as_html(Path::new(path)).map_err(into_strider_err)
    }
    /// Render the CFG to a Graphviz `.dot` file at `path`.
    #[pyo3(signature = (path,))]
    fn to_dot(&self, path: &str) -> PyResult<()> {
        let d = dot::GraphDot::new(self.inner.dot_dumper(&self.sleigh), dot_style_for(Some("dark_cfg")));
        d.dump_as_dot(Path::new(path)).map_err(into_strider_err)
    }
    /// Return the CFG rendered as an HTML string (default `"dark_cfg"`
    /// style) instead of writing it to a file.
    #[pyo3(signature = (style=None))]
    fn html_str(&self, style: Option<&str>) -> PyResult<String> {
        let style = style.unwrap_or("dark_cfg");
        let d = dot::GraphDot::new(self.inner.dot_dumper(&self.sleigh), dot_style_for(Some(style)));
        d.as_html_from_dot().map_err(into_strider_err)
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCfg>()?;
    m.add_function(wrap_pyfunction!(build_cfg, m)?)?;
    Ok(())
}
