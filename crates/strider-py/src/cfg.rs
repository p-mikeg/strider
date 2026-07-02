//! `PyCfg` — wraps `strider_cfg::Cfg` and exposes dot rendering.
//!
//! A `Cfg` is built by `Lifter.build_cfg`, which borrows the `Lifter`'s
//! owned `Sleigh` mutably for the duration of the build.  The `Cfg` is a
//! pure data structure and does not own the Sleigh; `PyCfg` keeps a
//! shared `Py<PyLifter>` handle (the `Lifter` that built it) and borrows
//! the owned Sleigh from it on demand for dot rendering and register-name
//! resolution.

use std::path::Path;

use pyo3::prelude::*;

use crate::dot::dot_style_for;
use crate::errors::into_strider_err;
use crate::reader::AnyMemReader;
use crate::strider_cls::PyLifter;

/// Control-flow graph of a single function, produced by `Lifter.build_cfg`.
/// Renderable to Graphviz dot / dark-themed HTML for inspection.
#[pyclass(name = "Cfg", module = "strider")]
pub struct PyCfg {
    inner: strider_cfg::Cfg,
    /// Shared handle to the `Lifter` that built `inner`.  The `Cfg` is a
    /// pure data structure and does not own the Sleigh; the `Lifter` owns
    /// it.  Dot rendering and the IR lift (`Lifter.analyze_cfg`) borrow
    /// the Sleigh through this handle to resolve register names.
    lifter: Py<PyLifter>,
}

impl PyCfg {
    /// Borrow the parent `Lifter` and run `f` with the `Lifter`'s owned
    /// `rsleigh::Sleigh`.
    fn with_sleigh<R>(
        &self,
        py: Python<'_>,
        f: impl FnOnce(&rsleigh::Sleigh<AnyMemReader>) -> PyResult<R>,
    ) -> PyResult<R> {
        let lifter_borrow = self.lifter.borrow(py);
        f(lifter_borrow.inner.sleigh())
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
            let d = dot::GraphDot::new(
                self.inner.dot_dumper(sleigh),
                dot_style_for(Some("dark_cfg")),
            );
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
    Ok(())
}
