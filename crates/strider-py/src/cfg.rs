//! `PyCfg` — wraps `strider_lift::cfg::Cfg` and exposes dot rendering.
//!
//! `build_cfg` consumes the inner `Sleigh` of its `PySleigh` argument
//! (the Sleigh moves into `strider_lift::cfg::Builder`, then into the resulting
//! `Cfg`).  The PySleigh wrapper is left "empty" — `inner = None` —
//! after a successful build.  Subsequent uses of the same PySleigh as
//! a Sleigh raise `LiftError("Sleigh already in use")`.  `Sleigh.regs`
//! was eagerly cached at construction time so callers can still build
//! a `Strider` from the same PySleigh after `build_cfg`.

use std::path::Path;

use pyo3::prelude::*;

use crate::dot::dot_style_for;
use crate::errors::{into_lift_err, into_strider_err};
use crate::reader::AnyMemReader;
use crate::sleigh::PySleigh;

#[pyclass(name = "Cfg", module = "strider")]
pub struct PyCfg {
    pub(crate) inner: strider_lift::cfg::Cfg<AnyMemReader>,
}

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
        .ok_or_else(|| into_lift_err(anyhow::anyhow!("Sleigh already in use")))?;
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
    // preset.  (`Builder::new` was deleted in round 12 W5c — it used
    // to default to `X86_64` and silently mis-classified arch-specific
    // user-ops on non-x86 targets.)
    //
    // Install the strider-analyze mini-IR resolver so the cfg-time
    // resolver classifies `BranchIndirect` rather than deferring every
    // site via `UnresolvedIndirectBranch`.  Phase 3 Task 3.1.
    let resolver: std::sync::Arc<
        dyn strider_lift::cfg::IndirectTargetResolver<AnyMemReader>,
    > = std::sync::Arc::new(strider_analyze::opt::indirect_resolver::MiniIrIndirectResolver);
    let built = strider_lift::cfg::Builder::for_arch(&arch, inner_sleigh, entry, opts)
        .with_indirect_resolver(resolver)
        .build()
        .map_err(into_lift_err)?;

    Ok(PyCfg { inner: built })
}

#[pymethods]
impl PyCfg {
    #[pyo3(signature = (path, style=None))]
    fn to_html(&self, path: &str, style: Option<&str>) -> PyResult<()> {
        let style = style.unwrap_or("dark_cfg");
        let d = strider_ir::dot::GraphDot::new(self.inner.dot_dumper(), dot_style_for(Some(style)));
        d.dump_as_html(Path::new(path)).map_err(into_strider_err)
    }
    #[pyo3(signature = (path,))]
    fn to_dot(&self, path: &str) -> PyResult<()> {
        let d = strider_ir::dot::GraphDot::new(self.inner.dot_dumper(), dot_style_for(Some("dark_cfg")));
        d.dump_as_dot(Path::new(path)).map_err(into_strider_err)
    }
    #[pyo3(signature = (style=None))]
    fn html_str(&self, style: Option<&str>) -> PyResult<String> {
        let style = style.unwrap_or("dark_cfg");
        let d = strider_ir::dot::GraphDot::new(self.inner.dot_dumper(), dot_style_for(Some(style)));
        d.as_html_from_dot().map_err(into_strider_err)
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCfg>()?;
    m.add_function(wrap_pyfunction!(build_cfg, m)?)?;
    Ok(())
}
