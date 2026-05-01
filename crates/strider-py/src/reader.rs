//! Python-visible memory readers.
//!
//! `PyMemoryMap` is the data-only fast path: regions live entirely on
//! the Rust side.  `MemReader` (subclass-able from Python) is the
//! callback path: every read crosses the Rust↔Python boundary.
//! `ReadOnlyMemory` likewise has a Python-subclass-able callback path
//! for the optimiser's `LoadReadOnly` pass.
//!
//! `AnyMemReader` is the unified Rust enum used everywhere downstream
//! (PySleigh / PyCfg / PyStrider) — this lets the wrapper cover both
//! the fast in-process map and the callback variant with a single
//! `Sleigh<AnyMemReader>` type without monomorphising the entire
//! pipeline twice.

use std::sync::{Arc, Mutex, RwLock};

use pyo3::prelude::*;
use pyo3::types::PyBytes;

use crate::errors::into_reader_err;
use reader::{MemRegion, MemRegionsLookupTable, ReadOnlyMemory};

// ── PyMemoryMap (data-only fast path) ────────────────────────────────────

/// Owned-data memory map. Implements `rsleigh::MemReader` (via the
/// internal `PyMemoryMapReader` view) and `reader::ReadOnlyMemory`
/// directly. Cheap to clone: the inner data is held behind `Arc`.
#[pyclass(name = "MemoryMap", module = "strider")]
#[derive(Clone)]
pub struct PyMemoryMap {
    /// Wrapped in `Arc<RwLock<...>>` so `add_region` after construction
    /// remains possible without requiring `&mut self` plumbing across
    /// PyO3.  Cloning the wrapper bumps the Arc; mutations are
    /// synchronized through the `RwLock`.
    inner: Arc<RwLock<Vec<MemRegion>>>,
    /// Lazily-rebuilt lookup table; cleared on every `add_region`.
    table: Arc<RwLock<Option<Arc<MemRegionsLookupTable>>>>,
}

impl PyMemoryMap {
    fn rebuild_table(&self) -> anyhow::Result<Arc<MemRegionsLookupTable>> {
        let regions = self
            .inner
            .read()
            .map_err(|_| anyhow::anyhow!("MemoryMap regions lock poisoned"))?
            .clone();
        let t = Arc::new(MemRegionsLookupTable::new(regions));
        let mut slot = self
            .table
            .write()
            .map_err(|_| anyhow::anyhow!("MemoryMap table lock poisoned"))?;
        *slot = Some(Arc::clone(&t));
        Ok(t)
    }

    /// Returns a snapshot of the current lookup table, building it on
    /// demand if invalidated.  Used internally by both `read` and the
    /// `MemReader` view supplied to `Sleigh::new`.
    pub(crate) fn lookup_table(&self) -> anyhow::Result<Arc<MemRegionsLookupTable>> {
        let slot = self
            .table
            .read()
            .map_err(|_| anyhow::anyhow!("MemoryMap table lock poisoned"))?;
        if let Some(t) = slot.as_ref() {
            return Ok(Arc::clone(t));
        }
        drop(slot);
        self.rebuild_table()
    }
}

#[pymethods]
impl PyMemoryMap {
    #[new]
    fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(Vec::new())),
            table: Arc::new(RwLock::new(None)),
        }
    }

    fn add_region(&self, start_addr: u64, data: Vec<u8>) -> PyResult<()> {
        let region = MemRegion::new(start_addr, data).map_err(into_reader_err)?;
        let mut regions = self
            .inner
            .write()
            .map_err(|_| into_reader_err(anyhow::anyhow!("MemoryMap regions lock poisoned")))?;
        regions.push(region);
        let mut slot = self
            .table
            .write()
            .map_err(|_| into_reader_err(anyhow::anyhow!("MemoryMap table lock poisoned")))?;
        *slot = None;
        Ok(())
    }

    fn region_count(&self) -> PyResult<usize> {
        let regions = self
            .inner
            .read()
            .map_err(|_| into_reader_err(anyhow::anyhow!("MemoryMap regions lock poisoned")))?;
        Ok(regions.len())
    }

    fn read<'py>(
        &self,
        py: Python<'py>,
        addr: u64,
        size: usize,
    ) -> PyResult<Option<Bound<'py, PyBytes>>> {
        let table = self.lookup_table().map_err(into_reader_err)?;
        let mut buf = vec![0u8; size];
        match table.read(addr, &mut buf) {
            Some(n) => {
                buf.truncate(n);
                Ok(Some(PyBytes::new_bound(py, &buf)))
            }
            None => Ok(None),
        }
    }

    /// Convenience: load every executable section and every non-writable
    /// section with file-backed data from an ELF file at `path` and add
    /// them as regions.  Mirrors `reader::ElfFileMemReader::from_path`'s
    /// region selection.
    fn add_region_from_elf(&self, path: &str) -> PyResult<()> {
        let obj = reader::load_elf(path).map_err(into_reader_err)?;
        let regions = reader::elf::elf_get_code_and_readonly_sections_as_mem_regions(&obj)
            .map_err(into_reader_err)?;
        let mut inner = self
            .inner
            .write()
            .map_err(|_| into_reader_err(anyhow::anyhow!("MemoryMap regions lock poisoned")))?;
        inner.extend(regions);
        let mut slot = self
            .table
            .write()
            .map_err(|_| into_reader_err(anyhow::anyhow!("MemoryMap table lock poisoned")))?;
        *slot = None;
        Ok(())
    }
}

// ── PyMemReader (callback ABC) ───────────────────────────────────────────

/// Python-subclassable abstract base.  Subclasses MUST override
/// `read(addr, size) -> Optional[bytes]`.  The default implementation
/// raises NotImplementedError.
///
/// Performance note: each `read` crosses the Rust↔Python boundary.
/// Use `MemoryMap` for the in-process fast path when you can.
#[pyclass(name = "MemReader", module = "strider", subclass)]
pub struct PyMemReader;

#[pymethods]
impl PyMemReader {
    #[new]
    #[pyo3(signature = (*_args, **_kwargs))]
    fn new(_args: &Bound<'_, pyo3::types::PyTuple>, _kwargs: Option<&Bound<'_, pyo3::types::PyDict>>) -> Self {
        // Accept (and ignore) arbitrary positional / keyword args so
        // Python subclasses can call `super().__init__(...)` from
        // their own `__init__` without arity errors.
        Self
    }

    #[allow(unused_variables)]
    fn read<'py>(
        &self,
        py: Python<'py>,
        addr: u64,
        size: usize,
    ) -> PyResult<Option<Bound<'py, PyBytes>>> {
        Err(pyo3::exceptions::PyNotImplementedError::new_err(
            "MemReader.read must be overridden by subclass",
        ))
    }
}

/// Internal adapter: holds a `Py<PyAny>` (the user's Python subclass)
/// and implements `rsleigh::MemReader` by `Python::with_gil` per call.
pub struct PyMemReaderAdapter {
    pub py_obj: Py<PyAny>,
}

impl rsleigh::MemReader for PyMemReaderAdapter {
    type Err = anyhow::Error;

    fn read(&self, addr: rsleigh::VnAddr, out_buf: &mut [u8]) -> anyhow::Result<usize> {
        Python::with_gil(|py| {
            let result = self
                .py_obj
                .call_method1(py, "read", (addr.off, out_buf.len()))
                .map_err(|e| anyhow::anyhow!("PyMemReader.read raised: {e}"))?;
            // None → not mapped (return Err so the matcher falls through).
            if result.is_none(py) {
                anyhow::bail!("address {:#x} is not mapped (Python read returned None)", addr.off);
            }
            let bytes = result
                .extract::<Vec<u8>>(py)
                .map_err(|e| anyhow::anyhow!("PyMemReader.read must return bytes: {e}"))?;
            let n = bytes.len().min(out_buf.len());
            out_buf[..n].copy_from_slice(&bytes[..n]);
            Ok(n)
        })
    }
}

// ── PyReadOnlyMemory (callback ABC) ──────────────────────────────────────

/// Python-subclassable abstract base for `LoadReadOnly`.  Subclasses
/// override `read(space_id, addr, size) -> Optional[int]` returning the
/// little-endian-decoded value or None for unmapped.
#[pyclass(name = "ReadOnlyMemory", module = "strider", subclass)]
pub struct PyReadOnlyMemory;

#[pymethods]
impl PyReadOnlyMemory {
    #[new]
    #[pyo3(signature = (*_args, **_kwargs))]
    fn new(_args: &Bound<'_, pyo3::types::PyTuple>, _kwargs: Option<&Bound<'_, pyo3::types::PyDict>>) -> Self {
        Self
    }

    #[allow(unused_variables)]
    fn read(&self, space_id: u32, addr: u64, size: usize) -> PyResult<Option<u64>> {
        Err(pyo3::exceptions::PyNotImplementedError::new_err(
            "ReadOnlyMemory.read must be overridden by subclass",
        ))
    }
}

/// Internal adapter wrapping a Python `ReadOnlyMemory` subclass.
pub struct PyReadOnlyMemoryAdapter {
    pub py_obj: Py<PyAny>,
}

impl ReadOnlyMemory for PyReadOnlyMemoryAdapter {
    fn read(&self, space: rsleigh::VnSpace, addr: u64, size: usize) -> Option<u64> {
        Python::with_gil(|py| -> Option<u64> {
            let space_id = space_to_u32(space);
            let result = self
                .py_obj
                .call_method1(py, "read", (space_id, addr, size))
                .ok()?;
            if result.is_none(py) {
                return None;
            }
            result.extract::<u64>(py).ok()
        })
    }
}

/// Map a `VnSpace` to a stable u32 identifier exposed to Python.  The
/// caller's Python `read` impl can use this to distinguish RAM (0)
/// from REGISTER (1) or other spaces.
fn space_to_u32(s: rsleigh::VnSpace) -> u32 {
    if s == rsleigh::VnSpace::RAM {
        0
    } else if s == rsleigh::VnSpace::REGISTER {
        1
    } else if s == rsleigh::VnSpace::CONST {
        2
    } else if s == rsleigh::VnSpace::UNIQUE {
        3
    } else {
        u32::MAX
    }
}

// ── AnyMemReader — unified Rust reader type ──────────────────────────────

/// Unified `MemReader` used by every downstream Python wrapper
/// (PySleigh, PyCfg, PyStrider, …).  Constructed from either a
/// `PyMemoryMap` snapshot (fast in-process path) or a
/// `PyMemReaderAdapter` (callback into a Python subclass).
pub enum AnyMemReader {
    Map(PyMemoryMapReader),
    Cb(PyMemReaderAdapter),
}

impl rsleigh::MemReader for AnyMemReader {
    type Err = anyhow::Error;

    fn read(&self, addr: rsleigh::VnAddr, out_buf: &mut [u8]) -> anyhow::Result<usize> {
        match self {
            AnyMemReader::Map(m) => m.read(addr, out_buf),
            AnyMemReader::Cb(c) => c.read(addr, out_buf),
        }
    }
}

/// Internal view over a `PyMemoryMap` snapshot used by AnyMemReader::Map.
/// Decoupling the trait impl from the Python class keeps the rsleigh
/// dependency local and lets us hand a *snapshot* to Sleigh — Sleigh
/// consumes its reader by value, so a snapshot avoids observing later
/// `add_region` calls in flight.
pub struct PyMemoryMapReader {
    pub table: Arc<MemRegionsLookupTable>,
}

impl rsleigh::MemReader for PyMemoryMapReader {
    type Err = anyhow::Error;

    fn read(&self, addr: rsleigh::VnAddr, out_buf: &mut [u8]) -> anyhow::Result<usize> {
        self.table
            .read(addr.off, out_buf)
            .ok_or_else(|| anyhow::anyhow!("address {:#x} is not mapped", addr.off))
    }
}

/// `ReadOnlyMemory` impl reading 1/2/4/8-byte little-endian words from
/// any space — same pattern as `reader::ElfFileMemReader`, less the
/// endianness flip (for the data-only path the user supplies bytes
/// directly so the host endian convention dominates).
impl ReadOnlyMemory for PyMemoryMap {
    fn read(&self, _space: rsleigh::VnSpace, addr: u64, size: usize) -> Option<u64> {
        if size == 0 || size > 8 {
            return None;
        }
        let table = self.lookup_table().ok()?;
        let mut buf = [0u8; 8];
        let n = table.read(addr, &mut buf[..size])?;
        if n != size {
            return None;
        }
        Some(u64::from_le_bytes(buf))
    }
}

// ── Polymorphic ROM input ────────────────────────────────────────────────

/// Polymorphic argument for callers that accept a ROM: either a
/// `MemoryMap` (fast path) or any subclass of `ReadOnlyMemory`.  The
/// pipeline wraps both in `Arc<dyn ReadOnlyMemory>`.
pub enum RomInput {
    Map(PyMemoryMap),
    Cb(Py<PyAny>),
}

impl<'py> FromPyObject<'py> for RomInput {
    fn extract_bound(ob: &Bound<'py, PyAny>) -> PyResult<Self> {
        if let Ok(m) = ob.extract::<PyMemoryMap>() {
            return Ok(RomInput::Map(m));
        }
        // Otherwise treat as a Python subclass implementing read(space_id, addr, size).
        // Validate it has a read method.
        if ob.hasattr("read")? {
            return Ok(RomInput::Cb(ob.clone().unbind()));
        }
        Err(pyo3::exceptions::PyTypeError::new_err(
            "rom must be a MemoryMap or an object with a `read(space_id, addr, size)` method",
        ))
    }
}

impl RomInput {
    /// Lift this ROM input to an `Arc<dyn ReadOnlyMemory>`.
    pub fn into_arc(self) -> Arc<dyn ReadOnlyMemory> {
        match self {
            RomInput::Map(m) => Arc::new(m),
            RomInput::Cb(obj) => Arc::new(PyReadOnlyMemoryAdapter { py_obj: obj }),
        }
    }
}

// ── ReaderInput — polymorphic reader for Sleigh construction ─────────────

/// Polymorphic input for `Sleigh.__init__` / `strider.run(mem=...)`:
/// either a `MemoryMap` or a `MemReader` subclass.
pub enum ReaderInput {
    Map(PyMemoryMap),
    Cb(Py<PyAny>),
}

impl<'py> FromPyObject<'py> for ReaderInput {
    fn extract_bound(ob: &Bound<'py, PyAny>) -> PyResult<Self> {
        if let Ok(m) = ob.extract::<PyMemoryMap>() {
            return Ok(ReaderInput::Map(m));
        }
        if ob.hasattr("read")? {
            return Ok(ReaderInput::Cb(ob.clone().unbind()));
        }
        Err(pyo3::exceptions::PyTypeError::new_err(
            "mem must be a MemoryMap or an object with a `read(addr, size)` method",
        ))
    }
}

impl ReaderInput {
    /// Materialise into the unified `AnyMemReader`.
    pub fn into_any(self) -> anyhow::Result<AnyMemReader> {
        match self {
            ReaderInput::Map(m) => Ok(AnyMemReader::Map(PyMemoryMapReader {
                table: m.lookup_table()?,
            })),
            ReaderInput::Cb(obj) => Ok(AnyMemReader::Cb(PyMemReaderAdapter { py_obj: obj })),
        }
    }

    /// Convert to a `ReaderInputClone` which can produce multiple
    /// independent `AnyMemReader` instances (each Sleigh call wants
    /// its own).  For PyMemoryMap this is a cheap Arc bump; for the
    /// callback path we clone the `Py<PyAny>` ref-count.
    pub fn into_clone(self) -> PyResult<ReaderInputClone> {
        match self {
            ReaderInput::Map(m) => Ok(ReaderInputClone::Map(m)),
            ReaderInput::Cb(obj) => Ok(ReaderInputClone::Cb(obj)),
        }
    }
}

/// A clone-able reader input — produces fresh `AnyMemReader` instances
/// on demand via `materialise`.
pub enum ReaderInputClone {
    Map(PyMemoryMap),
    Cb(Py<PyAny>),
}

impl ReaderInputClone {
    pub fn materialise(&self) -> anyhow::Result<AnyMemReader> {
        match self {
            ReaderInputClone::Map(m) => Ok(AnyMemReader::Map(PyMemoryMapReader {
                table: m.lookup_table()?,
            })),
            ReaderInputClone::Cb(obj) => Python::with_gil(|py| {
                Ok(AnyMemReader::Cb(PyMemReaderAdapter {
                    py_obj: obj.clone_ref(py),
                }))
            }),
        }
    }
}

// Suppress an unused-warning for a Mutex import that's only used by
// downstream wrappers.
#[allow(dead_code)]
fn _unused_marker(_: Mutex<()>) {}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyMemoryMap>()?;
    m.add_class::<PyMemReader>()?;
    m.add_class::<PyReadOnlyMemory>()?;
    Ok(())
}
