//! `PySleigh` — wraps a constructed `rsleigh::Sleigh` keyed off a
//! `PySleighArch` + a `PyMemoryMap`.  Holds the `Sleigh` in an `Option`
//! so it can be moved into a downstream consumer (`cfg::Builder`,
//! which takes the Sleigh by value) and then put back when the
//! consumer hands it back via `Cfg::sleigh`.

use pyo3::prelude::*;

use crate::arch::PySleighArch;
use crate::errors::into_lift_err;
use crate::reader::{PyMemoryMap, PyMemoryMapReader};

/// A constructed Sleigh keyed off a (SleighArch, MemoryMap) pair.
///
/// The inner `Sleigh` is held in an `Option` so it can be moved out
/// (into a `cfg::Builder`, for example) and put back later via
/// `put_inner`.  While the inner is `None` the wrapper is "in use" by
/// some downstream consumer; further moves fail with `LiftError`.
#[pyclass(name = "Sleigh", module = "strider")]
pub struct PySleigh {
    pub(crate) inner: Option<rsleigh::Sleigh<PyMemoryMapReader>>,
    pub(crate) arch_name: &'static str,
    /// Retained so the wrapper can be reconstructed if needed (currently
    /// unused — kept for symmetry with the planned callback-reader path
    /// added in phase 7).
    #[allow(dead_code)]
    pub(crate) arch: target::SleighArch,
}

impl PySleigh {
    /// Move the inner Sleigh out, leaving the wrapper as "in use".
    /// Returns `None` if it is already in use.
    #[allow(dead_code)]
    pub(crate) fn take_inner(&mut self) -> Option<rsleigh::Sleigh<PyMemoryMapReader>> {
        self.inner.take()
    }

    /// Restore the inner Sleigh, typically the one harvested out of
    /// `Cfg::sleigh` when a consumer hands it back.
    #[allow(dead_code)]
    pub(crate) fn put_inner(&mut self, sleigh: rsleigh::Sleigh<PyMemoryMapReader>) {
        self.inner = Some(sleigh);
    }
}

#[pymethods]
impl PySleigh {
    #[new]
    fn new(arch: PySleighArch, mem: PyMemoryMap) -> PyResult<Self> {
        let table = mem.lookup_table().map_err(into_lift_err)?;
        let reader = PyMemoryMapReader { table };
        let inner = rsleigh::Sleigh::new(arch.inner.sla_spec, arch.inner.pspec, reader)
            .map_err(|e| into_lift_err(anyhow::anyhow!("Sleigh::new failed: {e:?}")))?;
        Ok(Self {
            inner: Some(inner),
            arch_name: arch.preset_name,
            arch: arch.inner,
        })
    }

    fn arch_name(&self) -> &'static str {
        self.arch_name
    }

    fn __repr__(&self) -> String {
        format!("Sleigh(arch={})", self.arch_name)
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PySleigh>()
}
