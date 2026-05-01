//! Python-visible memory readers.
//!
//! `PyMemoryMap` is the data-only fast path: regions live entirely on
//! the Rust side. Callback-style `MemReader` / `ReadOnlyMemory`
//! subclasses live in the same file but are added in a later task.

use std::sync::{Arc, RwLock};

use pyo3::prelude::*;
use pyo3::types::PyBytes;

use crate::errors::into_reader_err;
use reader::{MemRegion, MemRegionsLookupTable, ReadOnlyMemory};

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

/// Internal view that implements `rsleigh::MemReader` for a snapshot of
/// a `PyMemoryMap`.  Decoupling the trait impl from the Python class
/// keeps the rsleigh dependency local and lets us hand a *snapshot* to
/// Sleigh — Sleigh consumes its reader by value, so a snapshot avoids
/// observing later `add_region` calls in flight.
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

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyMemoryMap>()
}
