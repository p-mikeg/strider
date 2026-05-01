//! `PySleigh` — wraps a constructed `rsleigh::Sleigh` keyed off a
//! `PySleighArch` + a memory reader (either `PyMemoryMap` or any
//! `MemReader` subclass).  Holds the `Sleigh` in an `Option` so it can
//! be moved into a downstream consumer (`cfg::Builder`, which takes
//! the Sleigh by value) and then put back when the consumer hands it
//! back via `Cfg::sleigh`.

use pyo3::prelude::*;

use crate::arch::PySleighArch;
use crate::errors::into_lift_err;
use crate::reader::{AnyMemReader, ReaderInput};

/// A constructed Sleigh keyed off a (SleighArch, reader) pair.
///
/// The inner `Sleigh<AnyMemReader>` is held in an `Option` so it can be
/// moved out (into a `cfg::Builder`, for example) and put back later
/// via `put_inner`.  While the inner is `None` the wrapper is "in use"
/// by some downstream consumer; further moves fail with `LiftError`.
#[pyclass(name = "Sleigh", module = "strider")]
pub struct PySleigh {
    pub(crate) inner: Option<rsleigh::Sleigh<AnyMemReader>>,
    pub(crate) arch_name: &'static str,
    /// Retained so the wrapper can be reconstructed if needed.
    #[allow(dead_code)]
    pub(crate) arch: target::SleighArch,
    /// Cached register table.  `Sleigh::regs()` only requires `&self`,
    /// but we eagerly cache it at construction time so callers can read
    /// the registers after the inner Sleigh has been moved into a
    /// downstream consumer (e.g. `cfg::Builder`).
    pub(crate) regs: rsleigh::SleighRegs,
}

impl PySleigh {
    /// Move the inner Sleigh out, leaving the wrapper as "in use".
    /// Returns `None` if it is already in use.
    pub(crate) fn take_inner(&mut self) -> Option<rsleigh::Sleigh<AnyMemReader>> {
        self.inner.take()
    }

    /// Restore the inner Sleigh, typically the one harvested out of
    /// `Cfg::sleigh` when a consumer hands it back.
    #[allow(dead_code)]
    pub(crate) fn put_inner(&mut self, sleigh: rsleigh::Sleigh<AnyMemReader>) {
        self.inner = Some(sleigh);
    }

    /// Internal constructor (mirrors `#[new]`).  Lets the run-style
    /// helpers in `run.rs` build a PySleigh without going through
    /// PyO3's argument-conversion path.
    pub(crate) fn new_internal(arch: PySleighArch, reader: AnyMemReader) -> PyResult<Self> {
        let inner = rsleigh::Sleigh::new(arch.inner.sla_spec, arch.inner.pspec, reader)
            .map_err(|e| into_lift_err(anyhow::anyhow!("Sleigh::new failed: {e:?}")))?;
        let regs = inner
            .regs()
            .map_err(|e| into_lift_err(anyhow::anyhow!("Sleigh::regs failed: {e:?}")))?;
        Ok(Self {
            inner: Some(inner),
            arch_name: arch.preset_name,
            arch: arch.inner,
            regs,
        })
    }
}

#[pymethods]
impl PySleigh {
    #[new]
    fn new(arch: PySleighArch, mem: ReaderInput) -> PyResult<Self> {
        let reader = mem.into_any().map_err(into_lift_err)?;
        Self::new_internal(arch, reader)
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
