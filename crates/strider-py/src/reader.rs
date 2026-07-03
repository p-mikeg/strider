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
/// `ElfLifter`), which builds the (multi-region) reader from the ELF
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
    /// Returns a snapshot of the current lookup table, building it on
    /// demand if invalidated.  Used internally by both `read` and the
    /// `MemReader` view supplied to `Sleigh::new`.
    pub(crate) fn lookup_table(&self) -> Arc<MemRegionsLookupTable> {
        if let Some(t) = self.inner.borrow().table.as_ref() {
            return Arc::clone(t);
        }
        let mut inner = self.inner.borrow_mut();
        let t = Arc::new(MemRegionsLookupTable::new(inner.regions.clone()));
        inner.table = Some(Arc::clone(&t));
        t
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

    /// Longest contiguous run of bytes mapped from `addr` across all
    /// regions (`0` when `addr` is unmapped).  Used to clamp the read
    /// allocation against an unbounded caller-supplied `size` so a huge
    /// request never allocates more than is actually mapped.
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
        // Clamp the allocation against an unbounded Python-supplied
        // `size`: `table.read` only ever fills the bytes that are
        // actually mapped from `addr`, so a multi-exabyte `size` would
        // allocate (and abort/OOM) for nothing.  Cap at the longest
        // mapped run starting at `addr`.
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

// ── _LoadedElf (ELF parse + symbols, built by load_elf_from_*) ───────────

/// Which ELF region-collection strategy `_LoadedElf` was built with.
///
/// `Segments` is the auto-dispatching path strider has always used:
/// PT_LOAD program headers for ET_EXEC / ET_DYN, falling back to the
/// section-walker for ET_REL (which has no program headers at all) —
/// see `strider_reader::elf::elf_get_loadable_regions`'s kind dispatch.
/// `Sections` FORCES the section-header walk (first-wins VMA dedup)
/// even for a linked ET_EXEC/ET_DYN binary that does carry PT_LOAD
/// segments — `strider.load_elf_from_sections`'s strategy.
#[derive(Clone, Copy)]
pub(crate) enum ElfRegionSource {
    Segments,
    Sections,
}

/// Load an ELF's code + read-only (and, when `apply_relocations`, the
/// relocated-data) sections into the instruction-fetch / raw-read `mem`
/// region list, applying every understood relocation in-place when
/// requested.  Shared by `load_elf_from_segments` /
/// `load_elf_from_sections` and `_LoadedElf::add_elf`.
fn elf_to_mem_regions(
    obj: &object::File<'static>,
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
                strider_reader::elf::elf_get_loadable_regions_sections_only_including_writable(
                    obj,
                )
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

/// Parsed ELF binary: the friendly face is the Python `ElfLifter`
/// returned by `strider.load_elf(...)` / `load_elf_from_segments(...)` /
/// `load_elf_from_sections(...)`, which wraps one of these.
///
/// Holds the parsed `object::File`(s) (in load order — the first wins
/// on symbol-name collisions) plus two internal raw `BufferReader`s
/// built from the ELF sections (with relocations applied per the
/// `apply_relocations` flag): a writable-inclusive `mem` reader for
/// instruction fetch / raw reads (`reader()`), and a runtime-immutable
/// `rom` reader (code + read-only only) for `LoadReadOnly` constant
/// folding (`ro_reader()`).  The leading underscore marks it as
/// internal-by-convention: construct it via `strider.load_elf(path)`
/// (or the explicit `load_elf_from_segments` / `load_elf_from_sections`)
/// and reach for `ElfLifter` for the user-facing surface.
#[pyclass(name = "_LoadedElf", module = "strider", unsendable)]
pub struct PyLoadedElf {
    /// Loaded ELF objects, in `load_elf` / `add_elf` insertion order.
    /// `object::File<'static>` borrows from a leaked byte slice (see
    /// `strider_reader::load_elf`), so storing it here is sound.
    elfs: Vec<object::File<'static>>,
    /// Instruction-fetch / raw-read reader assembled from the ELF
    /// sections (writable sections included when relocations are
    /// applied).  Handed to `strider.lifter(arch, mem=…)` via `reader()`.
    mem: PyBufferReader,
    /// Runtime-immutable reader (code + read-only sections only,
    /// writable sections EXCLUDED).  Handed to `strider.lifter(arch, mem,
    /// rom=…)` via `ro_reader()`: the `LoadReadOnly` rom MUST be
    /// runtime-immutable because the fold trusts it unconditionally.
    rom: PyBufferReader,
    /// The region-collection strategy this handle was built with
    /// (`load_elf_from_segments` vs `load_elf_from_sections`).  Reused
    /// by `add_elf` so a later merge stays consistent with the
    /// strategy the caller originally picked.
    source: ElfRegionSource,
}

/// Returns `Some(s)` when `s != 0`, `None` otherwise — used to map
/// a zero ELF `st_size` to "unknown" and a positive size to its value.
fn nonzero_size(s: u64) -> Option<u64> {
    (s != 0).then_some(s)
}

/// Extend `reader`'s region list with `regions` and invalidate its
/// cached lookup table.  Used by `add_elf` for both the mem and rom
/// readers so the two identical borrow_mut + extend + table=None blocks
/// don't need to be written twice.
fn invalidate_and_extend(reader: &PyBufferReader, regions: Vec<MemRegion>) {
    let mut inner = reader.inner.borrow_mut();
    inner.regions.extend(regions);
    inner.table = None;
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
    /// `strider.lifter(arch, mem=…)`.
    fn reader(&self) -> PyBufferReader {
        self.mem.clone()
    }

    /// The **runtime-immutable** `BufferReader` (code + read-only
    /// sections only — writable `.data` / `.got` / `.data.rel.ro`
    /// EXCLUDED).  Pass it to `strider.lifter(arch, mem, rom=…)`: the
    /// `LoadReadOnly` rom MUST be runtime-immutable, because the fold
    /// replaces a constant-address load with the resolved bytes WITHOUT
    /// consulting the memory chain.
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
    /// argument for `Lifter.analyze` / `Lifter.build_cfg`.
    fn symbol_size(&self, name: &str) -> PyResult<Option<u64>> {
        self.find_symbol(name, |sym| nonzero_size(sym.size()))
    }

    /// Convenience shortcut for the `(symbol(name), symbol_size(name))`
    /// pair — returns `(addr, size)` so callers don't need two lookups.
    /// `size` is `None` when the ELF doesn't record one (zero
    /// `st_size`).  Raises `StriderError` when the symbol is undefined.
    /// The `size` half is exactly what `Lifter.analyze`'s
    /// `function_max_size=` keyword expects.
    fn symbol_addr_and_size(&self, name: &str) -> PyResult<(u64, Option<u64>)> {
        self.find_symbol(name, |sym| (sym.address(), nonzero_size(sym.size())))
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
        let mem_regions = elf_to_mem_regions(&obj, self.source, apply_relocations)?;
        let rom_regions = elf_to_rom_regions(&obj, self.source, apply_relocations)?;
        invalidate_and_extend(&self.mem, mem_regions);
        invalidate_and_extend(&self.rom, rom_regions);
        self.elfs.push(obj);
        Ok(())
    }
}

/// Shared body of `load_elf_from_segments` / `load_elf_from_sections`:
/// parses the ELF at `path` and builds a `_LoadedElf` whose `mem` / `rom`
/// readers are assembled with `source`'s region-collection strategy.
fn load_elf_impl(
    path: &str,
    source: ElfRegionSource,
    apply_relocations: bool,
) -> PyResult<PyLoadedElf> {
    let obj = strider_reader::load_elf(path).map_err(into_strider_err)?;
    let mem = PyBufferReader::from_regions(elf_to_mem_regions(&obj, source, apply_relocations)?);
    let rom = PyBufferReader::from_regions(elf_to_rom_regions(&obj, source, apply_relocations)?);
    Ok(PyLoadedElf {
        elfs: vec![obj],
        mem,
        rom,
        source,
    })
}

/// Load an ELF binary from `path` into a `_LoadedElf` (the parsed
/// object the high-level `ElfLifter` wraps), collecting regions by
/// walking **PT_LOAD program headers** (the runtime memory layout) for
/// ET_EXEC / ET_DYN binaries — falling back to the section-walker (with
/// first-wins VMA dedup) for ET_REL objects, which carry no program
/// headers at all.  This is the strategy `strider.load_elf` (and every
/// prior version of the loader) has always used.
///
/// `apply_relocations` defaults to `False`.  Set it to `True` for
/// ET_DYN binaries (kernels, PIE userland) whose `.text` or
/// function-pointer tables ship with unresolved relocations: the
/// widened section coverage (`.data.rel.ro`, `.got`, …) is loaded and
/// every understood relocation is patched in-place.
#[pyfunction]
#[pyo3(signature = (path, apply_relocations=false))]
pub fn load_elf_from_segments(path: &str, apply_relocations: bool) -> PyResult<PyLoadedElf> {
    load_elf_impl(path, ElfRegionSource::Segments, apply_relocations)
}

/// Load an ELF binary from `path` into a `_LoadedElf`, collecting
/// regions by walking **section headers** (first-wins VMA dedup) —
/// bypassing the PT_LOAD path even for a linked ET_EXEC / ET_DYN binary
/// that does carry program headers.  Use this when you want
/// section-granular regions (`.text` / `.rodata` / `.plt` as separate
/// mappings) instead of the segment loader's coalesced PT_LOAD ranges.
///
/// `apply_relocations` defaults to `False`, with the same semantics as
/// `load_elf_from_segments`.
#[pyfunction]
#[pyo3(signature = (path, apply_relocations=false))]
pub fn load_elf_from_sections(path: &str, apply_relocations: bool) -> PyResult<PyLoadedElf> {
    load_elf_impl(path, ElfRegionSource::Sections, apply_relocations)
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
    fn new(
        _args: &Bound<'_, pyo3::types::PyTuple>,
        _kwargs: Option<&Bound<'_, pyo3::types::PyDict>>,
    ) -> Self {
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

/// Shared `read`-callback prologue for both Python reader adapters.
///
/// Runs inside the caller's `Python::with_gil`.  Performs:
/// 1. the PENDING_CONTROL_FLOW short-circuit (a prior call already stashed a
///    control-flow exception — stop calling into Python so we don't trip
///    CPython's "returned a result with an exception set" guard; the outer
///    boundary drains the cell + surfaces the saved PyErr);
/// 2. the `py_obj.read(*args)` call;
/// 3. control-flow-exception classification: `KeyboardInterrupt` /
///    `SystemExit` are stashed (NOT `PyErr::restore`, so the next invocation
///    doesn't see a set error indicator) and bail; every other error bails
///    with the caller-supplied message.
///
/// Returns the raw result object; each adapter keeps its own divergent tail
/// (None handling, length checks, copy semantics).  `args` is the
/// already-built argument tuple so per-adapter arg encoding stays at the call
/// site.  `abort_label` prefixes the two abort messages (e.g. `"MemReader.read"`
/// → `"MemReader.read aborted: …"`); `raise_msg` formats the non-control-flow
/// error from the caught `PyErr`.
///
/// This stash logic is soundness-critical — it lives here in ONE place.
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

/// Internal adapter: holds a `Py<PyAny>` (the user's Python subclass)
/// and implements `rsleigh::MemReader` by `Python::with_gil` per call.
pub struct PyMemReaderAdapter {
    pub py_obj: Py<PyAny>,
}

/// Manual (not `#[derive]`) `Clone`: `Py<T>: Clone` requires the
/// `py-clone` pyo3 feature (not enabled here), so cloning the
/// underlying `Py<PyAny>` needs a `Python::with_gil` (the same pattern
/// this struct's own `MemReader::read` impl below already uses).  This
/// is what makes `AnyMemReader: Clone`, which in turn lets
/// `rsleigh::Sleigh<AnyMemReader>: Clone` mint a fresh, independent
/// Sleigh context (fresh underlying engine state, cloned reader) — see
/// `PyLifter::pcode_at`'s throwaway-Sleigh build.
impl Clone for PyMemReaderAdapter {
    fn clone(&self) -> Self {
        Python::with_gil(|py| Self {
            py_obj: self.py_obj.clone_ref(py),
        })
    }
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
            // None → not mapped (return Err so the matcher falls through).
            if result.is_none(py) {
                anyhow::bail!(
                    "address {:#x} is not mapped (Python read returned None)",
                    addr.off
                );
            }
            let bytes = result
                .extract::<Vec<u8>>(py)
                .map_err(|e| anyhow::anyhow!("PyMemReader.read must return bytes: {e}"))?;
            // Short reads near a region edge are legitimate (the
            // `MemReader` contract allows them), but an *over-long*
            // return is a Python bug — reject it rather than silently
            // dropping the excess (mirrors the strict check in
            // `PyReadOnlyMemoryAdapter::read`).
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
    fn new(
        _args: &Bound<'_, pyo3::types::PyTuple>,
        _kwargs: Option<&Bound<'_, pyo3::types::PyDict>>,
    ) -> Self {
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
            // The Python override returns the RAW `size` bytes at `addr`
            // (`bytes`) or `None` for unmapped.  Control-flow exceptions
            // (KeyboardInterrupt / SystemExit) are stashed so the outer
            // boundary surfaces them; every other failure errors here so
            // `LoadReadOnly` simply leaves the Load intact.
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

// ── AnyMemReader — unified Rust reader type ──────────────────────────────

/// Unified `MemReader` used by every downstream Python wrapper
/// (PySleigh, PyCfg, PyStrider, …).  Constructed from either a
/// `PyBufferReader` snapshot (fast in-process path) or a
/// `PyMemReaderAdapter` (callback into a Python subclass).
///
/// `Clone` (both variants are cheap clones — an `Arc` bump or a
/// `Py<PyAny>` refcount bump) is what makes
/// `rsleigh::Sleigh<AnyMemReader>: Clone` available: cloning a `Sleigh`
/// builds a brand-new underlying engine context from `(sla_spec, pspec,
/// cloned reader)` — a genuinely fresh, independent instance, not a
/// shared one — which is exactly the "fresh, throwaway Sleigh" a
/// re-lift for `Lifter.pcode_at` needs (see its doc comment).
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
///
/// `Clone` is a cheap `Arc` bump (see `PyMemReaderAdapter`'s doc for why
/// that matters: it's what lets `AnyMemReader` — and thus
/// `rsleigh::Sleigh<AnyMemReader>` — be `Clone`).
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
/// Consumed in two modes:
/// - [`into_box`](Self::into_box) — lift to `Box<dyn ReadOnlyMemory>`
///   for the ROM-style pipeline pass.
/// - [`into_any`](Self::into_any) — materialise into the unified
///   `AnyMemReader` (used to build a `Sleigh`).
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
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyBufferReader>()?;
    // NOTE: `PyLoadedElf` (`_LoadedElf`) is deliberately NOT registered as
    // a public Python class — it is an internal ELF parse / symbol backend
    // owned by the Python `ElfLifter`.  The `load_elf_from_segments` /
    // `load_elf_from_sections` pyfunctions below are the only seam: each
    // returns a fully-usable `_LoadedElf` instance (its pyclass methods
    // are bound on the type object regardless of module registration)
    // that `_api.py` wraps inside an `ElfLifter`.
    m.add_class::<PyMemReader>()?;
    m.add_class::<PyReadOnlyMemory>()?;
    m.add_function(wrap_pyfunction!(load_elf_from_segments, m)?)?;
    m.add_function(wrap_pyfunction!(load_elf_from_sections, m)?)?;
    Ok(())
}
