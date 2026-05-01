//! `PyMatch` — result wrapper for a successful pattern match.
//!
//! The Rust `pattern::Matcher` borrows the BuiltFunctionGraph
//! immutably for its lifetime; we cannot store one across Python
//! method calls without an unsafe lifetime extension.  Instead each
//! call constructs a fresh `Matcher`, runs the query, and converts
//! every `pattern::Match` to a `PyMatch` that carries:
//! - The `pattern::Match` itself (for capture lookup).
//! - A `Py<PyGraph>` reference so accessors like `get_uint` can
//!   re-borrow the graph and call `Match::get_uint(c, &graph)`.
//!
//! The `Graph.find_all` / `Graph.match_at` / `Graph.matcher` entry
//! points live on PyGraph in `graph.rs`.

use pyo3::prelude::*;

use crate::errors::into_strider_err;
use crate::graph::PyGraph;
use crate::pattern::{intern_str, PyCapture};

/// Result of a successful pattern match.
#[pyclass(name = "Match", module = "strider")]
pub struct PyMatch {
    pub(crate) inner: pattern::Match,
    pub(crate) graph: Py<PyGraph>,
}

/// Polymorphic capture key: a `Capture` instance or a string name
/// (looked up in the global intern table).
#[derive(FromPyObject)]
pub enum CaptureKey<'py> {
    Capture(Bound<'py, PyCapture>),
    Str(String),
}

impl CaptureKey<'_> {
    fn resolve(self) -> PyResult<pattern::Capture> {
        match self {
            CaptureKey::Capture(c) => Ok(c.borrow().inner),
            CaptureKey::Str(s) => intern_str(s.as_str()),
        }
    }
}

#[pymethods]
impl PyMatch {
    /// `m["name"]` / `m[capture]` — best-effort: integer if value
    /// output is an int, bool if it's a bool, raw bits otherwise.
    fn __getitem__(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<PyObject> {
        let cap = key.resolve()?;
        let graph_borrow = self.graph.borrow(py);
        let g = graph_borrow.read_inner().map_err(into_strider_err)?;
        if let Some(v) = self.inner.get_uint(cap, &g) {
            // Try fitting in a Python int via i128.
            return Ok((v as i128).into_py(py));
        }
        if let Some(b) = self.inner.get_bool(cap, &g) {
            return Ok(b.into_py(py));
        }
        if let Some(f) = self.inner.get_float_bits(cap, &g) {
            return Ok(f.into_py(py));
        }
        // Fall back to None for control-flow captures.
        Ok(py.None())
    }

    fn __contains__(&self, key: CaptureKey<'_>) -> PyResult<bool> {
        let cap = key.resolve()?;
        Ok(self.inner.node(cap).is_some())
    }

    fn uint(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<u128>> {
        let cap = key.resolve()?;
        let graph_borrow = self.graph.borrow(py);
        let g = graph_borrow.read_inner().map_err(into_strider_err)?;
        Ok(self.inner.get_uint(cap, &g))
    }

    #[pyo3(name = "int")]
    fn int_(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<i128>> {
        let cap = key.resolve()?;
        let graph_borrow = self.graph.borrow(py);
        let g = graph_borrow.read_inner().map_err(into_strider_err)?;
        Ok(self.inner.get_int(cap, &g))
    }

    #[pyo3(name = "bool")]
    fn bool_(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<bool>> {
        let cap = key.resolve()?;
        let graph_borrow = self.graph.borrow(py);
        let g = graph_borrow.read_inner().map_err(into_strider_err)?;
        Ok(self.inner.get_bool(cap, &g))
    }

    fn float_bits(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<u64>> {
        let cap = key.resolve()?;
        let graph_borrow = self.graph.borrow(py);
        let g = graph_borrow.read_inner().map_err(into_strider_err)?;
        Ok(self.inner.get_float_bits(cap, &g))
    }

    /// Returns True if the capture has a binding.
    fn has(&self, key: CaptureKey<'_>) -> PyResult<bool> {
        let cap = key.resolve()?;
        Ok(self.inner.node(cap).is_some())
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyMatch>()
}
