//! `PyGraph` — wraps `ir::BuiltFunctionGraph` and exposes dot
//! rendering plus (in later tasks) pattern queries and rewrites.
//!
//! The IR graph's dot dumper requires a borrowed `Sleigh` for
//! register-name resolution.  PyGraph keeps a `Py<PyCfg>` reference
//! so the Sleigh stays alive for the graph's lifetime and is
//! reachable through `cfg::Cfg::sleigh`.

use std::sync::{Arc, RwLock};

use pyo3::prelude::*;

use crate::cfg::PyCfg;
use crate::dot::{dot_style_for, dump_dot, dump_html, html_str};

/// Opaque wrapper over `ir::BuiltFunctionGraph`.
///
/// The graph is held in `Arc<RwLock<...>>` so optimization passes
/// (added in phase 3) can mutate it without requiring `&mut self` on
/// the PyGraph wrapper, and so the same graph can be shared across
/// multiple Python references.
#[pyclass(name = "Graph", module = "strider")]
pub struct PyGraph {
    pub(crate) inner: Arc<RwLock<ir::BuiltFunctionGraph>>,
    /// Strong reference to the parent Cfg; keeps the Sleigh alive for
    /// dot rendering and ensures destruction order is graph-then-cfg.
    pub(crate) cfg: Py<PyCfg>,
}

impl PyGraph {
    pub(crate) fn new(graph: ir::BuiltFunctionGraph, cfg: Py<PyCfg>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(graph)),
            cfg,
        }
    }

    /// Borrow the inner graph for read.  Returns an `anyhow::Error`
    /// when the lock is poisoned.
    #[allow(dead_code)]
    pub(crate) fn read_inner(&self) -> anyhow::Result<std::sync::RwLockReadGuard<'_, ir::BuiltFunctionGraph>> {
        self.inner
            .read()
            .map_err(|_| anyhow::anyhow!("Graph lock poisoned"))
    }

    /// Borrow the inner graph for write.  Returns an `anyhow::Error`
    /// when the lock is poisoned.
    #[allow(dead_code)]
    pub(crate) fn write_inner(&self) -> anyhow::Result<std::sync::RwLockWriteGuard<'_, ir::BuiltFunctionGraph>> {
        self.inner
            .write()
            .map_err(|_| anyhow::anyhow!("Graph lock poisoned"))
    }
}

#[pymethods]
impl PyGraph {
    #[pyo3(signature = (path, style=None))]
    fn to_html(&self, py: Python<'_>, path: &str, style: Option<&str>) -> PyResult<()> {
        let style = style.unwrap_or("dark");
        let cfg_borrow = self.cfg.borrow(py);
        let graph = self
            .inner
            .read()
            .map_err(|_| crate::errors::into_strider_err(anyhow::anyhow!("Graph lock poisoned")))?;
        let dumper = graph.dot_dumper(&cfg_borrow.inner.sleigh);
        let d = dot::GraphDot::new(dumper, dot_style_for(Some(style)));
        dump_html(&d, path)
    }

    #[pyo3(signature = (path,))]
    fn to_dot(&self, py: Python<'_>, path: &str) -> PyResult<()> {
        let cfg_borrow = self.cfg.borrow(py);
        let graph = self
            .inner
            .read()
            .map_err(|_| crate::errors::into_strider_err(anyhow::anyhow!("Graph lock poisoned")))?;
        let dumper = graph.dot_dumper(&cfg_borrow.inner.sleigh);
        let d = dot::GraphDot::new(dumper, dot_style_for(Some("dark")));
        dump_dot(&d, path)
    }

    #[pyo3(signature = (style=None))]
    fn html_str(&self, py: Python<'_>, style: Option<&str>) -> PyResult<String> {
        let style = style.unwrap_or("dark");
        let cfg_borrow = self.cfg.borrow(py);
        let graph = self
            .inner
            .read()
            .map_err(|_| crate::errors::into_strider_err(anyhow::anyhow!("Graph lock poisoned")))?;
        let dumper = graph.dot_dumper(&cfg_borrow.inner.sleigh);
        let d = dot::GraphDot::new(dumper, dot_style_for(Some(style)));
        html_str(&d)
    }

    fn node_count(&self) -> PyResult<usize> {
        let graph = self
            .inner
            .read()
            .map_err(|_| crate::errors::into_strider_err(anyhow::anyhow!("Graph lock poisoned")))?;
        Ok(graph.all_node_ids().count())
    }

    /// Apply a `PyOptimizerPipeline` to this graph in place.  Drains
    /// the pipeline (subsequent calls to the same pipeline see an
    /// empty pass list); rebuild it from `OptimizerPipeline.default()`
    /// or the equivalent classmethods if you need to apply it again.
    fn optimize(&self, pipeline: &crate::opt::PyOptimizerPipeline) -> PyResult<()> {
        let real_pipeline = pipeline.drain_into_pipeline()?;
        let mut graph = self
            .inner
            .write()
            .map_err(|_| crate::errors::into_strider_err(anyhow::anyhow!("Graph lock poisoned")))?;
        real_pipeline
            .run_on_built(&mut graph)
            .map_err(|e| crate::errors::into_strider_err(anyhow::anyhow!("optimize failed: {e:?}")))
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyGraph>()
}
