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

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use object::{Object, ObjectSymbol};
use pyo3::prelude::*;
use pyo3::types::PyBytes;

use crate::errors::into_strider_err;
use strider_reader::{MemRegion, MemRegionsLookupTable, ReadOnlyMemory};

// ── PyMemoryMap (data-only fast path) ────────────────────────────────────

/// Plain-data inner state shared by every clone of a `PyMemoryMap`.
/// Held behind a single `Rc<RefCell<...>>` on the surface pyclass; the
/// `#[pyclass(unsendable)]` marker plus PyO3's GIL serialisation lets
/// us drop all of the prior `Arc<RwLock<...>>` ceremony.
pub(crate) struct PyMemoryMapInner {
    pub(crate) regions: Vec<MemRegion>,
    /// Lazily-rebuilt lookup table; cleared on every `add_region`.
    pub(crate) table: Option<Arc<MemRegionsLookupTable>>,
    /// Byte order used by `ReadOnlyMemory::read` when assembling
    /// multi-byte words from the underlying buffer.  Defaults to
    /// [`strider_target::Endianness::Little`]; set from the ELF header
    /// by `load_elf` (which builds the inner map for a `_LoadedElf`),
    /// or explicitly via [`PyMemoryMap::set_endianness`].
    pub(crate) endianness: strider_target::Endianness,
}

/// Owned-data raw-region memory map.  Implements `rsleigh::MemReader`
/// and `strider_reader::ReadOnlyMemory` indirectly through the internal
/// `PyMemoryMapReader` view (`MemInput::into_arc` / `MemInput::into_any`
/// mint the view on demand).  Cheap to clone: the inner data is held
/// behind one `Rc<RefCell<...>>`.
///
/// This is the low-level reader for non-ELF / firmware / custom-source
/// cases.  For an ELF, prefer `strider.load(path)` (→ `Program`), which
/// builds one of these from the ELF sections and adds symbol lookups.
///
/// `unsendable`: a `PyMemoryMap` is only ever touched from the Python
/// thread that holds the GIL.  Downstream consumers that need a
/// `Send + Sync` reader take a `PyMemoryMapReader` snapshot instead
/// (see `reader_view`), so the surface pyclass doesn't need to be
/// thread-safe.
#[pyclass(name = "MemoryMap", module = "strider", unsendable)]
#[derive(Clone)]
pub struct PyMemoryMap {
    /// `Rc` so a `MemoryMap` clone shares state with the original
    /// (e.g. an `add_region` on one handle is visible from the other —
    /// the prior `Arc<RwLock<...>>` layout had the same semantics).
    pub(crate) inner: Rc<RefCell<PyMemoryMapInner>>,
}

impl PyMemoryMap {
    fn rebuild_table(&self) -> Arc<MemRegionsLookupTable> {
        let mut inner = self.inner.borrow_mut();
        let t = Arc::new(MemRegionsLookupTable::new(inner.regions.clone()));
        inner.table = Some(Arc::clone(&t));
        t
    }

    /// Returns a snapshot of the current lookup table, building it on
    /// demand if invalidated.  Used internally by both `read` and the
    /// `MemReader` view supplied to `Sleigh::new`.
    pub(crate) fn lookup_table(&self) -> Arc<MemRegionsLookupTable> {
        if let Some(t) = self.inner.borrow().table.as_ref() {
            return Arc::clone(t);
        }
        self.rebuild_table()
    }

    /// Mint a `PyMemoryMapReader` snapshot of the current state — the
    /// lookup table is built on demand (or returned from the cache) and
    /// the endianness is copied out.  The view is `Send + Sync` and
    /// implements both `rsleigh::MemReader` and `ReadOnlyMemory`, so
    /// downstream consumers that need either trait can take the view
    /// without forcing the surface `PyMemoryMap` to be thread-safe.
    pub(crate) fn reader_view(&self) -> PyMemoryMapReader {
        let table = self.lookup_table();
        let endianness = self.inner.borrow().endianness;
        PyMemoryMapReader { table, endianness }
    }
}

#[pymethods]
impl PyMemoryMap {
    /// Create an empty memory map.  Add raw byte regions with
    /// `add_region`.
    #[new]
    fn new() -> Self {
        Self {
            inner: Rc::new(RefCell::new(PyMemoryMapInner {
                regions: Vec::new(),
                table: None,
                // Default to LE; override via set_endianness for a
                // big-endian raw-bytes target (load_elf sets it from
                // the header for the ELF-backed path).
                endianness: strider_target::Endianness::Little,
            })),
        }
    }

    /// Set the byte order used by `ReadOnlyMemory::read` when reading
    /// multi-byte words.  Use `"little"` or `"big"` (case-insensitive).
    ///
    /// Useful when constructing a MemoryMap from raw bytes for a
    /// big-endian target.  The ELF-backed path (`strider.load`) sets
    /// this from the ELF header automatically.
    ///
    /// # Errors
    /// Raises `StriderError` for unrecognised endianness strings.
    fn set_endianness(&self, endianness: &str) -> PyResult<()> {
        let parsed = match endianness.to_ascii_lowercase().as_str() {
            "little" | "le" => strider_target::Endianness::Little,
            "big" | "be" => strider_target::Endianness::Big,
            other => {
                return Err(into_strider_err(anyhow::anyhow!(
                    "unknown endianness {other:?}; use \"little\" or \"big\""
                )));
            }
        };
        self.inner.borrow_mut().endianness = parsed;
        Ok(())
    }

    /// Add a region of raw bytes `data` mapped at `start_addr`.
    /// Raises `StriderError` if the region overlaps an existing one.
    fn add_region(&self, start_addr: u64, data: Vec<u8>) -> PyResult<()> {
        let region = MemRegion::new(start_addr, data).map_err(into_strider_err)?;
        let mut inner = self.inner.borrow_mut();
        inner.regions.push(region);
        inner.table = None;
        Ok(())
    }

    /// Number of regions currently in the map.
    fn region_count(&self) -> usize {
        self.inner.borrow().regions.len()
    }

    /// Read up to `size` bytes starting at `addr`.  Returns the bytes
    /// (possibly fewer than `size` near a region edge) or `None` when
    /// `addr` is unmapped.
    fn read<'py>(
        &self,
        py: Python<'py>,
        addr: u64,
        size: usize,
    ) -> PyResult<Option<Bound<'py, PyBytes>>> {
        let table = self.lookup_table();
        let mut buf = vec![0u8; size];
        match table.read(addr, &mut buf) {
            Some(n) => {
                buf.truncate(n);
                Ok(Some(PyBytes::new_bound(py, &buf)))
            }
            None => Ok(None),
        }
    }
}

// ── _LoadedElf (ELF parse + symbols, built by load_elf) ──────────────────

/// Derive the byte order of an `object::File` as a
/// `strider_target::Endianness`.
fn elf_endianness(obj: &object::File<'_>) -> strider_target::Endianness {
    match object::Object::endianness(obj) {
        object::Endianness::Little => strider_target::Endianness::Little,
        object::Endianness::Big => strider_target::Endianness::Big,
    }
}

/// Load an ELF's code + read-only (and, when `apply_relocations`, the
/// relocated-data) sections into a fresh region list, applying every
/// understood relocation in-place when requested.  Shared by both
/// `load_elf` and `_LoadedElf::add_elf`.
fn elf_to_regions(
    obj: &object::File<'static>,
    apply_relocations: bool,
) -> PyResult<Vec<MemRegion>> {
    if apply_relocations {
        let (regions, _stats) =
            strider_reader::elf::elf_load_with_relocations(obj).map_err(into_strider_err)?;
        Ok(regions)
    } else {
        strider_reader::elf::elf_get_code_and_readonly_sections_as_mem_regions(obj)
            .map_err(into_strider_err)
    }
}

/// Parsed ELF binary: the friendly face is the Python `Program`
/// returned by `strider.load(...)`, which wraps one of these.
///
/// Holds the parsed `object::File`(s) (in load order — the first wins
/// on symbol-name collisions) plus an internal raw `MemoryMap` built
/// from the ELF sections (with relocations applied per the
/// `apply_relocations` flag).  The leading underscore marks it as
/// internal-by-convention: construct it via `strider.load_elf(path)`
/// and reach for `Program` for the user-facing surface.
#[pyclass(name = "_LoadedElf", module = "strider", unsendable)]
pub struct PyLoadedElf {
    /// Loaded ELF objects, in `load_elf` / `add_elf` insertion order.
    /// `object::File<'static>` borrows from a leaked byte slice (see
    /// `strider_reader::load_elf`), so storing it here is sound.
    elfs: Vec<object::File<'static>>,
    /// Raw-region reader assembled from the ELF sections.  Handed to
    /// `strider.run(mem=…, rom=…)` via `memory_map()`.
    mem: PyMemoryMap,
}

impl PyLoadedElf {
    /// Walk the loaded ELFs in load order and run `f` on the first
    /// symbol whose name matches `name`.  Raises `StriderError` when no
    /// loaded ELF defines the name.
    fn find_symbol<R>(
        &self,
        name: &str,
        f: impl FnOnce(&object::Symbol<'_, '_>) -> R,
    ) -> PyResult<R> {
        for obj in self.elfs.iter() {
            if let Some(sym) = obj.symbol_by_name(name) {
                return Ok(f(&sym));
            }
        }
        Err(into_strider_err(anyhow::anyhow!(
            "symbol {name:?} not found in any ELF loaded into this Program \
             ({} loaded)",
            self.elfs.len()
        )))
    }
}

#[pymethods]
impl PyLoadedElf {
    /// The raw-region `MemoryMap` assembled from this ELF's sections.
    /// Pass it to `strider.run(mem=…, rom=…)`; mutating it (e.g.
    /// `add_region`) is visible to subsequent reads through the same
    /// handle.
    fn memory_map(&self) -> PyMemoryMap {
        self.mem.clone()
    }

    /// Resolve a function/data symbol name to its address.  Returns the
    /// first match in load order; raises `StriderError` when no loaded
    /// ELF defines the name.
    fn symbol(&self, name: &str) -> PyResult<u64> {
        self.find_symbol(name, |sym| sym.address())
    }

    /// The ELF-recorded size in bytes of the symbol named `name`
    /// (`st_size`).  Returns `None` when the symbol exists but its size
    /// is recorded as 0 (typical for data symbols in stripped binaries
    /// or stub functions).  Raises `StriderError` when the symbol isn't
    /// defined in any loaded ELF.
    ///
    /// Pair with `symbol(name)` to derive a `function_max_size`
    /// argument for `strider.run` / `strider.build_cfg`.
    fn symbol_size(&self, name: &str) -> PyResult<Option<u64>> {
        self.find_symbol(name, |sym| {
            let size = sym.size();
            if size == 0 { None } else { Some(size) }
        })
    }

    /// Convenience shortcut for the `(symbol(name), symbol_size(name))`
    /// pair — returns `(addr, size)` so callers don't need two lookups.
    /// `size` is `None` when the ELF doesn't record one (zero
    /// `st_size`).  Raises `StriderError` when the symbol is undefined.
    /// The `size` half is exactly what `strider.run`'s
    /// `function_max_size=` keyword expects.
    fn symbol_addr_and_size(&self, name: &str) -> PyResult<(u64, Option<u64>)> {
        self.find_symbol(name, |sym| {
            let size = sym.size();
            (sym.address(), if size == 0 { None } else { Some(size) })
        })
    }

    /// All function/data symbols across every loaded ELF as a
    /// `dict[str, int]`.  Symbols with empty names or zero addresses
    /// (typical for synthetic linker entries) are skipped.  When two
    /// ELFs define the same name, the earlier-loaded one wins.
    fn symbols(&self) -> HashMap<String, u64> {
        let mut out: HashMap<String, u64> = HashMap::new();
        for obj in self.elfs.iter() {
            for sym in obj.symbols() {
                let Ok(name) = sym.name() else { continue };
                if name.is_empty() || sym.address() == 0 {
                    continue;
                }
                out.entry(name.to_string()).or_insert(sym.address());
            }
        }
        out
    }

    /// ELF entry-point address from the first loaded ELF.
    fn entry_point(&self) -> u64 {
        // `load_elf` always pushes at least one ELF before handing back
        // a `_LoadedElf`, so `first()` is always `Some`.
        self.elfs.first().map_or(0, |o| o.entry())
    }

    /// Read up to `size` raw bytes starting at `addr` from the loaded
    /// regions.  Returns the bytes (possibly fewer than `size` near a
    /// region edge) or `None` when `addr` is unmapped.
    fn read<'py>(
        &self,
        py: Python<'py>,
        addr: u64,
        size: usize,
    ) -> PyResult<Option<Bound<'py, PyBytes>>> {
        self.mem.read(py, addr, size)
    }

    /// The byte order of the loaded ELF as `"little"` or `"big"`.
    fn endianness(&self) -> &'static str {
        match self.mem.inner.borrow().endianness {
            strider_target::Endianness::Little => "little",
            strider_target::Endianness::Big => "big",
        }
    }

    /// Merge another ELF (e.g. a shared library) into this one: extends
    /// the inner `MemoryMap`'s regions and the symbol set.  The
    /// earlier-loaded ELF wins on symbol-name collisions.
    ///
    /// `apply_relocations` defaults to `False`; set it to `True` for
    /// ET_DYN binaries whose sections ship with unresolved relocations.
    #[pyo3(signature = (path, apply_relocations=false))]
    fn add_elf(&mut self, path: &str, apply_relocations: bool) -> PyResult<()> {
        let obj = strider_reader::load_elf(path).map_err(into_strider_err)?;
        let regions = elf_to_regions(&obj, apply_relocations)?;
        {
            let mut inner = self.mem.inner.borrow_mut();
            inner.regions.extend(regions);
            inner.table = None;
        }
        self.elfs.push(obj);
        Ok(())
    }
}

/// Load an ELF binary from `path` into a `_LoadedElf` (the parsed
/// object the high-level `Program` wraps).  Loads every executable
/// section and every non-writable file-backed section into the inner
/// raw `MemoryMap`, deriving the byte order from the ELF header.
///
/// `apply_relocations` defaults to `False`.  Set it to `True` for
/// ET_DYN binaries (kernels, PIE userland) whose `.text` or
/// function-pointer tables ship with unresolved relocations: the
/// widened section coverage (`.data.rel.ro`, `.got`, …) is loaded and
/// every understood relocation is patched in-place.
#[pyfunction]
#[pyo3(signature = (path, apply_relocations=false))]
pub fn load_elf(path: &str, apply_relocations: bool) -> PyResult<PyLoadedElf> {
    let obj = strider_reader::load_elf(path).map_err(into_strider_err)?;
    let endianness = elf_endianness(&obj);
    let regions = elf_to_regions(&obj, apply_relocations)?;
    let mem = PyMemoryMap::new();
    {
        let mut inner = mem.inner.borrow_mut();
        inner.endianness = endianness;
        inner.regions.extend(regions);
        inner.table = None;
    }
    Ok(PyLoadedElf {
        elfs: vec![obj],
        mem,
    })
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
    /// Base initialiser; accepts and ignores any args so subclasses can
    /// call `super().__init__(...)` freely.
    #[new]
    #[pyo3(signature = (*_args, **_kwargs))]
    fn new(_args: &Bound<'_, pyo3::types::PyTuple>, _kwargs: Option<&Bound<'_, pyo3::types::PyDict>>) -> Self {
        // Accept (and ignore) arbitrary positional / keyword args so
        // Python subclasses can call `super().__init__(...)` from
        // their own `__init__` without arity errors.
        Self
    }

    /// Override in a subclass to return up to `size` bytes at `addr`
    /// (`bytes`) or `None` for unmapped.  The base raises
    /// `NotImplementedError`.
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
    type Err = strider_reader::MemReadError;

    fn read(&self, addr: rsleigh::VnAddr, out_buf: &mut [u8]) -> Result<usize, Self::Err> {
        Python::with_gil(|py| -> anyhow::Result<usize> {
            // Re-raise control-flow exceptions (`KeyboardInterrupt`,
            // `SystemExit`) so Ctrl-C / sys.exit during a long lift
            // can interrupt rather than being silently absorbed into
            // a `StriderError`.  Mirrors the same guard in
            // `PyReadOnlyMemoryAdapter::read`.
            let result = match self
                .py_obj
                .call_method1(py, "read", (addr.off, out_buf.len()))
            {
                Ok(r) => r,
                Err(e) => {
                    if e.is_instance_of::<pyo3::exceptions::PyKeyboardInterrupt>(py)
                        || e.is_instance_of::<pyo3::exceptions::PySystemExit>(py)
                    {
                        e.restore(py);
                        anyhow::bail!("MemReader.read interrupted by Python control-flow exception");
                    }
                    anyhow::bail!("PyMemReader.read raised: {e}");
                }
            };
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
        .map_err(strider_reader::MemReadError::from)
    }
}

// ── PyReadOnlyMemory (callback ABC) ──────────────────────────────────────

/// Python-subclassable abstract base for `LoadReadOnly`.  Subclasses
/// override `read(addr, size) -> Optional[int]` returning the
/// target-endian-decoded value (the subclass is responsible for
/// byte-swapping per the binary's arch endianness — see the Rust
/// `ReadOnlyMemory` trait's contract) or `None` for unmapped.
#[pyclass(name = "ReadOnlyMemory", module = "strider", subclass)]
pub struct PyReadOnlyMemory;

#[pymethods]
impl PyReadOnlyMemory {
    /// Base initialiser; accepts and ignores any args so subclasses can
    /// call `super().__init__(...)` freely.
    #[new]
    #[pyo3(signature = (*_args, **_kwargs))]
    fn new(_args: &Bound<'_, pyo3::types::PyTuple>, _kwargs: Option<&Bound<'_, pyo3::types::PyDict>>) -> Self {
        Self
    }

    /// Override in a subclass to return the target-endian-decoded value
    /// of `size` bytes at `addr` (`int`) or `None` for unmapped.  The
    /// subclass owns the byte-swap per the binary's endianness.  The
    /// base raises `NotImplementedError`.
    #[allow(unused_variables)]
    fn read(&self, addr: u64, size: usize) -> PyResult<Option<u64>> {
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
    fn read(&self, addr: u64, size: usize) -> Option<u64> {
        Python::with_gil(|py| -> Option<u64> {
            // Short-circuit: a prior `read` already raised a control-
            // flow exception that we stashed in the
            // PENDING_CONTROL_FLOW cell.  Stop calling into Python so
            // we don't trip CPython's "returned a result with an
            // exception set" guard on the next invocation.  The outer
            // `strider.run` boundary will drain the cell + surface the
            // saved PyErr.
            if crate::pattern::peek_pending_control_flow() {
                return None;
            }
            // Surface Python exceptions on stderr instead of silently
            // converting them to None — otherwise a buggy user override
            // (raises ValueError, returns wrong type, …) shows up as
            // "no fold" in LoadReadOnly with no diagnostic.  The
            // contract is still `Option<u64>` (we can't propagate
            // through this trait); stash control-flow exceptions so
            // the outer boundary surfaces them.
            let result = match self.py_obj.call_method1(py, "read", (addr, size)) {
                Ok(r) => r,
                Err(e) => {
                    if e.is_instance_of::<pyo3::exceptions::PyKeyboardInterrupt>(py)
                        || e.is_instance_of::<pyo3::exceptions::PySystemExit>(py)
                    {
                        // Stash in the pending cell (NOT via
                        // `PyErr::restore`) so the next invocation
                        // doesn't see a set error indicator and trip
                        // the CPython "returned a result with an
                        // exception set" wrapper.
                        crate::pattern::stash_pending_control_flow(e);
                        return None;
                    }
                    eprintln!(
                        "strider: ReadOnlyMemory.read({addr:#x}, {size}) raised: {e}"
                    );
                    return None;
                }
            };
            if result.is_none(py) {
                return None;
            }
            match result.extract::<u64>(py) {
                Ok(v) => Some(v),
                Err(e) => {
                    eprintln!(
                        "strider: ReadOnlyMemory.read({addr:#x}, {size}) did not return int: {e}"
                    );
                    None
                }
            }
        })
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
    type Err = strider_reader::MemReadError;

    fn read(&self, addr: rsleigh::VnAddr, out_buf: &mut [u8]) -> Result<usize, Self::Err> {
        match self {
            AnyMemReader::Map(m) => rsleigh::MemReader::read(m, addr, out_buf),
            AnyMemReader::Cb(c) => rsleigh::MemReader::read(c, addr, out_buf),
        }
    }
}

/// Internal view over a `PyMemoryMap` snapshot used by AnyMemReader::Map
/// and by `MemInput::into_arc`'s `Arc<dyn ReadOnlyMemory>` lift.
/// Decoupling the trait impl from the Python class keeps the rsleigh
/// dependency local, lets us hand a *snapshot* to Sleigh (Sleigh
/// consumes its reader by value, so a snapshot avoids observing later
/// `add_region` calls in flight), and naturally satisfies `Send + Sync`
/// — the lookup table is an `Arc<...>` and the endianness is `Copy` —
/// without forcing the surface `PyMemoryMap` pyclass to be thread-safe.
pub struct PyMemoryMapReader {
    pub table: Arc<MemRegionsLookupTable>,
    /// Snapshot of the source `PyMemoryMap`'s endianness at the moment
    /// the view was minted.  Used by the `ReadOnlyMemory::read` impl to
    /// decode multi-byte words for big-endian targets.
    pub endianness: strider_target::Endianness,
}

impl rsleigh::MemReader for PyMemoryMapReader {
    type Err = strider_reader::MemReadError;

    fn read(&self, addr: rsleigh::VnAddr, out_buf: &mut [u8]) -> Result<usize, Self::Err> {
        self.table
            .read(addr.off, out_buf)
            .ok_or_else(|| {
                strider_reader::MemReadError::from(anyhow::anyhow!("address {:#x} is not mapped", addr.off))
            })
    }
}

/// `ReadOnlyMemory` impl reading 1/2/4/8-byte words from any space.
/// Mirrors `strider_reader::ElfFileMemReader`'s endianness-aware decoding so
/// big-endian targets (MIPS-BE / PowerPC-BE / AArch64-BE) get correct
/// `LoadReadOnly` constants.  Endianness is captured by
/// `PyMemoryMap::reader_view` at the moment the view is minted (the
/// surface `PyMemoryMap` is auto-set by `add_region_from_elf`, or set
/// explicitly via `set_endianness`); defaults to little for
/// raw-bytes-only construction.
impl ReadOnlyMemory for PyMemoryMapReader {
    fn read(&self, addr: u64, size: usize) -> Option<u64> {
        if size == 0 || size > 8 {
            return None;
        }
        let mut buf = [0u8; 8];
        let n = self.table.read(addr, &mut buf[..size])?;
        if n != size {
            return None;
        }
        // Layout `buf` so that `Endianness::read_u64` decodes the
        // size-byte payload correctly.  LE: bytes already in low slots.
        // BE: shift bytes to the high end so from_be_bytes treats the
        // payload as a widened N-byte BE word.
        let layout = match self.endianness {
            strider_target::Endianness::Little => buf,
            strider_target::Endianness::Big => {
                let mut be_buf = [0u8; 8];
                be_buf[8 - size..].copy_from_slice(&buf[..size]);
                be_buf
            }
        };
        Some(self.endianness.read_u64(layout))
    }
}

// ── Polymorphic memory input ─────────────────────────────────────────────

/// Polymorphic memory argument used by every Python entry point that
/// accepts either a `MemoryMap` (fast owned-data path) or a Python
/// subclass implementing `read(...)` (the callback path).
///
/// Consumed in three modes:
/// - [`into_arc`](Self::into_arc) — lift to `Arc<dyn ReadOnlyMemory>`
///   for the ROM-style pipeline pass.
/// - [`into_any`](Self::into_any) — materialise into the unified
///   `AnyMemReader` (used to build a `Sleigh`).
/// - [`clone_one`](Self::clone_one) — produce an independent copy so a
///   single user-facing input can feed multiple `Sleigh` instances
///   (the orchestrator + the snapshot CFG each want their own reader).
pub enum MemInput {
    Map(PyMemoryMap),
    Cb(Py<PyAny>),
}

impl<'py> FromPyObject<'py> for MemInput {
    fn extract_bound(ob: &Bound<'py, PyAny>) -> PyResult<Self> {
        if let Ok(m) = ob.extract::<PyMemoryMap>() {
            return Ok(MemInput::Map(m));
        }
        if ob.hasattr("read")? {
            return Ok(MemInput::Cb(ob.clone().unbind()));
        }
        Err(pyo3::exceptions::PyTypeError::new_err(
            "expected a MemoryMap or an object with a `read(...)` method",
        ))
    }
}

impl MemInput {
    /// Lift this input to an `Arc<dyn ReadOnlyMemory>` (ROM role).
    /// For `PyMemoryMap` this mints a `PyMemoryMapReader` snapshot —
    /// the surface pyclass no longer implements `ReadOnlyMemory` so
    /// callers can't accidentally observe later `add_region` calls in
    /// flight (the snapshot semantics match the Sleigh-reader path).
    pub fn into_arc(self) -> Arc<dyn ReadOnlyMemory> {
        match self {
            MemInput::Map(m) => Arc::new(m.reader_view()),
            MemInput::Cb(obj) => Arc::new(PyReadOnlyMemoryAdapter { py_obj: obj }),
        }
    }

    /// Materialise into the unified `AnyMemReader` (Sleigh-reader role).
    pub fn into_any(self) -> AnyMemReader {
        match self {
            MemInput::Map(m) => AnyMemReader::Map(m.reader_view()),
            MemInput::Cb(obj) => AnyMemReader::Cb(PyMemReaderAdapter { py_obj: obj }),
        }
    }

    /// Produce an independent `MemInput` referring to the same
    /// underlying source.  For `PyMemoryMap` this is a cheap `Arc`
    /// bump; for the callback path we bump the `Py<PyAny>` refcount.
    pub fn clone_one(&self) -> PyResult<MemInput> {
        match self {
            MemInput::Map(m) => Ok(MemInput::Map(m.clone())),
            MemInput::Cb(obj) => {
                Python::with_gil(|py| Ok(MemInput::Cb(obj.clone_ref(py))))
            }
        }
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyMemoryMap>()?;
    m.add_class::<PyLoadedElf>()?;
    m.add_class::<PyMemReader>()?;
    m.add_class::<PyReadOnlyMemory>()?;
    m.add_function(wrap_pyfunction!(load_elf, m)?)?;
    Ok(())
}
