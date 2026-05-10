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

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use object::{Object, ObjectSymbol};
use pyo3::prelude::*;
use pyo3::types::PyBytes;

use crate::errors::into_reader_err;
use reader::{MemRegion, MemRegionsLookupTable, ReadOnlyMemory};

// ── PyRelocationStats — return type for apply_elf_relocations ────────────

/// Counts from a single `apply_elf_relocations` run.  Mirrors the
/// fields of `reader::elf::RelocationStats` one-to-one.  Useful for
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

impl From<reader::elf::RelocationStats> for PyRelocationStats {
    fn from(s: reader::elf::RelocationStats) -> Self {
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
    /// Loaded ELF objects, in `add_region_from_elf` insertion order.
    /// Kept around so `symbol(name)` / `symbols()` can resolve names
    /// without forcing the user to re-parse the file via pyelftools.
    /// `object::File<'static>` borrows from a leaked byte slice (see
    /// `reader::load_elf`), so storing it here is sound.
    elfs: Arc<RwLock<Vec<object::File<'static>>>>,
    /// Byte order used by `ReadOnlyMemory::read` when assembling
    /// multi-byte words from the underlying buffer.  Defaults to
    /// [`target::Endianness::Little`]; auto-set from the ELF header in
    /// `add_region_from_elf`, or set explicitly via
    /// [`Self::set_endianness`].  Stored behind an Arc/RwLock so a
    /// `PyMemoryMap` clone shares the same setting.
    endianness: Arc<RwLock<target::Endianness>>,
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
    ///
    /// Recovers from RwLock poisoning via `into_inner()` — the table's
    /// inner state is `Option<Arc<...>>` (an atomic-pointer slot), so
    /// the only way it could be inconsistent after a panicking writer
    /// is to be partially overwritten, which `*slot = Some(...)` cannot
    /// do.  Recovery is therefore safe and matches the read-side
    /// semantic: a partial-write panic leaves the prior value intact
    /// or replaces it atomically.
    pub(crate) fn lookup_table(&self) -> anyhow::Result<Arc<MemRegionsLookupTable>> {
        let slot = self.table.read().unwrap_or_else(|p| p.into_inner());
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
            elfs: Arc::new(RwLock::new(Vec::new())),
            // Default to LE; overridden by add_region_from_elf or
            // set_endianness once the user supplies a real arch.
            endianness: Arc::new(RwLock::new(target::Endianness::Little)),
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
            "little" | "le" => target::Endianness::Little,
            "big" | "be" => target::Endianness::Big,
            other => {
                return Err(into_reader_err(anyhow::anyhow!(
                    "unknown endianness {other:?}; use \"little\" or \"big\""
                )));
            }
        };
        let mut slot = self
            .endianness
            .write()
            .map_err(|_| into_reader_err(anyhow::anyhow!("MemoryMap endianness lock poisoned")))?;
        *slot = parsed;
        Ok(())
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
    ///
    /// `apply_relocations` defaults to `False`.  Set it to `True` for
    /// ET_DYN binaries (FreeBSD kernels, PIE userland) whose `.text`
    /// or function-pointer tables ship with unresolved relocations
    /// (`call rel32` placeholders, `R_*_RELATIVE` entries):
    /// `reader::elf::apply_elf_relocations` runs over the loaded
    /// regions in-place after the section copy and patches every
    /// relocation it understands.  See the function's doc-comment
    /// for the supported kinds.
    #[pyo3(signature = (path, apply_relocations=false))]
    fn add_region_from_elf(&self, path: &str, apply_relocations: bool) -> PyResult<()> {
        let obj = reader::load_elf(path).map_err(into_reader_err)?;
        // Auto-set the byte order from the ELF header so subsequent
        // ReadOnlyMemory::read calls assemble multi-byte words in the
        // right order (big-endian targets like MIPS-BE / PowerPC-BE
        // would otherwise byte-swap their LoadReadOnly constants).
        let elf_endian = match object::Object::endianness(&obj) {
            object::Endianness::Little => target::Endianness::Little,
            object::Endianness::Big => target::Endianness::Big,
        };
        {
            let mut slot = self.endianness.write().map_err(|_| {
                into_reader_err(anyhow::anyhow!("MemoryMap endianness lock poisoned"))
            })?;
            *slot = elf_endian;
        }
        // When apply_relocations=True we widen the section coverage
        // to include `.data.rel.ro`, `.got`, …  Without the widening
        // a `R_*_RELATIVE` relocation against a writable-but-
        // relocated table has nowhere to land (the applier reports
        // skipped_no_region) and the analysis sees zeros where the
        // function pointers should be.
        let regions = if apply_relocations {
            let (regions, _stats) =
                reader::elf::elf_load_with_relocations(&obj).map_err(into_reader_err)?;
            regions
        } else {
            reader::elf::elf_get_code_and_readonly_sections_as_mem_regions(&obj)
                .map_err(into_reader_err)?
        };
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
        // Cache the ELF for subsequent symbol() / symbols() / entry_point() calls.
        let mut elfs = self
            .elfs
            .write()
            .map_err(|_| into_reader_err(anyhow::anyhow!("MemoryMap elfs lock poisoned")))?;
        elfs.push(obj);
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
        let obj = reader::load_elf(path).map_err(into_reader_err)?;
        let mut regions = self
            .inner
            .write()
            .map_err(|_| into_reader_err(anyhow::anyhow!("MemoryMap regions lock poisoned")))?;
        let stats = reader::elf::apply_elf_relocations_autoload(&mut regions, &obj)
            .map_err(into_reader_err)?;
        // Invalidate the lookup table — both the autoload step
        // (which appends new regions) and the in-place patches
        // require a rebuild before the next read.
        let mut slot = self
            .table
            .write()
            .map_err(|_| into_reader_err(anyhow::anyhow!("MemoryMap table lock poisoned")))?;
        *slot = None;
        Ok(stats.into())
    }

    /// Look up the address of a function/data symbol across every ELF
    /// loaded into this MemoryMap via `add_region_from_elf`.  Returns
    /// the first match in load order; raises `ReaderError` when no
    /// loaded ELF defines the name.
    fn symbol(&self, name: &str) -> PyResult<u64> {
        let elfs = self
            .elfs
            .read()
            .map_err(|_| into_reader_err(anyhow::anyhow!("MemoryMap elfs lock poisoned")))?;
        for obj in elfs.iter() {
            if let Some(sym) = obj.symbol_by_name(name) {
                return Ok(sym.address());
            }
        }
        Err(into_reader_err(anyhow::anyhow!(
            "symbol {name:?} not found in any ELF loaded into this MemoryMap \
             ({} loaded)", elfs.len()
        )))
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
        let elfs = self
            .elfs
            .read()
            .map_err(|_| into_reader_err(anyhow::anyhow!("MemoryMap elfs lock poisoned")))?;
        for obj in elfs.iter() {
            if let Some(sym) = obj.symbol_by_name(name) {
                let size = sym.size();
                return Ok(if size == 0 { None } else { Some(size) });
            }
        }
        Err(into_reader_err(anyhow::anyhow!(
            "symbol {name:?} not found in any ELF loaded into this MemoryMap \
             ({} loaded)", elfs.len()
        )))
    }

    /// Convenience shortcut for the common
    /// `(symbol(name), symbol_size(name))` pair — returns
    /// `(addr, size)` so callers don't need two lookups.  `size` is
    /// `None` when the ELF doesn't record one (zero `st_size`).
    /// Raises `ReaderError` when the symbol is undefined.
    fn function_max_size(&self, name: &str) -> PyResult<(u64, Option<u64>)> {
        let elfs = self
            .elfs
            .read()
            .map_err(|_| into_reader_err(anyhow::anyhow!("MemoryMap elfs lock poisoned")))?;
        for obj in elfs.iter() {
            if let Some(sym) = obj.symbol_by_name(name) {
                let size = sym.size();
                return Ok((sym.address(), if size == 0 { None } else { Some(size) }));
            }
        }
        Err(into_reader_err(anyhow::anyhow!(
            "symbol {name:?} not found in any ELF loaded into this MemoryMap \
             ({} loaded)", elfs.len()
        )))
    }

    /// All function/data symbols across every loaded ELF as a
    /// `dict[str, int]`.  Symbols with empty names or zero addresses
    /// (typical for synthetic linker entries) are skipped.  When two
    /// ELFs define the same name, the earlier-loaded one wins.
    fn symbols(&self) -> PyResult<HashMap<String, u64>> {
        let elfs = self
            .elfs
            .read()
            .map_err(|_| into_reader_err(anyhow::anyhow!("MemoryMap elfs lock poisoned")))?;
        let mut out: HashMap<String, u64> = HashMap::new();
        for obj in elfs.iter() {
            for sym in obj.symbols() {
                let Ok(name) = sym.name() else { continue };
                if name.is_empty() || sym.address() == 0 {
                    continue;
                }
                out.entry(name.to_string()).or_insert(sym.address());
            }
        }
        Ok(out)
    }

    /// ELF entry-point address from the first loaded ELF.  Raises
    /// `ReaderError` when no ELF has been loaded yet.
    fn entry_point(&self) -> PyResult<u64> {
        let elfs = self
            .elfs
            .read()
            .map_err(|_| into_reader_err(anyhow::anyhow!("MemoryMap elfs lock poisoned")))?;
        let first = elfs.first().ok_or_else(|| {
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
    type Err = reader::MemReadError;

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
        .map_err(reader::MemReadError::from)
    }
}

// ── PyReadOnlyMemory (callback ABC) ──────────────────────────────────────

/// Python-subclassable abstract base for `LoadReadOnly`.  Subclasses
/// override `read(addr, size) -> Optional[int]` returning the
/// little-endian-decoded value or `None` for unmapped.
///
/// The Rust trait `reader::ReadOnlyMemory::read(VnSpace, addr, size)`
/// takes a varnode-space because the IR's `Load` nodes carry one
/// (`Load(VnSpace::REGISTER)` is a register read; `Load(VnSpace::RAM)`
/// is a memory read).  In practice the `LoadReadOnly` pass only
/// fires on RAM loads — every other space is either a register read
/// (folded via varnode aliasing) or a constant/unique value (no rom
/// involved) — so the Python ABC narrows the surface to RAM only.
/// Non-RAM reads return `None` automatically without ever calling
/// the user's `read` method.
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
    fn read(&self, space: rsleigh::VnSpace, addr: u64, size: usize) -> Option<u64> {
        // The trait carries `space` for symmetry with the IR's `Load`
        // nodes, but a Python rom only ever needs to model RAM (every
        // other space is folded by varnode aliasing or constant
        // propagation before reaching `LoadReadOnly`).  Skip the
        // GIL acquisition entirely for non-RAM reads so the Python
        // override sees only the calls it can answer.
        if space != rsleigh::VnSpace::RAM {
            return None;
        }
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
            // silently absorbed.  Mirrors the wrap_when fix from round 9
            // wave 31 (H-8).
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
    type Err = reader::MemReadError;

    fn read(&self, addr: rsleigh::VnAddr, out_buf: &mut [u8]) -> Result<usize, Self::Err> {
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
    type Err = reader::MemReadError;

    fn read(&self, addr: rsleigh::VnAddr, out_buf: &mut [u8]) -> Result<usize, Self::Err> {
        self.table
            .read(addr.off, out_buf)
            .ok_or_else(|| {
                reader::MemReadError(anyhow::anyhow!("address {:#x} is not mapped", addr.off))
            })
    }
}

/// `ReadOnlyMemory` impl reading 1/2/4/8-byte words from any space.
/// Mirrors `reader::ElfFileMemReader`'s endianness-aware decoding so
/// big-endian targets (MIPS-BE / PowerPC-BE / AArch64-BE) get correct
/// `LoadReadOnly` constants.  Endianness is auto-set by
/// `add_region_from_elf` (or explicitly via `set_endianness`); defaults
/// to little for raw-bytes-only construction.
impl ReadOnlyMemory for PyMemoryMap {
    fn read(&self, space: rsleigh::VnSpace, addr: u64, size: usize) -> Option<u64> {
        // PyMemoryMap models RAM only — it has no backing for REGISTER /
        // CONST / UNIQUE / OTHER spaces.  Reject non-RAM reads up front
        // so a misrouted read (e.g. a Load whose space is REGISTER but
        // whose address happens to fall inside a loaded RAM region)
        // doesn't return RAM bytes.  Mirrors `PyReadOnlyMemoryAdapter::read`
        // which gates on space at the FFI boundary.
        if space != rsleigh::VnSpace::RAM {
            return None;
        }
        if size == 0 || size > 8 {
            return None;
        }
        let table = self.lookup_table().ok()?;
        let mut buf = [0u8; 8];
        let n = table.read(addr, &mut buf[..size])?;
        if n != size {
            return None;
        }
        // Layout `buf` so that `Endianness::read_u64` decodes the
        // size-byte payload correctly.  LE: bytes already in low slots.
        // BE: shift bytes to the high end so from_be_bytes treats the
        // payload as a widened N-byte BE word.
        //
        // Recover from poisoning rather than silently failing — the inner
        // is `target::Endianness` (Copy), and `*guard = new_endianness`
        // is atomic, so a partial-write panic cannot leave the slot
        // half-initialised.
        let endianness = *self.endianness.read().unwrap_or_else(|p| p.into_inner());
        let layout = match endianness {
            target::Endianness::Little => buf,
            target::Endianness::Big => {
                let mut be_buf = [0u8; 8];
                be_buf[8 - size..].copy_from_slice(&buf[..size]);
                be_buf
            }
        };
        Some(endianness.read_u64(layout))
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

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyMemoryMap>()?;
    m.add_class::<PyMemReader>()?;
    m.add_class::<PyReadOnlyMemory>()?;
    m.add_class::<PyRelocationStats>()?;
    Ok(())
}
