use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use object::{Object, ObjectSymbol};
use pyo3::prelude::*;
use pyo3::types::PyBytes;

use crate::errors::into_strider_err;
use strider_reader::{MemRegion, MemRegionsLookupTable, ReadOnlyMemory};

pub(crate) struct PyBufferReaderInner {
    pub(crate) regions: Vec<MemRegion>,
    /// Lazily rebuilt; cleared on every region change.
    pub(crate) table: Option<Arc<MemRegionsLookupTable>>,
}

/// Raw byte reader for firmware or custom sources.  Works as both the
/// `mem` (instruction fetch) and `rom` (read-only memory) argument.  Cheap
/// to clone; clones share state with the original.
#[pyclass(name = "BufferReader", module = "strider.reader", unsendable)]
#[derive(Clone)]
pub struct PyBufferReader {
    pub(crate) inner: Rc<RefCell<PyBufferReaderInner>>,
}

impl PyBufferReader {
    pub(crate) fn lookup_table(&self) -> Arc<MemRegionsLookupTable> {
        if let Some(t) = self.inner.borrow().table.as_ref() {
            return Arc::clone(t);
        }
        let mut inner = self.inner.borrow_mut();
        let t = Arc::new(MemRegionsLookupTable::new(inner.regions.clone()));
        inner.table = Some(Arc::clone(&t));
        t
    }

    /// Point-in-time snapshot implementing both `rsleigh::MemReader` and
    /// `ReadOnlyMemory`.
    pub(crate) fn reader_view(&self) -> PyBufferReaderView {
        let table = self.lookup_table();
        PyBufferReaderView { table }
    }

    /// Longest contiguous run mapped from `addr` (`0` when unmapped).
    fn available_at(&self, addr: u64) -> usize {
        self.inner
            .borrow()
            .regions
            .iter()
            .filter(|r| r.contains(addr))
            .map(|r| (r.end_addr() - addr) as usize)
            .max()
            .unwrap_or(0)
    }

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
    /// Create a reader over a single raw-byte region: `data` mapped at
    /// `base_addr`.  Raises `StriderError` if the region is invalid.
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
        // `table.read` fills only what is mapped, so a multi-exabyte
        // `size` would allocate (and OOM) for nothing.
        let available = self.available_at(addr);
        let mut buf = vec![0u8; size.min(available)];
        match table.read(addr, &mut buf) {
            Some(n) => {
                buf.truncate(n);
                Ok(Some(PyBytes::new_bound(py, &buf)))
            }
            None => Ok(None),
        }
    }
}

/// `Segments` auto-dispatches: PT_LOAD headers for ET_EXEC / ET_DYN,
/// falling back to the section walker for ET_REL (no program headers).
/// `Sections` forces the section walk (first-wins VMA dedup) even on a
/// linked binary that does carry PT_LOAD segments.
#[derive(Clone, Copy)]
pub(crate) enum ElfRegionSource {
    Segments,
    Sections,
}

fn elf_to_mem_regions(
    obj: &object::File<'_>,
    source: ElfRegionSource,
    apply_relocations: bool,
) -> PyResult<Vec<MemRegion>> {
    match source {
        ElfRegionSource::Segments => {
            if apply_relocations {
                strider_reader::elf::elf_load_with_relocations(obj).map_err(into_strider_err)
            } else {
                strider_reader::elf::elf_get_loadable_regions(obj).map_err(into_strider_err)
            }
        }
        ElfRegionSource::Sections => {
            let mut regions = if apply_relocations {
                strider_reader::elf::elf_get_loadable_regions_sections_only_including_writable(obj)
                    .map_err(into_strider_err)?
            } else {
                strider_reader::elf::elf_get_loadable_regions_sections_only(obj)
                    .map_err(into_strider_err)?
            };
            if apply_relocations {
                strider_reader::elf::apply_elf_relocations_autoload(&mut regions, obj)
                    .map_err(into_strider_err)?;
            }
            Ok(regions)
        }
    }
}

/// Code + read-only sections only; writable ones (`.data`, `.got`,
/// `.data.rel.ro`) are EXCLUDED, so every address here is
/// runtime-immutable.
fn elf_to_rom_regions(
    obj: &object::File<'_>,
    source: ElfRegionSource,
    apply_relocations: bool,
) -> PyResult<Vec<MemRegion>> {
    match source {
        ElfRegionSource::Segments => {
            if apply_relocations {
                strider_reader::elf::elf_load_readonly_with_relocations(obj)
                    .map_err(into_strider_err)
            } else {
                strider_reader::elf::elf_get_loadable_regions(obj).map_err(into_strider_err)
            }
        }
        ElfRegionSource::Sections => {
            let mut regions = strider_reader::elf::elf_get_loadable_regions_sections_only(obj)
                .map_err(into_strider_err)?;
            if apply_relocations {
                strider_reader::elf::apply_elf_relocations(&mut regions, obj)
                    .map_err(into_strider_err)?;
            }
            Ok(regions)
        }
    }
}

/// Parsed ELF binary.  Construct via `strider.lift.load_elf(path)`.
#[pyclass(name = "_LoadedElf", module = "strider.reader", unsendable)]
pub struct PyLoadedElf {
    /// Load order; the first wins on symbol-name collisions.
    elfs: Vec<strider_reader::OwnedElf>,
    /// Instruction fetch / raw reads; includes writable sections when
    /// relocations were applied.
    mem: PyBufferReader,
    /// The runtime-immutable subset.
    rom: PyBufferReader,
    /// The region-collection strategy this ELF was loaded with.
    source: ElfRegionSource,
}

/// A zero ELF `st_size` means "unknown", not "empty".
fn nonzero_size(s: u64) -> Option<u64> {
    (s != 0).then_some(s)
}

fn invalidate_and_extend(reader: &PyBufferReader, regions: Vec<MemRegion>) {
    let mut inner = reader.inner.borrow_mut();
    inner.regions.extend(regions);
    inner.table = None;
}

impl PyLoadedElf {
    /// Run `f` on the first symbol named `name`, in ELF load order.
    fn find_symbol<R>(
        &self,
        name: &str,
        f: impl FnOnce(&object::Symbol<'_, '_>) -> R,
    ) -> PyResult<R> {
        for obj in self.elfs.iter() {
            // One name can have several symbols: FreeBSD's `model_name` is
            // both an STT_FUNC in `.text` and an STT_OBJECT in `.rodata`.
            // `object::symbol_by_name` returns the first symtab match
            // regardless of kind, which hands the lifter a data address and
            // decodes `.rodata` as code. Prefer `Text`, fall back to any
            // match so pure-data names still resolve for `symbol`/`read`.
            let mut fallback: Option<object::Symbol<'_, '_>> = None;
            let file = obj.file();
            for sym in file.symbols() {
                let Ok(sym_name) = sym.name() else { continue };
                if sym_name != name {
                    continue;
                }
                if sym.kind() == object::SymbolKind::Text {
                    return Ok(f(&sym));
                }
                fallback.get_or_insert(sym);
            }
            if let Some(sym) = fallback {
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
    /// The instruction-fetch / raw-read `BufferReader` for this ELF.
    fn reader(&self) -> PyBufferReader {
        self.mem.clone()
    }

    /// The runtime-immutable `BufferReader` (code + read-only sections
    /// only).
    fn ro_reader(&self) -> PyBufferReader {
        self.rom.clone()
    }

    /// Resolve a symbol name to its address (first match in load order).
    /// Raises `StriderError` when no loaded ELF defines it.
    fn symbol(&self, name: &str) -> PyResult<u64> {
        self.find_symbol(name, |sym| sym.address())
    }

    /// The ELF-recorded `st_size` of `name`, or `None` when recorded as 0
    /// (typical for stripped data symbols and stubs).  Raises
    /// `StriderError` when the symbol is undefined.
    fn symbol_size(&self, name: &str) -> PyResult<Option<u64>> {
        self.find_symbol(name, |sym| nonzero_size(sym.size()))
    }

    /// `(symbol(name), symbol_size(name))` in one lookup.  Raises
    /// `StriderError` when the symbol is undefined.
    fn symbol_addr_and_size(&self, name: &str) -> PyResult<(u64, Option<u64>)> {
        self.find_symbol(name, |sym| (sym.address(), nonzero_size(sym.size())))
    }

    /// All symbols across every loaded ELF as `dict[str, int]`.  Empty
    /// names and zero addresses (synthetic linker entries) are skipped;
    /// the earlier-loaded ELF wins a name collision.
    fn symbols(&self) -> HashMap<String, u64> {
        let mut out: HashMap<String, u64> = HashMap::new();
        for obj in self.elfs.iter() {
            let file = obj.file();
            for sym in file.symbols() {
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
        // `load_elf` always pushes one ELF, so `first()` is never `None`.
        self.elfs.first().map_or(0, |o| o.file().entry())
    }

    /// Read up to `size` raw bytes at `addr`.  Returns fewer bytes near a
    /// region edge, or `None` when `addr` is unmapped.
    fn read<'py>(
        &self,
        py: Python<'py>,
        addr: u64,
        size: usize,
    ) -> PyResult<Option<Bound<'py, PyBytes>>> {
        self.mem.read(py, addr, size)
    }

    /// Merge another ELF (e.g. a shared library) into this one, extending
    /// the regions and symbol set.  The earlier-loaded ELF wins a name
    /// collision.  Set `apply_relocations` for ET_DYN binaries whose
    /// sections ship with unresolved relocations.
    #[pyo3(signature = (path, apply_relocations=false))]
    fn add_elf(&mut self, path: &str, apply_relocations: bool) -> PyResult<()> {
        let obj = strider_reader::load_elf(path).map_err(into_strider_err)?;
        let file = obj.file();
        let mem_regions = elf_to_mem_regions(&file, self.source, apply_relocations)?;
        let rom_regions = elf_to_rom_regions(&file, self.source, apply_relocations)?;
        invalidate_and_extend(&self.mem, mem_regions);
        invalidate_and_extend(&self.rom, rom_regions);
        self.elfs.push(obj);
        Ok(())
    }
}

fn load_elf_impl(
    path: &str,
    source: ElfRegionSource,
    apply_relocations: bool,
) -> PyResult<PyLoadedElf> {
    let obj = strider_reader::load_elf(path).map_err(into_strider_err)?;
    let file = obj.file();
    let mem = PyBufferReader::from_regions(elf_to_mem_regions(&file, source, apply_relocations)?);
    let rom = PyBufferReader::from_regions(elf_to_rom_regions(&file, source, apply_relocations)?);
    Ok(PyLoadedElf {
        elfs: vec![obj],
        mem,
        rom,
        source,
    })
}

/// Load the ELF at `path`, collecting regions from PT_LOAD program
/// headers (falling back to the section walker for header-less ET_REL).
///
/// Set `apply_relocations` for ET_DYN binaries (kernels, PIE userland)
/// whose `.text` or function-pointer tables ship with unresolved
/// relocations: section coverage widens to `.data.rel.ro` / `.got` and
/// every understood relocation is patched in place.
#[pyfunction]
#[pyo3(name = "_load_elf_from_segments", signature = (path, apply_relocations=false))]
pub fn load_elf_from_segments(path: &str, apply_relocations: bool) -> PyResult<PyLoadedElf> {
    load_elf_impl(path, ElfRegionSource::Segments, apply_relocations)
}

/// Load the ELF at `path`, collecting regions by walking section headers
/// (first-wins VMA dedup) even when the binary carries PT_LOAD segments.
/// Use for section-granular regions (`.text` / `.rodata` / `.plt` as
/// separate mappings) instead of coalesced PT_LOAD ranges.
#[pyfunction]
#[pyo3(name = "_load_elf_from_sections", signature = (path, apply_relocations=false))]
pub fn load_elf_from_sections(path: &str, apply_relocations: bool) -> PyResult<PyLoadedElf> {
    load_elf_impl(path, ElfRegionSource::Sections, apply_relocations)
}

/// Instruction source backed by Python.  Subclass and override
/// `read(addr, size)` to feed the pipeline from a custom data source.
#[pyclass(name = "MemReader", module = "strider.reader", subclass)]
pub struct PyMemReader;

#[pymethods]
impl PyMemReader {
    /// Base initialiser; ignores any args so subclasses can call
    /// `super().__init__(...)` freely.
    #[new]
    #[pyo3(signature = (*_args, **_kwargs))]
    fn new(
        _args: &Bound<'_, pyo3::types::PyTuple>,
        _kwargs: Option<&Bound<'_, pyo3::types::PyDict>>,
    ) -> Self {
        Self
    }

    /// Override to return up to `size` bytes at `addr`, or `None` for
    /// unmapped.  The base raises `NotImplementedError`.
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

/// Shared `read`-callback prologue for both Python reader adapters, run
/// inside the caller's `Python::with_gil`.
///
/// `KeyboardInterrupt` / `SystemExit` are STASHED, not `PyErr::restore`d:
/// restoring would leave the error indicator set, so the next callback
/// would trip CPython's "returned a result with an exception set" guard
/// and destroy the original exception.  A stashed exception also
/// short-circuits every later call until the outer boundary drains the
/// cell and surfaces it.
fn call_py_read<A>(
    py: Python<'_>,
    py_obj: &Py<PyAny>,
    args: A,
    abort_label: &str,
    raise_msg: impl FnOnce(PyErr) -> anyhow::Error,
) -> anyhow::Result<Py<PyAny>>
where
    A: IntoPy<Py<pyo3::types::PyTuple>>,
{
    if crate::pattern::peek_pending_control_flow() {
        anyhow::bail!("{abort_label} aborted: pending control-flow exception");
    }
    match py_obj.call_method1(py, "read", args) {
        Ok(r) => Ok(r),
        Err(e) => {
            if e.is_instance_of::<pyo3::exceptions::PyKeyboardInterrupt>(py)
                || e.is_instance_of::<pyo3::exceptions::PySystemExit>(py)
            {
                crate::pattern::stash_pending_control_flow(e);
                anyhow::bail!("{abort_label} aborted: control-flow exception stashed");
            }
            Err(raise_msg(e))
        }
    }
}

/// Holds the user's Python reader object and implements
/// `rsleigh::MemReader` by `Python::with_gil` per call.
///
/// The `Py<PyAny>` is shared, not cloned per adapter, so a `Lifter` can
/// visit the same reference for cyclic-GC traversal
/// (`PyLifter::__traverse__`) and a reader/lifter cycle stays collectable.
#[derive(Clone)]
pub struct PyMemReaderAdapter {
    pub py_obj: std::sync::Arc<Py<PyAny>>,
}

impl rsleigh::MemReader for PyMemReaderAdapter {
    type Err = strider_reader::MemReadError;

    fn read(&self, addr: rsleigh::VnAddr, out_buf: &mut [u8]) -> Result<usize, Self::Err> {
        Python::with_gil(|py| -> anyhow::Result<usize> {
            let result = call_py_read(
                py,
                &self.py_obj,
                (addr.off, out_buf.len()),
                "MemReader.read",
                |e| anyhow::anyhow!("PyMemReader.read raised: {e}"),
            )?;
            // None means unmapped; Err so the matcher falls through.
            if result.is_none(py) {
                anyhow::bail!(
                    "address {:#x} is not mapped (Python read returned None)",
                    addr.off
                );
            }
            let bytes = result
                .extract::<Vec<u8>>(py)
                .map_err(|e| anyhow::anyhow!("PyMemReader.read must return bytes: {e}"))?;
            // The `MemReader` contract allows a short read near a region
            // edge, but an over-long return is a Python bug: reject it
            // rather than silently dropping the excess.
            if bytes.len() > out_buf.len() {
                anyhow::bail!(
                    "PyMemReader.read({:#x}, {}) returned {} bytes, more than requested",
                    addr.off,
                    out_buf.len(),
                    bytes.len()
                );
            }
            let n = bytes.len();
            out_buf[..n].copy_from_slice(&bytes[..n]);
            Ok(n)
        })
        .map_err(strider_reader::MemReadError::from)
    }
}

/// Read-only memory backed by Python, used by the `LoadReadOnly` pass.
/// Subclass and override `read(addr, size)` to return the raw bytes at
/// `addr`.  Only RAM loads reach it, so subclasses need not filter on
/// space.
#[pyclass(name = "ReadOnlyMemory", module = "strider.reader", subclass)]
pub struct PyReadOnlyMemory;

#[pymethods]
impl PyReadOnlyMemory {
    /// Base initialiser; ignores any args so subclasses can call
    /// `super().__init__(...)` freely.
    #[new]
    #[pyo3(signature = (*_args, **_kwargs))]
    fn new(
        _args: &Bound<'_, pyo3::types::PyTuple>,
        _kwargs: Option<&Bound<'_, pyo3::types::PyDict>>,
    ) -> Self {
        Self
    }

    /// Override to return the `size` RAW bytes at `addr`, or `None` for
    /// unmapped.  Bytes are not byte-swapped; the optimizer decodes them
    /// per the run's endianness.  The base raises `NotImplementedError`.
    #[allow(unused_variables)]
    fn read(&self, addr: u64, size: usize) -> PyResult<Option<Vec<u8>>> {
        Err(pyo3::exceptions::PyNotImplementedError::new_err(
            "ReadOnlyMemory.read must be overridden by subclass",
        ))
    }
}

/// Wraps a Python `ReadOnlyMemory` subclass.  Shares one `Py<>` for the
/// same cyclic-GC reason as [`PyMemReaderAdapter`].
pub struct PyReadOnlyMemoryAdapter {
    pub py_obj: std::sync::Arc<Py<PyAny>>,
}

impl ReadOnlyMemory for PyReadOnlyMemoryAdapter {
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        let size = buf.len();
        Python::with_gil(|py| -> anyhow::Result<()> {
            // Control-flow exceptions are stashed for the outer boundary;
            // any other failure errors here, so `LoadReadOnly` just leaves
            // the Load intact.
            let result = call_py_read(py, &self.py_obj, (addr, size), "read", |e| {
                anyhow::anyhow!("ReadOnlyMemory.read({addr:#x}, {size}) raised: {e}")
            })?;
            if result.is_none(py) {
                anyhow::bail!("ReadOnlyMemory.read({addr:#x}, {size}) returned None (unmapped)");
            }
            let bytes = result.extract::<Vec<u8>>(py).map_err(|e| {
                anyhow::anyhow!("ReadOnlyMemory.read({addr:#x}, {size}) did not return bytes: {e}")
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

/// Unified `MemReader`: either a `PyBufferReader` snapshot or a callback
/// into a Python subclass.  Both variants clone cheaply.
#[derive(Clone)]
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

/// Point-in-time snapshot of a `PyBufferReader`'s region table: it does not
/// observe later region changes.  Both `read` impls fill the caller buffer
/// with RAW bytes, never byte-swapped.
#[derive(Clone)]
pub struct PyBufferReaderView {
    pub table: Arc<MemRegionsLookupTable>,
}

impl rsleigh::MemReader for PyBufferReaderView {
    type Err = strider_reader::MemReadError;

    fn read(&self, addr: rsleigh::VnAddr, out_buf: &mut [u8]) -> Result<usize, Self::Err> {
        self.table.read(addr.off, out_buf).ok_or_else(|| {
            strider_reader::MemReadError::from(anyhow::anyhow!(
                "address {:#x} is not mapped",
                addr.off
            ))
        })
    }
}

/// Fill-all-or-error: a partial or unmapped range errors.
impl ReadOnlyMemory for PyBufferReaderView {
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        self.table.read_exact(addr, buf)
    }
}

/// The memory argument every Python entry point accepts: either a
/// `BufferReader` or any Python object with a `read(...)` method.
pub enum MemInput {
    Buffer(PyBufferReader),
    Cb(std::sync::Arc<Py<PyAny>>),
}

impl<'py> FromPyObject<'py> for MemInput {
    fn extract_bound(ob: &Bound<'py, PyAny>) -> PyResult<Self> {
        if let Ok(m) = ob.extract::<PyBufferReader>() {
            return Ok(MemInput::Buffer(m));
        }
        if ob.hasattr("read")? {
            return Ok(MemInput::Cb(std::sync::Arc::new(ob.clone().unbind())));
        }
        Err(pyo3::exceptions::PyTypeError::new_err(
            "expected a BufferReader or an object with a `read(...)` method",
        ))
    }
}

impl MemInput {
    /// The Python callback object backing this input, if any.  Shares the
    /// exact reference the adapter holds, so registering it for cyclic-GC
    /// traversal does not inflate the object's refcount.
    pub fn py_callback(&self) -> Option<std::sync::Arc<Py<PyAny>>> {
        match self {
            MemInput::Cb(obj) => Some(std::sync::Arc::clone(obj)),
            MemInput::Buffer(_) => None,
        }
    }
}

impl MemInput {
    /// This input in the rom role.
    pub fn into_box(self) -> Box<dyn ReadOnlyMemory> {
        match self {
            MemInput::Buffer(m) => Box::new(m.reader_view()),
            MemInput::Cb(obj) => Box::new(PyReadOnlyMemoryAdapter { py_obj: obj }),
        }
    }

    pub fn into_any(self) -> AnyMemReader {
        match self {
            MemInput::Buffer(m) => AnyMemReader::Buffer(m.reader_view()),
            MemInput::Cb(obj) => AnyMemReader::Cb(PyMemReaderAdapter { py_obj: obj }),
        }
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyBufferReader>()?;
    // `_LoadedElf` is deliberately unregistered: its methods are bound on the
    // type object regardless, so `load_elf_from_*` still hands back a fully
    // usable instance.
    m.add_class::<PyMemReader>()?;
    m.add_class::<PyReadOnlyMemory>()?;
    Ok(())
}
