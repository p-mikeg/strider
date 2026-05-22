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

use crate::errors::into_reader_err;
use strider_reader::{MemRegion, MemRegionsLookupTable, ReadOnlyMemory};

// ── PyRelocationStats — return type for apply_elf_relocations ────────────

/// Counts from a single `apply_elf_relocations` run.  Mirrors the
/// fields of `strider_reader::elf::RelocationStats` one-to-one.  Useful for
/// post-load diagnostics: a binary that reports `applied = 0` but
/// `seen > 0` is signalling that every relocation kind it carries is
/// unsupported (or every target is undefined / outside the loaded
/// region set).
///
/// `unsupported_r_types` lists the raw ELF `r_type` codes the applier
/// classified as unsupported.  Use it to self-diagnose what's
/// missing on a specific binary — pair with the System V ABI
/// per-arch relocation tables to identify each code.
#[pyclass(name = "RelocationStats", module = "strider", frozen)]
#[derive(Clone)]
pub struct PyRelocationStats {
    #[pyo3(get)]
    pub seen: usize,
    #[pyo3(get)]
    pub applied: usize,
    #[pyo3(get)]
    pub skipped_unresolved_target: usize,
    #[pyo3(get)]
    pub skipped_unsupported_kind: usize,
    #[pyo3(get)]
    pub skipped_no_region: usize,
    #[pyo3(get)]
    pub unsupported_r_types: Vec<u32>,
}

#[pymethods]
impl PyRelocationStats {
    fn __repr__(&self) -> String {
        format!(
            "RelocationStats(seen={}, applied={}, skipped_unresolved_target={}, \
             skipped_unsupported_kind={}, skipped_no_region={}, \
             unsupported_r_types={:?})",
            self.seen,
            self.applied,
            self.skipped_unresolved_target,
            self.skipped_unsupported_kind,
            self.skipped_no_region,
            self.unsupported_r_types,
        )
    }
}

impl From<strider_reader::elf::RelocationStats> for PyRelocationStats {
    fn from(s: strider_reader::elf::RelocationStats) -> Self {
        Self {
            seen: s.seen,
            applied: s.applied,
            skipped_unresolved_target: s.skipped_unresolved_target,
            skipped_unsupported_kind: s.skipped_unsupported_kind,
            skipped_no_region: s.skipped_no_region,
            unsupported_r_types: s.unsupported_r_types,
        }
    }
}

// ── PyMemoryMap (data-only fast path) ────────────────────────────────────

/// Plain-data inner state shared by every clone of a `PyMemoryMap`.
/// Held behind a single `Rc<RefCell<...>>` on the surface pyclass; the
/// `#[pyclass(unsendable)]` marker plus PyO3's GIL serialisation lets
/// us drop all of the prior `Arc<RwLock<...>>` ceremony.
pub(crate) struct PyMemoryMapInner {
    pub(crate) regions: Vec<MemRegion>,
    /// Lazily-rebuilt lookup table; cleared on every `add_region`.
    pub(crate) table: Option<Arc<MemRegionsLookupTable>>,
    /// Loaded ELF objects, in `add_region_from_elf` insertion order.
    /// Kept around so `symbol(name)` / `symbols()` can resolve names
    /// without forcing the user to re-parse the file via pyelftools.
    /// `object::File<'static>` borrows from a leaked byte slice (see
    /// `strider_reader::load_elf`), so storing it here is sound.
    pub(crate) elfs: Vec<object::File<'static>>,
    /// Byte order used by `ReadOnlyMemory::read` when assembling
    /// multi-byte words from the underlying buffer.  Defaults to
    /// [`strider_target::Endianness::Little`]; auto-set from the ELF
    /// header in `add_region_from_elf`, or set explicitly via
    /// [`PyMemoryMap::set_endianness`].
    pub(crate) endianness: strider_target::Endianness,
}

/// Owned-data memory map. Implements `rsleigh::MemReader` and
/// `strider_reader::ReadOnlyMemory` indirectly through the
/// internal `PyMemoryMapReader` view (`MemInput::into_arc` /
/// `MemInput::into_any` mint the view on demand). Cheap to clone:
/// the inner data is held behind one `Rc<RefCell<...>>`.
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

    /// Walk the loaded ELFs in insertion order and run `f` on the first
    /// symbol whose name matches `name`, returning its result.  Raises
    /// `ReaderError` when no loaded ELF defines the name.  Centralises
    /// the iterate-and-match loop plus the not-found error string
    /// shared by `symbol`, `symbol_size`, and `function_max_size`.
    fn find_symbol<R>(
        &self,
        name: &str,
        f: impl FnOnce(&object::Symbol<'_, '_>) -> R,
    ) -> PyResult<R> {
        let inner = self.inner.borrow();
        for obj in inner.elfs.iter() {
            if let Some(sym) = obj.symbol_by_name(name) {
                return Ok(f(&sym));
            }
        }
        Err(into_reader_err(anyhow::anyhow!(
            "symbol {name:?} not found in any ELF loaded into this MemoryMap \
             ({} loaded)",
            inner.elfs.len()
        )))
    }
}

#[pymethods]
impl PyMemoryMap {
    #[new]
    fn new() -> Self {
        Self {
            inner: Rc::new(RefCell::new(PyMemoryMapInner {
                regions: Vec::new(),
                table: None,
                elfs: Vec::new(),
                // Default to LE; overridden by add_region_from_elf or
                // set_endianness once the user supplies a real arch.
                endianness: strider_target::Endianness::Little,
            })),
        }
    }

    /// Set the byte order used by `ReadOnlyMemory::read` when reading
    /// multi-byte words.  Use `"little"` or `"big"` (case-insensitive).
    ///
    /// Auto-set by `add_region_from_elf` from the ELF header; explicit
    /// calls are useful only when constructing a MemoryMap from raw
    /// bytes for a big-endian target.
    ///
    /// # Errors
    /// Raises `ReaderError` for unrecognised endianness strings.
    fn set_endianness(&self, endianness: &str) -> PyResult<()> {
        let parsed = match endianness.to_ascii_lowercase().as_str() {
            "little" | "le" => strider_target::Endianness::Little,
            "big" | "be" => strider_target::Endianness::Big,
            other => {
                return Err(into_reader_err(anyhow::anyhow!(
                    "unknown endianness {other:?}; use \"little\" or \"big\""
                )));
            }
        };
        self.inner.borrow_mut().endianness = parsed;
        Ok(())
    }

    fn add_region(&self, start_addr: u64, data: Vec<u8>) -> PyResult<()> {
        let region = MemRegion::new(start_addr, data).map_err(into_reader_err)?;
        let mut inner = self.inner.borrow_mut();
        inner.regions.push(region);
        inner.table = None;
        Ok(())
    }

    fn region_count(&self) -> usize {
        self.inner.borrow().regions.len()
    }

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

    /// Convenience: load every executable section and every non-writable
    /// section with file-backed data from an ELF file at `path` and add
    /// them as regions.  Mirrors `strider_reader::ElfFileMemReader::from_path`'s
    /// region selection.
    ///
    /// `apply_relocations` defaults to `False`.  Set it to `True` for
    /// ET_DYN binaries (FreeBSD kernels, PIE userland) whose `.text`
    /// or function-pointer tables ship with unresolved relocations
    /// (`call rel32` placeholders, `R_*_RELATIVE` entries):
    /// `strider_reader::elf::apply_elf_relocations` runs over the loaded
    /// regions in-place after the section copy and patches every
    /// relocation it understands.  See the function's doc-comment
    /// for the supported kinds.
    #[pyo3(signature = (path, apply_relocations=false))]
    fn add_region_from_elf(&self, path: &str, apply_relocations: bool) -> PyResult<()> {
        let obj = strider_reader::load_elf(path).map_err(into_reader_err)?;
        // Auto-set the byte order from the ELF header so subsequent
        // ReadOnlyMemory::read calls assemble multi-byte words in the
        // right order (big-endian targets like MIPS-BE / PowerPC-BE
        // would otherwise byte-swap their LoadReadOnly constants).
        let elf_endian = match object::Object::endianness(&obj) {
            object::Endianness::Little => strider_target::Endianness::Little,
            object::Endianness::Big => strider_target::Endianness::Big,
        };
        // When apply_relocations=True we widen the section coverage
        // to include `.data.rel.ro`, `.got`, …  Without the widening
        // a `R_*_RELATIVE` relocation against a writable-but-
        // relocated table has nowhere to land (the applier reports
        // skipped_no_region) and the analysis sees zeros where the
        // function pointers should be.
        let regions = if apply_relocations {
            let (regions, _stats) =
                strider_reader::elf::elf_load_with_relocations(&obj).map_err(into_reader_err)?;
            regions
        } else {
            strider_reader::elf::elf_get_code_and_readonly_sections_as_mem_regions(&obj)
                .map_err(into_reader_err)?
        };
        let mut inner = self.inner.borrow_mut();
        inner.endianness = elf_endian;
        inner.regions.extend(regions);
        inner.table = None;
        // Cache the ELF for subsequent symbol() / symbols() / entry_point() calls.
        inner.elfs.push(obj);
        Ok(())
    }

    /// Apply the ELF at `path`'s dynamic relocations to the regions
    /// already loaded into this MemoryMap.  Returns the
    /// `RelocationStats` breakdown so callers can sanity-check the
    /// outcome.
    ///
    /// **Auto-loads missing site sections.**  When a relocation
    /// site falls outside every region currently in the MemoryMap,
    /// the supplied ELF's containing section is appended on the
    /// fly and the relocation is then applied to it.  This is what
    /// makes the common pattern
    ///
    /// ```python
    /// mem.add_region_from_elf(path)              # code + rodata
    /// mem.apply_elf_relocations(path)            # autoloads .got.plt etc.
    /// ```
    ///
    /// produce the same patched-region set as the bundled
    /// `add_region_from_elf(path, apply_relocations=True)` form,
    /// instead of silently reporting `applied = 0` with every
    /// reloc counted under `skipped_no_region`.  See
    /// `crates/reader/src/elf.rs::apply_elf_relocations_autoload`
    /// for the lazy-load contract (file-backed `SHF_ALLOC`
    /// sections only — `SHT_NOBITS` like `.bss` is never
    /// autoloaded).
    fn apply_elf_relocations(&self, path: &str) -> PyResult<PyRelocationStats> {
        let obj = strider_reader::load_elf(path).map_err(into_reader_err)?;
        let mut inner = self.inner.borrow_mut();
        let stats = strider_reader::elf::apply_elf_relocations_autoload(&mut inner.regions, &obj)
            .map_err(into_reader_err)?;
        // Invalidate the lookup table — both the autoload step
        // (which appends new regions) and the in-place patches
        // require a rebuild before the next read.
        inner.table = None;
        Ok(stats.into())
    }

    /// Look up the address of a function/data symbol across every ELF
    /// loaded into this MemoryMap via `add_region_from_elf`.  Returns
    /// the first match in load order; raises `ReaderError` when no
    /// loaded ELF defines the name.
    fn symbol(&self, name: &str) -> PyResult<u64> {
        self.find_symbol(name, |sym| sym.address())
    }

    /// Return the ELF-recorded size in bytes of the symbol named
    /// `name` (`st_size` for an ELF symbol).  Returns `None` when
    /// the symbol exists but its size is recorded as 0 (typical for
    /// data symbols in stripped binaries, or for stub functions).
    /// Raises `ReaderError` when the symbol isn't defined in any
    /// loaded ELF.
    ///
    /// Pair with `symbol(name)` to derive a `function_max_size`
    /// argument for `strider.run` / `strider.build_cfg`:
    ///
    /// ```python
    /// addr = mem.symbol("sys_thr_self")
    /// size = mem.symbol_size("sys_thr_self")
    /// strider.run(..., entry=addr, function_max_size=size)
    /// ```
    fn symbol_size(&self, name: &str) -> PyResult<Option<u64>> {
        self.find_symbol(name, |sym| {
            let size = sym.size();
            if size == 0 { None } else { Some(size) }
        })
    }

    /// Convenience shortcut for the common
    /// `(symbol(name), symbol_size(name))` pair — returns
    /// `(addr, size)` so callers don't need two lookups.  `size` is
    /// `None` when the ELF doesn't record one (zero `st_size`).
    /// Raises `ReaderError` when the symbol is undefined.
    fn function_max_size(&self, name: &str) -> PyResult<(u64, Option<u64>)> {
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
        let inner = self.inner.borrow();
        let mut out: HashMap<String, u64> = HashMap::new();
        for obj in inner.elfs.iter() {
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

    /// ELF entry-point address from the first loaded ELF.  Raises
    /// `ReaderError` when no ELF has been loaded yet.
    fn entry_point(&self) -> PyResult<u64> {
        let inner = self.inner.borrow();
        let first = inner.elfs.first().ok_or_else(|| {
            into_reader_err(anyhow::anyhow!(
                "no ELF loaded into this MemoryMap; call add_region_from_elf first"
            ))
        })?;
        Ok(first.entry())
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
    type Err = strider_reader::MemReadError;

    fn read(&self, addr: rsleigh::VnAddr, out_buf: &mut [u8]) -> Result<usize, Self::Err> {
        Python::with_gil(|py| -> anyhow::Result<usize> {
            // Re-raise control-flow exceptions (`KeyboardInterrupt`,
            // `SystemExit`) so Ctrl-C / sys.exit during a long lift
            // can interrupt rather than being silently absorbed into
            // a `ReaderError`.  Mirrors the same guard in
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
/// little-endian-decoded value or `None` for unmapped.
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
            // Surface Python exceptions on stderr instead of silently
            // converting them to None — otherwise a buggy user override
            // (raises ValueError, returns wrong type, …) shows up as
            // "no fold" in LoadReadOnly with no diagnostic.  The
            // contract is still `Option<u64>` (we can't propagate
            // through this trait) but the user gets a visible warning.
            //
            // Control-flow exceptions (`KeyboardInterrupt`, `SystemExit`)
            // are re-raised so Ctrl-C in an interactive Python session
            // can interrupt a long `LoadReadOnly` pass instead of being
            // silently absorbed.
            let result = match self.py_obj.call_method1(py, "read", (addr, size)) {
                Ok(r) => r,
                Err(e) => {
                    if e.is_instance_of::<pyo3::exceptions::PyKeyboardInterrupt>(py)
                        || e.is_instance_of::<pyo3::exceptions::PySystemExit>(py)
                    {
                        e.restore(py);
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
    m.add_class::<PyMemReader>()?;
    m.add_class::<PyReadOnlyMemory>()?;
    m.add_class::<PyRelocationStats>()?;
    Ok(())
}
