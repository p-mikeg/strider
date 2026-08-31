use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use object::{Object, ObjectSymbol};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};

use crate::errors::into_strider_err;
use strider_reader::elf::{ElfSectionLayout, LoadFilter, RegionSource};
use strider_reader::{MemRegion, MemRegionsLookupTable, ReadOnlyMemory};

/// The GIL already serialises every access; `Mutex` is here so a wrapper
/// holding one is `Send`, which is what lets a `Function` outlive the thread
/// that built it. Poisoning is recovered from rather than propagated: the
/// guarded data is plain and no panic can leave it half-written.
trait LockShared<T> {
    fn lock_shared(&self) -> MutexGuard<'_, T>;
}

impl<T> LockShared<T> for Mutex<T> {
    fn lock_shared(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub(crate) struct PyBufferReaderInner {
    pub(crate) regions: Vec<MemRegion>,
    /// Lazily rebuilt; cleared on every region change.
    pub(crate) table: Option<Arc<MemRegionsLookupTable>>,
    /// Bytes in the largest single region, built with `table`.
    max_region_len: Option<usize>,
}

/// Raw byte reader for firmware or custom sources.  Works as both the
/// `mem` (instruction fetch) and `rom` (read-only memory) argument.  Cheap
/// to clone; clones share state with the original.
#[pyclass(name = "BufferReader", module = "strider.reader")]
#[derive(Clone)]
pub struct PyBufferReader {
    pub(crate) inner: Arc<Mutex<PyBufferReaderInner>>,
}

impl PyBufferReader {
    pub(crate) fn lookup_table(&self) -> Arc<MemRegionsLookupTable> {
        if let Some(t) = self.inner.lock_shared().table.as_ref() {
            return Arc::clone(t);
        }
        let mut inner = self.inner.lock_shared();
        let t = Arc::new(MemRegionsLookupTable::new(inner.regions.clone()));
        inner.max_region_len = Some(
            inner
                .regions
                .iter()
                .map(|r| (r.end_addr() - r.start_addr()) as usize)
                .max()
                .unwrap_or(0),
        );
        inner.table = Some(Arc::clone(&t));
        t
    }

    /// Point-in-time snapshot implementing both `rsleigh::MemReader` and
    /// `ReadOnlyMemory`.
    pub(crate) fn reader_view(&self) -> PyBufferReaderView {
        let table = self.lookup_table();
        PyBufferReaderView { table }
    }

    /// Upper bound on what one `MemRegionsLookupTable::read` can return; the
    /// table itself publishes no such bound.
    fn read_bound(&self) -> usize {
        if let Some(n) = self.inner.lock_shared().max_region_len {
            return n;
        }
        drop(self.lookup_table());
        self.inner.lock_shared().max_region_len.unwrap_or(0)
    }

    pub(crate) fn from_regions(regions: Vec<MemRegion>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PyBufferReaderInner {
                regions,
                table: None,
                max_region_len: None,
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

    fn __repr__(&self) -> String {
        format!(
            "BufferReader({} region(s))",
            self.inner.lock_shared().regions.len()
        )
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
        table.check_unchanged().map_err(into_strider_err)?;
        // `table.read` fills from one region only, so a multi-exabyte `size`
        // would allocate (and OOM) for nothing.
        let mut buf = vec![0u8; size.min(self.read_bound())];
        match table.read(addr, &mut buf) {
            Some(n) => {
                buf.truncate(n);
                Ok(Some(PyBytes::new_bound(py, &buf)))
            }
            None => Ok(None),
        }
    }
}

/// One stat per distinct mapping behind `mem_obj`, before anything decodes
/// through it. A reader that is not a `BufferReader` maps nothing and cannot
/// go stale.
pub(crate) fn check_mem_unchanged(py: Python<'_>, mem_obj: &Option<Py<PyAny>>) -> PyResult<()> {
    let Some(obj) = mem_obj else { return Ok(()) };
    let Ok(buf) = obj.extract::<PyBufferReader>(py) else {
        return Ok(());
    };
    buf.lookup_table()
        .check_unchanged()
        .map_err(into_strider_err)
}

/// Whether a symbol carries an address of its own. A linked image uses
/// `st_value == 0` for synthetic linker entries; an object file's `st_value`
/// is section-relative, so zero is a real address there and definedness is the
/// test instead.
fn symbol_is_addressed<'d, S: ObjectSymbol<'d>>(sym: &S, relocatable: bool) -> bool {
    // A TLS symbol's value is an offset into the per-thread block, so it has
    // no address in this space whichever kind of object holds it.
    if sym.kind() == object::SymbolKind::Tls {
        return false;
    }
    if relocatable {
        matches!(sym.section(), object::SymbolSection::Section(_))
    } else {
        sym.address() != 0
    }
}

/// `Segments` auto-dispatches: PT_LOAD headers for ET_EXEC / ET_DYN,
/// falling back to the section walker for ET_REL (no program headers).
/// `Sections` forces the section walk even on a linked binary that does
/// carry PT_LOAD segments.
#[derive(Clone, Copy)]
pub(crate) enum ElfRegionSource {
    Segments,
    Sections,
}

impl From<ElfRegionSource> for RegionSource {
    fn from(source: ElfRegionSource) -> Self {
        match source {
            ElfRegionSource::Segments => RegionSource::Auto,
            ElfRegionSource::Sections => RegionSource::Sections,
        }
    }
}

/// Instruction fetch / raw reads: writable sections are included only when
/// relocations are applied, so a relocated `.got` / `.data.rel.ro` is readable.
fn elf_to_mem_regions(
    elf: &strider_reader::OwnedElf,
    source: ElfRegionSource,
    apply_relocations: bool,
) -> PyResult<Vec<MemRegion>> {
    let filter = if apply_relocations {
        LoadFilter::AllAllocatable
    } else {
        LoadFilter::CodeAndReadOnly
    };
    elf.regions(source.into(), filter, apply_relocations)
        .map_err(into_strider_err)
}

/// Code + read-only sections only; writable ones (`.data`, `.got`,
/// `.data.rel.ro`) are EXCLUDED, so every address here is
/// runtime-immutable.
///
/// Shares one backing buffer with [`elf_to_mem_regions`]: the ROM is a filter
/// over the same bytes.
fn elf_to_rom_regions(
    elf: &strider_reader::OwnedElf,
    source: ElfRegionSource,
    apply_relocations: bool,
) -> PyResult<Vec<MemRegion>> {
    elf.regions(source.into(), LoadFilter::ImmutableOnly, apply_relocations)
        .map_err(into_strider_err)
}

/// Parsed ELF binary.  Construct via `strider.lift.load_elf(path)`.
#[pyclass(name = "_LoadedElf", module = "strider.reader")]
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
    /// ELFs consulted for symbols only, never for bytes: a debug companion is
    /// linked at the same addresses as the image it describes, so merging it
    /// through `add_elf` would collide with what is already mapped.
    symbol_elfs: Vec<strider_reader::OwnedElf>,
    /// Symbols supplied directly, which win nothing: they are appended last and
    /// an earlier source keeps the name.
    extra_symbols: Vec<PySymbol>,
    /// Every symbol of every source, built on the first symbol query and
    /// dropped whenever a source is added.
    symbol_table: Mutex<Option<SymbolTable>>,
}

fn invalidate_and_extend(reader: &PyBufferReader, regions: Vec<MemRegion>) {
    let mut inner = reader.inner.lock_shared();
    inner.regions.extend(regions);
    inner.table = None;
    inner.max_region_len = None;
}

/// A sub-range where a `new` region overlaps an `existing` one with DIFFERENT
/// bytes, or `None` if every overlap is byte-identical (a benign re-merge of
/// the same image). See `add_elf` for why differing overlap is rejected.
///
/// `existing` is start-ordered once so a `new` region probes only the prefix
/// whose running maximum end still reaches it, rather than every region.
fn differing_overlap(existing: &[MemRegion], new: &[MemRegion]) -> Option<(u64, u64)> {
    let mut order: Vec<usize> = (0..existing.len()).collect();
    order.sort_unstable_by_key(|&i| existing[i].start_addr());
    // Running maximum end address over the start-ordered prefix: the walk back
    // from a `new` region's upper bound stops once no earlier region reaches it.
    let mut max_end: Vec<u64> = Vec::with_capacity(order.len());
    let mut reach = 0u64;
    for &i in &order {
        reach = reach.max(existing[i].end_addr());
        max_end.push(reach);
    }
    for n in new {
        let (n_lo, n_hi) = (n.start_addr(), n.end_addr());
        let mut rank = order.partition_point(|&i| existing[i].start_addr() < n_hi);
        while rank > 0 && max_end[rank - 1] > n_lo {
            let e = &existing[order[rank - 1]];
            let lo = n_lo.max(e.start_addr());
            let hi = n_hi.min(e.end_addr());
            if lo < hi && !n.same_bytes_in(e, lo, hi) {
                return Some((lo, hi));
            }
            rank -= 1;
        }
    }
    None
}

/// One ELF symbol: where it is, what the ELF says it spans, and which loaded
/// region it falls in.
#[pyclass(name = "Symbol", module = "strider.reader", frozen)]
#[derive(Clone)]
pub struct PySymbol {
    /// The symbol name as spelled in the ELF symbol table.
    #[pyo3(get)]
    name: String,
    /// The symbol's virtual address (`st_value`).
    #[pyo3(get)]
    address: u64,
    /// `None` for `st_size == 0`, which records no extent rather than an
    /// empty one: a hand-written `.S` entry point with no `.size` directive
    /// is still a whole function.
    #[pyo3(get)]
    size: Option<u64>,
    is_function: bool,
    region: Option<(u64, u64)>,
}

#[pymethods]
impl PySymbol {
    /// Whether the ELF types this `STT_FUNC` (or `STT_GNU_IFUNC`), rather
    /// than inferring it from the section the symbol lives in.
    #[getter]
    fn is_function(&self) -> bool {
        self.is_function
    }

    /// One past the last byte, or `None` when `size` is.
    #[getter]
    fn end(&self) -> Option<u64> {
        self.size.map(|s| self.address.saturating_add(s))
    }

    /// The `(start, end)` bounds (end exclusive) of the loaded region this
    /// symbol maps into, such as the `.text` mapping, or `None` when its
    /// address falls in no mapped region.
    #[getter]
    fn region(&self) -> Option<(u64, u64)> {
        self.region
    }

    fn __repr__(&self) -> String {
        match self.size {
            Some(size) => format!(
                "Symbol(name={:?}, address={:#x}, size={})",
                self.name, self.address, size
            ),
            None => format!("Symbol(name={:?}, address={:#x})", self.name, self.address),
        }
    }
}

/// Every named, addressed symbol of every loaded ELF, with a by-name and a
/// by-address lookup over it.
struct SymbolTable {
    /// Load order, then symbol-table order within an ELF.
    syms: Vec<PySymbol>,
    /// The winner for each name; two symbols can share one.
    by_name: HashMap<String, usize>,
    /// Indices into `syms`, ascending by address, ties in load order.
    by_addr: Vec<usize>,
    /// Prefix maximum of the covered end over `by_addr`, so a backward scan
    /// stops as soon as nothing at or below can still reach the address.
    by_addr_max_end: Vec<u64>,
}

impl PyLoadedElf {
    /// Building parses the mapping, so a file that changed underneath is an
    /// error here rather than a SIGBUS. A table already built needs no mapping
    /// and costs no stat.
    fn with_symbols<T>(&self, f: impl FnOnce(&SymbolTable) -> T) -> PyResult<T> {
        let stale = self.symbol_table.lock_shared().is_none();
        if stale {
            self.check_unchanged()?;
            let built = self.build_symbol_table();
            *self.symbol_table.lock_shared() = Some(built);
        }
        let table = self.symbol_table.lock_shared();
        Ok(f(table.as_ref().expect("just built")))
    }

    /// One name can have several symbols: FreeBSD's `model_name` is both an
    /// STT_FUNC in `.text` and an STT_OBJECT in `.rodata`, so a code symbol
    /// wins within an ELF, and the first ELF in load order wins across them.
    fn build_symbol_table(&self) -> SymbolTable {
        let mem = self.mem.inner.lock_shared();
        // Region indices by ascending start, with a prefix maximum of each
        // region's end.  An `ET_REL` object carries one region per SHF_ALLOC
        // section, so scanning them per symbol is quadratic in a
        // `-ffunction-sections` build where both axes grow together.
        let mut by_start: Vec<usize> = (0..mem.regions.len()).collect();
        by_start.sort_unstable_by_key(|&i| mem.regions[i].start_addr());
        let mut reach: Vec<u64> = Vec::with_capacity(by_start.len());
        let mut furthest = 0u64;
        for &i in &by_start {
            furthest = furthest.max(mem.regions[i].end_addr());
            reach.push(furthest);
        }
        // The highest start among the regions covering `address`, which is how
        // `MemRegionsLookupTable::read` resolves an overlap; taking slice order
        // could name a region that never serves these bytes.
        let region_of = |address: u64| -> Option<(u64, u64)> {
            let hi = by_start.partition_point(|&i| mem.regions[i].start_addr() <= address);
            for slot in (0..hi).rev() {
                // Nothing at or below `slot` reaches `address` any more.
                if reach[slot] <= address {
                    break;
                }
                let r = &mem.regions[by_start[slot]];
                if r.contains(address) {
                    return Some((r.start_addr(), r.end_addr()));
                }
            }
            None
        };
        let mut syms: Vec<PySymbol> = Vec::new();
        let mut by_name: HashMap<String, usize> = HashMap::new();
        for obj in self.elfs.iter().chain(&self.symbol_elfs) {
            let file = obj.file();
            let layout = ElfSectionLayout::new(&file);
            let relocatable = file.kind() == object::ObjectKind::Relocatable;
            let mut per_elf: HashMap<String, usize> = HashMap::new();
            // `.symtab` and `.dynsym` overlap: an exported symbol is in both, and
            // only `iter_symbols` would show it twice.
            let mut seen: std::collections::HashSet<(String, u64)> =
                std::collections::HashSet::new();
            for sym in file.symbols().chain(file.dynamic_symbols()) {
                let Ok(name) = sym.name() else { continue };
                if name.is_empty() || !symbol_is_addressed(&sym, relocatable) {
                    continue;
                }
                let address = layout.symbol_address(&sym);
                if !seen.insert((name.to_string(), address)) {
                    continue;
                }
                let ix = syms.len();
                syms.push(PySymbol {
                    name: name.to_string(),
                    address,
                    size: (sym.size() != 0).then(|| sym.size()),
                    is_function: sym.kind() == object::SymbolKind::Text,
                    region: region_of(address),
                });
                match per_elf.entry(name.to_string()) {
                    std::collections::hash_map::Entry::Occupied(mut o) => {
                        if syms[ix].is_function && !syms[*o.get()].is_function {
                            o.insert(ix);
                        }
                    }
                    std::collections::hash_map::Entry::Vacant(v) => {
                        v.insert(ix);
                    }
                }
            }
            for (name, ix) in per_elf {
                by_name.entry(name).or_insert(ix);
            }
        }
        // Hand-supplied symbols land last, so a name any ELF already carries
        // keeps the ELF's answer; `symbol_at` still sees these by address.
        for extra in &self.extra_symbols {
            let ix = syms.len();
            syms.push(PySymbol {
                region: region_of(extra.address),
                ..extra.clone()
            });
            by_name.entry(extra.name.clone()).or_insert(ix);
        }
        let mut by_addr: Vec<usize> = (0..syms.len()).collect();
        by_addr.sort_by_key(|&i| syms[i].address);
        let mut by_addr_max_end: Vec<u64> = Vec::with_capacity(by_addr.len());
        let mut running = 0u64;
        for &i in &by_addr {
            running = running.max(covered_end(&syms[i]));
            by_addr_max_end.push(running);
        }
        SymbolTable {
            syms,
            by_name,
            by_addr,
            by_addr_max_end,
        }
    }
}

/// One past the last address `sym` covers. A symbol with no recorded size
/// covers only its own address.
fn covered_end(sym: &PySymbol) -> u64 {
    sym.address.saturating_add(sym.size.unwrap_or(1))
}

/// The symbol of `group` covering `address`. Aliases sharing an address are
/// ranked by recorded extent first, then by being code, which is the order
/// `functions()` uses, so the two accessors agree on the same address.
fn covering<'a>(group: impl Iterator<Item = &'a PySymbol>, address: u64) -> Option<&'a PySymbol> {
    let rank = |s: &PySymbol| (s.size.is_some(), s.is_function);
    let mut best: Option<&PySymbol> = None;
    for sym in group {
        let covers = sym.address <= address && address < covered_end(sym);
        if covers && best.is_none_or(|b| rank(sym) > rank(b)) {
            best = Some(sym);
        }
    }
    best
}

#[pymethods]
impl PyLoadedElf {
    /// Re-stat every mapped file, erroring if one changed since it was mapped.
    /// A rebuild between two operations is caught here; a change racing a read
    /// already in progress is still a torn read or a SIGBUS.
    fn check_unchanged(&self) -> PyResult<()> {
        for elf in &self.elfs {
            elf.check_unchanged().map_err(into_strider_err)?;
        }
        Ok(())
    }

    /// The instruction-fetch / raw-read `BufferReader` for this ELF.
    fn reader(&self) -> PyBufferReader {
        self.mem.clone()
    }

    /// The runtime-immutable `BufferReader` (code + read-only sections
    /// only).
    fn ro_reader(&self) -> PyBufferReader {
        self.rom.clone()
    }

    /// The `Symbol` named `name`, taking the first ELF in load order that
    /// defines it and preferring a code symbol over a data one of the same
    /// name.  Raises `StriderError` when no loaded ELF defines it.
    fn symbol(&self, name: &str) -> PyResult<PySymbol> {
        self.symbol_opt(name)?.ok_or_else(|| {
            into_strider_err(anyhow::anyhow!(
                "symbol {name:?} not found in any ELF loaded into this Program \
                 ({} loaded)",
                self.elfs.len()
            ))
        })
    }

    /// `symbol`, but `None` rather than raising when `name` is undefined.
    fn symbol_opt(&self, name: &str) -> PyResult<Option<PySymbol>> {
        self.with_symbols(|t| t.by_name.get(name).map(|&ix| t.syms[ix].clone()))
    }

    /// The symbol covering `address`: the nearest one at or below it whose
    /// recorded extent reaches `address`.  A symbol with no recorded size
    /// covers only its own address, so a size-less one sitting inside a sized
    /// function (every ARM `$a` / `$d` mapping symbol) does not hide it.
    /// Aliases sharing an address resolve by recorded extent, then by being
    /// code.  `None` when nothing covers `address`.
    fn symbol_at(&self, address: u64) -> PyResult<Option<PySymbol>> {
        self.with_symbols(|t| {
            let mut hi = t.by_addr.partition_point(|&i| t.syms[i].address <= address);
            while hi > 0 {
                // Nothing at or below this point extends far enough.
                if t.by_addr_max_end[hi - 1] <= address {
                    return None;
                }
                let base = t.syms[t.by_addr[hi - 1]].address;
                let lo = t.by_addr[..hi].partition_point(|&i| t.syms[i].address < base);
                if let Some(hit) = covering(t.by_addr[lo..hi].iter().map(|&i| &t.syms[i]), address)
                {
                    return Some(hit.clone());
                }
                hi = lo;
            }
            None
        })
    }

    /// Whether the first loaded ELF sets ARM's `EF_ARM_BE8`: instructions are
    /// stored little-endian while data stays big-endian.
    ///
    /// `EI_DATA` marks a BE8 image and a BE32 one alike, so the flag is the
    /// only thing separating them. Reported, not acted on: nothing here selects
    /// an arch from it. `False` off ARM.
    #[getter]
    fn is_arm_be8(&self) -> PyResult<bool> {
        // Reads the header out of the mapping.
        self.check_unchanged()?;
        Ok(self
            .elfs
            .first()
            .is_some_and(strider_reader::OwnedElf::is_arm_be8))
    }

    /// Every symbol across every loaded ELF as `dict[str, Symbol]`, keyed by
    /// the name each one resolves under.
    fn symbols(&self) -> PyResult<HashMap<String, PySymbol>> {
        self.with_symbols(|t| {
            t.by_name
                .iter()
                .map(|(name, &ix)| (name.clone(), t.syms[ix].clone()))
                .collect()
        })
    }

    /// The function symbols in address order, one per address: aliases of an
    /// address already listed are excluded, preferring the one whose size the
    /// ELF records.  A function with no recorded size is still yielded, with
    /// `Symbol.size` `None`.
    fn functions(&self) -> PyResult<PySymbolIter> {
        let syms = self.with_symbols(|t| {
            let mut out: Vec<PySymbol> = Vec::new();
            for &ix in &t.by_addr {
                let sym = &t.syms[ix];
                if !sym.is_function {
                    continue;
                }
                match out.last_mut() {
                    Some(prev) if prev.address == sym.address => {
                        if prev.size.is_none() && sym.size.is_some() {
                            *prev = sym.clone();
                        }
                    }
                    _ => out.push(sym.clone()),
                }
            }
            out
        })?;
        Ok(PySymbolIter { syms, next: 0 })
    }

    /// Every symbol pulled one at a time: a `Symbol` is built only when
    /// pulled, so the Python objects are never all live at once.  The Rust
    /// table is collected up front.
    fn iter_symbols(&self) -> PyResult<PySymbolIter> {
        Ok(PySymbolIter {
            syms: self.with_symbols(|t| t.syms.clone())?,
            next: 0,
        })
    }

    /// ELF entry-point address from the first loaded ELF.
    fn entry_point(&self) -> PyResult<u64> {
        // `load_elf` always pushes one ELF, so `first()` is never `None`.
        self.elfs.first().map_or(Ok(0), |o| {
            o.checked_file()
                .map(|f| f.entry())
                .map_err(into_strider_err)
        })
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
    ///
    /// Errors if the new ELF maps code over an address already loaded with
    /// DIFFERENT bytes: `add_elf` places shared objects at distinct addresses,
    /// so two ELFs linked at the same base cannot share one address space (a
    /// byte-identical re-merge of the same image is allowed and is a no-op).
    #[pyo3(signature = (path, apply_relocations=false))]
    fn add_elf(&mut self, path: &str, apply_relocations: bool) -> PyResult<()> {
        let obj = strider_reader::load_elf(path).map_err(into_strider_err)?;
        let mem_regions = elf_to_mem_regions(&obj, self.source, apply_relocations)?;
        let rom_regions = elf_to_rom_regions(&obj, self.source, apply_relocations)?;
        if let Some((lo, hi)) =
            differing_overlap(&self.mem.inner.lock_shared().regions, &mem_regions)
        {
            return Err(into_strider_err(anyhow::anyhow!(
                "add_elf: {path} maps [{lo:#x}, {hi:#x}) with bytes that differ from what is \
                 already loaded there; add_elf merges shared objects at DISTINCT addresses, so \
                 two ELFs linked at the same base cannot be merged into one address space"
            )));
        }
        invalidate_and_extend(&self.mem, mem_regions);
        invalidate_and_extend(&self.rom, rom_regions);
        self.elfs.push(obj);
        self.symbol_table.lock_shared().take();
        Ok(())
    }

    /// Take the symbols of `path` and none of its bytes.
    ///
    /// This is how a separate debug or symbol file attaches: `objcopy
    /// --only-keep-debug` output and distro debuginfo are linked at the same
    /// addresses as the image they describe, so `add_elf` would refuse them as
    /// an overlap. Both `.symtab` and `.dynsym` are read, as for any ELF.
    ///
    /// The already-loaded ELFs keep a colliding name; addresses that fall
    /// outside every mapped region are still recorded, and simply have no
    /// region attached.
    fn add_symbol_file(&mut self, path: &str) -> PyResult<()> {
        let obj = strider_reader::load_elf(path).map_err(into_strider_err)?;
        self.symbol_elfs.push(obj);
        self.symbol_table.lock_shared().take();
        Ok(())
    }

    /// Add symbols directly, for names that live in no ELF at all: a map file,
    /// a kernel `System.map`, a database, or your own naming.
    ///
    /// `symbols` maps a name to an address, or to a `(address, size)` pair
    /// when the extent is known. `is_function` marks them all as code, which
    /// is what `functions()` iterates. An ELF already carrying a name keeps
    /// its own answer for that name.
    #[pyo3(signature = (symbols, *, is_function=true))]
    fn add_symbols(&mut self, symbols: &Bound<'_, PyDict>, is_function: bool) -> PyResult<()> {
        for (name, value) in symbols.iter() {
            let name: String = name.extract()?;
            let (address, size) = if let Ok((a, n)) = value.extract::<(u64, u64)>() {
                (a, (n != 0).then_some(n))
            } else {
                (value.extract::<u64>()?, None)
            };
            self.extra_symbols.push(PySymbol {
                name,
                address,
                size,
                is_function,
                region: None,
            });
        }
        self.symbol_table.lock_shared().take();
        Ok(())
    }
}

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
        symbol_elfs: Vec::new(),
        extra_symbols: Vec::new(),
        mem,
        rom,
        source,
        symbol_table: Mutex::new(None),
    })
}

/// Load the ELF at `path`, collecting regions from PT_LOAD program
/// headers (falling back to the section walker for header-less ET_REL).
///
/// Set `apply_relocations` for ET_DYN binaries (kernels, PIE userland)
/// whose `.text` or function-pointer tables ship with unresolved
/// relocations: section coverage widens to `.data.rel.ro` / `.got` and
/// every understood relocation is applied.
#[pyfunction]
#[pyo3(name = "_load_elf_from_segments", signature = (path, apply_relocations=false))]
pub fn load_elf_from_segments(path: &str, apply_relocations: bool) -> PyResult<PyLoadedElf> {
    load_elf_impl(path, ElfRegionSource::Segments, apply_relocations)
}

/// Load the ELF at `path`, collecting regions by walking section headers
/// even when the binary carries PT_LOAD segments.
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
    ///
    /// An exception here fails the whole lift, chained as the `StriderError`'s
    /// `__cause__`.  `ReadOnlyMemory.read` is the opposite: see its docstring.
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

    fn __repr__(slf: Bound<'_, Self>) -> PyResult<String> {
        let name: String = slf.get_type().getattr("__name__")?.extract()?;
        Ok(format!("{name}()"))
    }
}

/// Shared `read`-callback prologue for both Python reader adapters, run
/// inside the caller's `Python::with_gil`.
///
/// `KeyboardInterrupt` / `SystemExit` are STASHED, not `PyErr::restore`d:
/// restoring leaves the error indicator set, so the next callback trips
/// CPython's "returned a result with an exception set" guard and destroys
/// the original.  A stash short-circuits every later call until the outer
/// boundary drains the cell.
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
    if crate::pattern::peek_pending_query_error() {
        anyhow::bail!("{abort_label} aborted: pending control-flow exception");
    }
    crate::errors::clear_callback_cause();
    match py_obj.call_method1(py, "read", args) {
        Ok(r) => Ok(r),
        Err(e) => {
            if e.is_instance_of::<pyo3::exceptions::PyKeyboardInterrupt>(py)
                || e.is_instance_of::<pyo3::exceptions::PySystemExit>(py)
            {
                crate::pattern::stash_pending_query_error(e);
                anyhow::bail!("{abort_label} aborted: control-flow exception stashed");
            }
            crate::errors::stash_callback_cause(e.clone_ref(py));
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
            // `MemReader::read` has no unmapped variant, so None becomes an
            // error.
            if result.is_none(py) {
                anyhow::bail!(
                    "address {:#x} is not mapped (Python read returned None)",
                    addr.off
                );
            }
            // Every instruction fetch lands here, so `bytes` (the documented
            // return) is read borrowed; anything else still converts.
            let bound = result.bind(py);
            let owned;
            let bytes: &[u8] = match bound.downcast::<pyo3::types::PyBytes>() {
                Ok(b) => b.as_bytes(),
                Err(_) => {
                    owned = bound
                        .extract::<Vec<u8>>()
                        .map_err(|e| anyhow::anyhow!("PyMemReader.read must return bytes: {e}"))?;
                    &owned
                }
            };
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
            out_buf[..n].copy_from_slice(bytes);
            Ok(n)
        })
        .map_err(strider_reader::MemReadError::from)
    }
}

/// Read-only memory backed by Python, read by the constant-load fold
/// (`LoadReadOnly`) and by the indirect-branch classifier's jump-table probes.
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
    ///
    /// An exception here is swallowed: the constant-load fold declines and
    /// analysis continues, indistinguishable from `None`.  Unlike
    /// `MemReader.read`, whose exception fails the lift.  `KeyboardInterrupt`
    /// and `SystemExit` are the exceptions that still stop the run.
    #[allow(unused_variables)]
    fn read(&self, addr: u64, size: usize) -> PyResult<Option<Vec<u8>>> {
        Err(pyo3::exceptions::PyNotImplementedError::new_err(
            "ReadOnlyMemory.read must be overridden by subclass",
        ))
    }

    fn __repr__(slf: Bound<'_, Self>) -> PyResult<String> {
        let name: String = slf.get_type().getattr("__name__")?.extract()?;
        Ok(format!("{name}()"))
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
            let result = call_py_read(py, &self.py_obj, (addr, size), "read", |e| {
                anyhow::anyhow!("ReadOnlyMemory.read({addr:#x}, {size}) raised: {e}")
            })?;
            if result.is_none(py) {
                anyhow::bail!("ReadOnlyMemory.read({addr:#x}, {size}) returned None (unmapped)");
            }
            let bound = result.bind(py);
            let owned;
            let bytes: &[u8] = match bound.downcast::<pyo3::types::PyBytes>() {
                Ok(b) => b.as_bytes(),
                Err(_) => {
                    owned = bound.extract::<Vec<u8>>().map_err(|e| {
                        anyhow::anyhow!(
                            "ReadOnlyMemory.read({addr:#x}, {size}) did not return bytes: {e}"
                        )
                    })?;
                    &owned
                }
            };
            if bytes.len() != size {
                anyhow::bail!(
                    "ReadOnlyMemory.read({addr:#x}, {size}) returned {} bytes, expected {size}",
                    bytes.len()
                );
            }
            buf.copy_from_slice(bytes);
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
    // `_LoadedElf` stays out of the module namespace; its methods are bound on
    // the type object regardless, so `load_elf_from_*` still hands back a fully
    // usable instance.
    m.add_class::<PySymbol>()?;
    m.add_class::<PySymbolIter>()?;
    m.add_class::<PyMemReader>()?;
    m.add_class::<PyReadOnlyMemory>()?;
    Ok(())
}

/// Yields `Symbol`s one at a time.
#[pyo3::pyclass(name = "SymbolIter", module = "strider.reader")]
pub struct PySymbolIter {
    syms: Vec<PySymbol>,
    next: usize,
}

#[pyo3::pymethods]
impl PySymbolIter {
    fn __iter__(slf: pyo3::PyRef<'_, Self>) -> pyo3::PyRef<'_, Self> {
        slf
    }

    fn __next__(&mut self) -> Option<PySymbol> {
        let out = self.syms.get(self.next).cloned();
        if out.is_some() {
            self.next += 1;
        }
        out
    }

    /// What is LEFT, not the total: CPython takes this as a length hint, so a
    /// partly consumed iterator would over-allocate.
    fn __len__(&self) -> usize {
        self.syms.len() - self.next
    }
}
