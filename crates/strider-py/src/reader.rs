//! Python-visible memory readers.
//!
//! `PyBufferReader` is the data-only fast path: regions live entirely on
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

// ── PyBufferReader (data-only fast path) ─────────────────────────────────

/// Plain-data inner state shared by every clone of a `PyBufferReader`.
/// Held behind a single `Rc<RefCell<...>>` on the surface pyclass; the
/// `#[pyclass(unsendable)]` marker plus PyO3's GIL serialisation lets
/// us drop all of the prior `Arc<RwLock<...>>` ceremony.
pub(crate) struct PyBufferReaderInner {
    pub(crate) regions: Vec<MemRegion>,
    /// Lazily-rebuilt lookup table; cleared on every region change.
    pub(crate) table: Option<Arc<MemRegionsLookupTable>>,
}

/// Owned-data single-region buffer reader.  Implements
/// `rsleigh::MemReader` and `strider_reader::ReadOnlyMemory` indirectly
/// through the internal `PyBufferReaderView` view
/// (`MemInput::into_box` / `MemInput::into_any` mint the view on
/// demand).  Cheap to clone: the inner data is held behind one
/// `Rc<RefCell<...>>`.
///
/// This is the low-level reader for non-ELF / firmware / custom-source
/// cases.  For an ELF, prefer `strider.load_elf(path)` (yields an
/// `ElfStrider`), which builds the (multi-region) reader from the ELF
/// sections and adds symbol lookups.
///
/// `unsendable`: a `PyBufferReader` is only ever touched from the Python
/// thread that holds the GIL.  Downstream consumers that need a
/// `Send + Sync` reader take a `PyBufferReaderView` snapshot instead
/// (see `reader_view`), so the surface pyclass doesn't need to be
/// thread-safe.
#[pyclass(name = "BufferReader", module = "strider", unsendable)]
#[derive(Clone)]
pub struct PyBufferReader {
    /// `Rc` so a `BufferReader` clone shares state with the original —
    /// the prior `Arc<RwLock<...>>` layout had the same semantics.
    pub(crate) inner: Rc<RefCell<PyBufferReaderInner>>,
}

impl PyBufferReader {
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

    /// Mint a `PyBufferReaderView` snapshot of the current state — the
    /// lookup table is built on demand (or returned from the cache).
    /// The view is `Send + Sync` and implements both `rsleigh::MemReader`
    /// and `ReadOnlyMemory`, so downstream consumers that need either
    /// trait can take the view without forcing the surface
    /// `PyBufferReader` to be thread-safe.
    pub(crate) fn reader_view(&self) -> PyBufferReaderView {
        let table = self.lookup_table();
        PyBufferReaderView { table }
    }

    /// Build a reader from an already-assembled region list.  Used by the
    /// ELF loader (`load_elf` / `add_elf`), which collects multiple
    /// regions; the public `new` constructor is the single-region path.
    pub(crate) fn from_regions(regions: Vec<MemRegion>) -> Self {
        Self {
            inner: Rc::new(RefCell::new(PyBufferReaderInner {
                regions,
                table: None,
            })),
        }
    }
}

#[pymethods]
impl PyBufferReader {
    /// Create a buffer reader over a single raw-byte region: `data`
    /// mapped at `base_addr`.
    ///
    /// # Errors
    /// Raises `StriderError` if the region cannot be constructed.
    #[new]
    fn new(base_addr: u64, data: Vec<u8>) -> PyResult<Self> {
        let region = MemRegion::new(base_addr, data).map_err(into_strider_err)?;
        Ok(Self::from_regions(vec![region]))
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

/// Load an ELF's code + read-only (and, when `apply_relocations`, the
/// relocated-data) sections into the instruction-fetch / raw-read `mem`
/// region list, applying every understood relocation in-place when
/// requested.  Shared by both `load_elf` and `_LoadedElf::add_elf`.
fn elf_to_mem_regions(
    obj: &object::File<'static>,
    apply_relocations: bool,
) -> PyResult<Vec<MemRegion>> {
    if apply_relocations {
        let (regions, _stats) =
            strider_reader::elf::elf_load_with_relocations(obj).map_err(into_strider_err)?;
        Ok(regions)
    } else {
        strider_reader::elf::elf_get_loadable_regions(obj).map_err(into_strider_err)
    }
}

/// Load an ELF's **runtime-immutable** code + read-only sections into a
/// fresh region list — writable sections (`.data`, `.got`,
/// `.data.rel.ro`) are EXCLUDED.  This is the rom for the optimizer's
/// `LoadReadOnly` pass, which folds a constant-address load to the
/// resolved bytes without consulting the memory chain and therefore
/// trusts every resolvable address to be runtime-immutable; a writable
/// global that is stored then reloaded must NOT fold to its file-initial
/// value.
///
/// Relocations are applied (when requested) only to the read-only
/// regions: relocations targeting absent writable sections are skipped,
/// relocations into `.rodata` are applied.
fn elf_to_rom_regions(
    obj: &object::File<'static>,
    apply_relocations: bool,
) -> PyResult<Vec<MemRegion>> {
    if apply_relocations {
        let (regions, _stats) = strider_reader::elf::elf_load_readonly_with_relocations(obj)
            .map_err(into_strider_err)?;
        Ok(regions)
    } else {
        strider_reader::elf::elf_get_loadable_regions(obj).map_err(into_strider_err)
    }
}

/// Parsed ELF binary: the friendly face is the Python `Program`
/// returned by `strider.load(...)`, which wraps one of these.
///
/// Holds the parsed `object::File`(s) (in load order — the first wins
/// on symbol-name collisions) plus two internal raw `BufferReader`s
/// built from the ELF sections (with relocations applied per the
/// `apply_relocations` flag): a writable-inclusive `mem` reader for
/// instruction fetch / raw reads (`reader()`), and a runtime-immutable
/// `rom` reader (code + read-only only) for `LoadReadOnly` constant
/// folding (`ro_reader()`).  The leading underscore marks it as
/// internal-by-convention: construct it via `strider.load_elf(path)`
/// and reach for `Program` for the user-facing surface.
#[pyclass(name = "_LoadedElf", module = "strider", unsendable)]
pub struct PyLoadedElf {
    /// Loaded ELF objects, in `load_elf` / `add_elf` insertion order.
    /// `object::File<'static>` borrows from a leaked byte slice (see
    /// `strider_reader::load_elf`), so storing it here is sound.
    elfs: Vec<object::File<'static>>,
    /// Instruction-fetch / raw-read reader assembled from the ELF
    /// sections (writable sections included when relocations are
    /// applied).  Handed to `strider.run(mem=…)` via `reader()`.
    mem: PyBufferReader,
    /// Runtime-immutable reader (code + read-only sections only,
    /// writable sections EXCLUDED).  Handed to `strider.run(rom=…)` via
    /// `ro_reader()`: the `LoadReadOnly` rom MUST be runtime-immutable
    /// because the fold trusts it unconditionally.
    rom: PyBufferReader,
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
    /// The multi-region instruction-fetch / raw-read `BufferReader`
    /// assembled from this ELF's sections (writable sections included
    /// when relocations were applied).  Pass it to
    /// `strider.run(mem=…)`.
    fn reader(&self) -> PyBufferReader {
        self.mem.clone()
    }

    /// The **runtime-immutable** `BufferReader` (code + read-only
    /// sections only — writable `.data` / `.got` / `.data.rel.ro`
    /// EXCLUDED).  Pass it to `strider.run(rom=…)`: the `LoadReadOnly`
    /// rom MUST be runtime-immutable, because the fold replaces a
    /// constant-address load with the resolved bytes WITHOUT consulting
    /// the memory chain.
    fn ro_reader(&self) -> PyBufferReader {
        self.rom.clone()
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

    /// Merge another ELF (e.g. a shared library) into this one: extends
    /// the inner `BufferReader`'s regions and the symbol set.  The
    /// earlier-loaded ELF wins on symbol-name collisions.
    ///
    /// `apply_relocations` defaults to `False`; set it to `True` for
    /// ET_DYN binaries whose sections ship with unresolved relocations.
    #[pyo3(signature = (path, apply_relocations=false))]
    fn add_elf(&mut self, path: &str, apply_relocations: bool) -> PyResult<()> {
        let obj = strider_reader::load_elf(path).map_err(into_strider_err)?;
        let mem_regions = elf_to_mem_regions(&obj, apply_relocations)?;
        let rom_regions = elf_to_rom_regions(&obj, apply_relocations)?;
        {
            let mut inner = self.mem.inner.borrow_mut();
            inner.regions.extend(mem_regions);
            inner.table = None;
        }
        {
            let mut inner = self.rom.inner.borrow_mut();
            inner.regions.extend(rom_regions);
            inner.table = None;
        }
        self.elfs.push(obj);
        Ok(())
    }
}

/// Load an ELF binary from `path` into a `_LoadedElf` (the parsed
/// object the high-level `Program` wraps).  Loads every executable
/// section and every non-writable file-backed section into the inner
/// raw `BufferReader`, deriving the byte order from the ELF header.
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
    let mem = PyBufferReader::from_regions(elf_to_mem_regions(&obj, apply_relocations)?);
    let rom = PyBufferReader::from_regions(elf_to_rom_regions(&obj, apply_relocations)?);
    Ok(PyLoadedElf {
        elfs: vec![obj],
        mem,
        rom,
    })
}

// ── PyMemReader (callback ABC) ───────────────────────────────────────────

/// Python-subclassable abstract base.  Subclasses MUST override
/// `read(addr, size) -> Optional[bytes]`.  The default implementation
/// raises NotImplementedError.
///
/// Performance note: each `read` crosses the Rust↔Python boundary.
/// Use `BufferReader` for the in-process fast path when you can.
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
/// override `read(addr, size) -> Optional[bytes]` returning the `size`
/// RAW bytes at `addr` (NO endianness swap — the optimizer decodes per
/// the run's arch endianness) or `None` for unmapped.
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

    /// Override in a subclass to return the `size` RAW bytes at `addr`
    /// (`bytes`) or `None` for unmapped.  The bytes are NOT byte-swapped
    /// — the optimizer decodes them per the run's endianness.  The base
    /// raises `NotImplementedError`.
    #[allow(unused_variables)]
    fn read(&self, addr: u64, size: usize) -> PyResult<Option<Vec<u8>>> {
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
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        let size = buf.len();
        Python::with_gil(|py| -> anyhow::Result<()> {
            // Short-circuit: a prior `read` already raised a control-
            // flow exception that we stashed in the
            // PENDING_CONTROL_FLOW cell.  Stop calling into Python so
            // we don't trip CPython's "returned a result with an
            // exception set" guard on the next invocation.  The outer
            // `strider.run` boundary will drain the cell + surface the
            // saved PyErr.
            if crate::pattern::peek_pending_control_flow() {
                anyhow::bail!("read aborted: pending control-flow exception");
            }
            // The Python override returns the RAW `size` bytes at `addr`
            // (`bytes`) or `None` for unmapped.  Control-flow exceptions
            // (KeyboardInterrupt / SystemExit) are stashed so the outer
            // boundary surfaces them; every other failure errors here so
            // `LoadReadOnly` simply leaves the Load intact.
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
                        anyhow::bail!("read aborted: control-flow exception stashed");
                    }
                    anyhow::bail!("ReadOnlyMemory.read({addr:#x}, {size}) raised: {e}");
                }
            };
            if result.is_none(py) {
                anyhow::bail!("ReadOnlyMemory.read({addr:#x}, {size}) returned None (unmapped)");
            }
            let bytes = result.extract::<Vec<u8>>(py).map_err(|e| {
                anyhow::anyhow!(
                    "ReadOnlyMemory.read({addr:#x}, {size}) did not return bytes: {e}"
                )
            })?;
            if bytes.len() != size {
                anyhow::bail!(
                    "ReadOnlyMemory.read({addr:#x}, {size}) returned {} bytes, expected {size}",
                    bytes.len()
                );
            }
            buf.copy_from_slice(&bytes);
            Ok(())
        })
    }
}

// ── AnyMemReader — unified Rust reader type ──────────────────────────────

/// Unified `MemReader` used by every downstream Python wrapper
/// (PySleigh, PyCfg, PyStrider, …).  Constructed from either a
/// `PyBufferReader` snapshot (fast in-process path) or a
/// `PyMemReaderAdapter` (callback into a Python subclass).
pub enum AnyMemReader {
    Buffer(PyBufferReaderView),
    Cb(PyMemReaderAdapter),
}

impl rsleigh::MemReader for AnyMemReader {
    type Err = strider_reader::MemReadError;

    fn read(&self, addr: rsleigh::VnAddr, out_buf: &mut [u8]) -> Result<usize, Self::Err> {
        match self {
            AnyMemReader::Buffer(m) => rsleigh::MemReader::read(m, addr, out_buf),
            AnyMemReader::Cb(c) => rsleigh::MemReader::read(c, addr, out_buf),
        }
    }
}

/// Internal view over a `PyBufferReader` snapshot used by
/// `AnyMemReader::Buffer` and by `MemInput::into_box`'s
/// `Box<dyn ReadOnlyMemory>` lift.  Decoupling the trait impl from the
/// Python class keeps the rsleigh dependency local, lets us hand a
/// *snapshot* to Sleigh (Sleigh consumes its reader by value, so a
/// snapshot avoids observing later region changes in flight), and
/// naturally satisfies `Send + Sync` — the lookup table is an
/// `Arc<...>` — without forcing the surface `PyBufferReader` pyclass to
/// be thread-safe.
///
/// The snapshot no longer carries an endianness: both trait impls fill
/// the caller buffer with RAW bytes, and integer decode happens in the
/// optimizer per the function's `Function::endianness` (derived from the
/// `SleighArch`).
pub struct PyBufferReaderView {
    pub table: Arc<MemRegionsLookupTable>,
}

impl rsleigh::MemReader for PyBufferReaderView {
    type Err = strider_reader::MemReadError;

    fn read(&self, addr: rsleigh::VnAddr, out_buf: &mut [u8]) -> Result<usize, Self::Err> {
        self.table
            .read(addr.off, out_buf)
            .ok_or_else(|| {
                strider_reader::MemReadError::from(anyhow::anyhow!("address {:#x} is not mapped", addr.off))
            })
    }
}

/// `ReadOnlyMemory` impl filling the caller buffer with RAW bytes from
/// the loaded region table — no endianness swap (the optimizer decodes
/// per the run's endianness now).  Fill-all-or-error: a partial /
/// unmapped range errors so `LoadReadOnly` never folds a partial word.
impl ReadOnlyMemory for PyBufferReaderView {
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        // Delegate to the lookup table's shared fill-all-or-error SSoT (raw
        // bytes, no endianness swap; partial/unmapped reads error).
        self.table.read_exact(addr, buf)
    }
}

// ── Polymorphic memory input ─────────────────────────────────────────────

/// Polymorphic memory argument used by every Python entry point that
/// accepts either a `BufferReader` (fast owned-data path) or a Python
/// subclass implementing `read(...)` (the callback path).
///
/// Consumed in three modes:
/// - [`into_box`](Self::into_box) — lift to `Box<dyn ReadOnlyMemory>`
///   for the ROM-style pipeline pass.
/// - [`into_any`](Self::into_any) — materialise into the unified
///   `AnyMemReader` (used to build a `Sleigh`).
/// - [`clone_one`](Self::clone_one) — produce an independent copy so a
///   single user-facing input can feed multiple `Sleigh` instances
///   (the orchestrator + the snapshot CFG each want their own reader).
pub enum MemInput {
    Buffer(PyBufferReader),
    Cb(Py<PyAny>),
}

impl<'py> FromPyObject<'py> for MemInput {
    fn extract_bound(ob: &Bound<'py, PyAny>) -> PyResult<Self> {
        if let Ok(m) = ob.extract::<PyBufferReader>() {
            return Ok(MemInput::Buffer(m));
        }
        if ob.hasattr("read")? {
            return Ok(MemInput::Cb(ob.clone().unbind()));
        }
        Err(pyo3::exceptions::PyTypeError::new_err(
            "expected a BufferReader or an object with a `read(...)` method",
        ))
    }
}

impl MemInput {
    /// Lift this input to a `Box<dyn ReadOnlyMemory>` (ROM role).
    /// For `PyBufferReader` this mints a `PyBufferReaderView` snapshot —
    /// the surface pyclass no longer implements `ReadOnlyMemory` so
    /// callers can't accidentally observe later region changes in
    /// flight (the snapshot semantics match the Sleigh-reader path).
    ///
    /// `Box` (not `Arc`) because strider runs single-threaded: the
    /// orchestrator's `Strider::rom` owns the rom for the whole run
    /// and threads it down as `&dyn ReadOnlyMemory` via the optimizer's
    /// `OptCtx`.  Python callbacks still go through
    /// [`PyReadOnlyMemoryAdapter`] which holds a refcounted `Py<...>`
    /// internally — no Rust-level sharing needed.
    pub fn into_box(self) -> Box<dyn ReadOnlyMemory> {
        match self {
            MemInput::Buffer(m) => Box::new(m.reader_view()),
            MemInput::Cb(obj) => Box::new(PyReadOnlyMemoryAdapter { py_obj: obj }),
        }
    }

    /// Materialise into the unified `AnyMemReader` (Sleigh-reader role).
    pub fn into_any(self) -> AnyMemReader {
        match self {
            MemInput::Buffer(m) => AnyMemReader::Buffer(m.reader_view()),
            MemInput::Cb(obj) => AnyMemReader::Cb(PyMemReaderAdapter { py_obj: obj }),
        }
    }

    /// Produce an independent `MemInput` referring to the same
    /// underlying source.  For `PyBufferReader` this is a cheap `Rc`
    /// bump; for the callback path we bump the `Py<PyAny>` refcount.
    pub fn clone_one(&self) -> PyResult<MemInput> {
        match self {
            MemInput::Buffer(m) => Ok(MemInput::Buffer(m.clone())),
            MemInput::Cb(obj) => {
                Python::with_gil(|py| Ok(MemInput::Cb(obj.clone_ref(py))))
            }
        }
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyBufferReader>()?;
    // NOTE: `PyLoadedElf` (`_LoadedElf`) is deliberately NOT registered as
    // a public Python class — it is an internal ELF parse / symbol backend
    // owned by the Python `ElfStrider`.  The `load_elf` pyfunction below is
    // the only seam: it returns a fully-usable `_LoadedElf` instance (its
    // pyclass methods are bound on the type object regardless of module
    // registration) that `_api.py` wraps inside an `ElfStrider`.
    m.add_class::<PyMemReader>()?;
    m.add_class::<PyReadOnlyMemory>()?;
    m.add_function(wrap_pyfunction!(load_elf, m)?)?;
    Ok(())
}
