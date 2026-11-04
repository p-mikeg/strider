use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use object::{Object, ObjectSymbol};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};

use crate::errors::into_strider_err;
use strider_reader::elf::{
    AddressSet, ElfSectionLayout, OpdTable, RegionSource, RelocationMode, without_ranges,
};
use strider_reader::{MemRegion, MemRegionsLookupTable, ReadOnlyMemory, RegionIndex};

/// A `#[pyclass]` must be `Send`, and `PyBufferReader`'s regions mutate
/// through `&self`, so they sit behind a `Mutex` rather than a `RefCell`.
/// Every access holds the GIL (a GIL-free `analyze` reads a lock-free
/// `reader_view` snapshot); never hold a guard across a call that can release
/// it. Poisoning is ignored: no panic can leave the plain data half-written.
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
        let mut inner = self.inner.lock_shared();
        if let Some(t) = inner.table.as_ref() {
            return Arc::clone(t);
        }
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

    pub(crate) fn from_regions(regions: Vec<MemRegion>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PyBufferReaderInner {
                regions,
                table: None,
            })),
        }
    }
}

#[pymethods]
impl PyBufferReader {
    /// Create a reader over a single raw-byte region: `data` mapped at
    /// `base_addr`.  Raises `ValueError` for a region that runs past the end
    /// of the address space.
    #[new]
    fn new(base_addr: u64, data: &Bound<'_, PyAny>) -> PyResult<Self> {
        let data = match data.extract::<pyo3::pybacked::PyBackedBytes>() {
            Ok(bytes) => bytes.to_vec(),
            Err(_) => data.extract::<Vec<u8>>()?,
        };
        let region = MemRegion::new(base_addr, data)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
        Ok(Self::from_regions(vec![region]))
    }

    fn __repr__(&self) -> String {
        format!(
            "BufferReader({} region(s))",
            self.inner.lock_shared().regions.len()
        )
    }

    /// Read up to `size` bytes starting at `addr`.  Returns the bytes
    /// (possibly fewer than `size` near a region edge, or before a relocation
    /// this loader does not compute) or `None` when `addr` is unmapped or
    /// inside such a relocation's field. In writable, non-executable data
    /// such a field is not cut out: it serves the file's bytes.
    fn read<'py>(
        &self,
        py: Python<'py>,
        addr: u64,
        size: usize,
    ) -> PyResult<Option<Bound<'py, PyBytes>>> {
        let table = self.lookup_table();
        table.check_unchanged().map_err(into_strider_err)?;
        Ok(table
            .read_vec(addr, size)
            .map(|buf| PyBytes::new_bound(py, &buf)))
    }
}

/// Errors if a file mapped behind a `BufferReader` in `objs` changed, before
/// anything decodes through it.
pub(crate) fn check_mem_unchanged(
    py: Python<'_>,
    objs: &[&Option<Arc<Py<PyAny>>>],
) -> PyResult<()> {
    let tables: Vec<_> = objs
        .iter()
        .filter_map(|obj| obj.as_ref()?.extract::<PyBufferReader>(py).ok())
        .map(|buf| buf.lookup_table())
        .collect();
    MemRegionsLookupTable::check_all_unchanged(tables.iter().map(|t| &**t))
        .map_err(into_strider_err)
}

/// Where a symbol sits in this address space, if anywhere.
enum SymbolPlace {
    Defined,
    /// An undefined symbol still carrying an address: a linked image's PLT
    /// stub, or the synthetic one an object file's layout gives it.
    Import(u64),
    Nowhere,
}

fn symbol_place<'d, S: ObjectSymbol<'d>>(sym: &S, layout: &ElfSectionLayout) -> SymbolPlace {
    // A TLS symbol's value is an offset into the per-thread block.
    if sym.kind() == object::SymbolKind::Tls {
        return SymbolPlace::Nowhere;
    }
    match sym.section() {
        object::SymbolSection::Section(_) => SymbolPlace::Defined,
        // `SHN_ABS` 0 is how a linked image spells a synthetic entry, such as
        // an `STT_FILE` name.
        object::SymbolSection::Absolute if sym.address() != 0 => SymbolPlace::Defined,
        object::SymbolSection::Undefined | object::SymbolSection::Common => {
            match layout.extern_address(sym.index().0) {
                Some(addr) => SymbolPlace::Import(addr),
                None if sym.is_undefined() && sym.address() != 0 => {
                    SymbolPlace::Import(sym.address())
                }
                None => SymbolPlace::Nowhere,
            }
        }
        _ => SymbolPlace::Nowhere,
    }
}

/// What `apply_relocations` and `relocated_got_is_rom` claim about the image.
/// Relocated, fetch maps every writable section too; `relocated_got_is_rom`
/// serves `.got`, `.got2` and `.data.rel.ro` (never `.got.plt`) as ROM, on
/// the claim nothing rewrites them after the loader, less the slots only the
/// loader computes. In any mode, a relocation against a defined IFUNC in an
/// executable mapping folds to the symbol's address, not the resolver's
/// result. Refuses `relocated_got_is_rom` without `apply_relocations`.
fn relocation_mode(
    apply_relocations: bool,
    relocated_got_is_rom: bool,
) -> PyResult<RelocationMode> {
    match (apply_relocations, relocated_got_is_rom) {
        (true, got_is_rom) => Ok(RelocationMode::Applied { got_is_rom }),
        (false, false) => Ok(RelocationMode::Unapplied),
        (false, true) => Err(into_strider_err(anyhow::anyhow!(
            "relocated_got_is_rom=True claims the loader has already relocated \
             `.got` / `.data.rel.ro`, and apply_relocations=False says it has not. \
             Served unrelocated, those slots read as their on-disk bytes, which is 0 \
             on a PIE (the address lives in the relocation addend), and a dispatch \
             through one folds to address 0 with nothing reported unresolved. Pass \
             apply_relocations=True, or relocated_got_is_rom=False"
        ))),
    }
}

/// Parsed ELF binary.  Construct via `strider.lift.load_elf(path)`.
#[pyclass(name = "_LoadedElf", module = "strider.reader")]
pub struct PyLoadedElf {
    /// Load order; the first wins on symbol-name collisions.
    elfs: Vec<strider_reader::OwnedElf>,
    /// Every address a loaded ELF maps writable, which `rom` never serves.
    writable: AddressSet,
    /// Instruction fetch / raw reads; includes writable sections when
    /// relocations were applied.
    mem: PyBufferReader,
    /// The runtime-immutable subset.
    rom: PyBufferReader,
    source: RegionSource,
    /// ELFs consulted for symbols only, never for bytes.
    symbol_elfs: Vec<strider_reader::OwnedElf>,
    /// Symbols supplied directly, appended last so an ELF keeps a colliding
    /// name.
    extra_symbols: Vec<PySymbol>,
    memo: ElfMemo,
}

/// Built over every source on first use, dropped when a source it reads is
/// added.
#[derive(Default)]
struct ElfMemo {
    symbols: OnceLock<SymbolTable>,
    /// The `$d` mapping-symbol spans.
    data_ranges: OnceLock<strider_cfg::DataRanges>,
    local_entries: OnceLock<Arc<strider_cfg::LocalEntries>>,
}

fn invalidate_and_extend(reader: &PyBufferReader, regions: Vec<MemRegion>) {
    let mut inner = reader.inner.lock_shared();
    inner.regions.extend(regions);
    inner.table = None;
}

fn region_spans(regions: &[MemRegion]) -> Vec<(u64, u64)> {
    regions
        .iter()
        .map(|r| (r.start_addr(), r.end_addr()))
        .collect()
}

/// Every address `new` maps is mapped by `existing`.
fn regions_covered(existing: &[MemRegion], new: &[MemRegion]) -> bool {
    let mapped = AddressSet::merged(region_spans(existing));
    region_spans(new)
        .iter()
        .all(|&(lo, hi)| mapped.covers(lo, hi))
}

/// A sub-range where `new` serves different bytes than `existing` at an
/// address both map, or `None` if every such address reads the same.
///
/// Each address resolves as a read does, through the highest-start covering
/// region, which only changes at a region boundary, so the walk is over the
/// intervals between boundaries.
fn differing_overlap(existing: &[MemRegion], new: &[MemRegion]) -> Option<(u64, u64)> {
    let (old_index, new_index) = (RegionIndex::new(existing), RegionIndex::new(new));
    let mut old_bounds: Vec<u64> = existing
        .iter()
        .flat_map(|r| [r.start_addr(), r.end_addr()])
        .collect();
    old_bounds.sort_unstable();
    let mut spans: Vec<(u64, u64)> = new.iter().map(|r| (r.start_addr(), r.end_addr())).collect();
    spans.sort_unstable();
    let mut cuts: Vec<u64> = Vec::new();
    let mut covered_to = 0u64;
    for (lo, hi) in spans {
        cuts.extend([lo, hi]);
        // Each old boundary is taken by the first span reaching it.
        let from = lo.max(covered_to);
        if from < hi {
            let first = old_bounds.partition_point(|&b| b <= from);
            let last = old_bounds.partition_point(|&b| b < hi);
            cuts.extend(&old_bounds[first..last.max(first)]);
            covered_to = hi;
        }
    }
    cuts.sort_unstable();
    cuts.dedup();
    cuts.windows(2).map(|w| (w[0], w[1])).find(|&(lo, hi)| {
        match (
            old_index.covering(lo, 1).next(),
            new_index.covering(lo, 1).next(),
        ) {
            (Some(o), Some(n)) => !new[n].same_bytes_in(&existing[o], lo, hi),
            _ => false,
        }
    })
}

/// One ELF symbol: where it is, what the ELF says it spans, and which loaded
/// region it falls in. Equal when every field is.
#[pyclass(name = "Symbol", module = "strider.reader", frozen, eq, hash)]
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PySymbol {
    /// The symbol name as spelled in the ELF symbol table.
    #[pyo3(get)]
    name: String,
    /// The symbol's virtual address (`st_value`), except for a ppc64 ELFv1
    /// function, where it is the code the `.opd` descriptor at `st_value`
    /// names.
    #[pyo3(get)]
    pub(crate) address: u64,
    /// `None` for `st_size == 0` (no recorded extent), and for a ppc64 ELFv1
    /// function whose `st_size` measures its `.opd` descriptor rather than its
    /// code.
    #[pyo3(get)]
    pub(crate) size: Option<u64>,
    is_function: bool,
    /// An ARM Thumb function, whose `address` keeps the ISA bit.
    thumb: bool,
    /// Where a call sharing the symbol's TOC enters: `address` plus the ppc64
    /// ELFv2 local-entry offset `st_other` encodes, else `address` itself.
    #[pyo3(get)]
    local_entry: u64,
    region: Option<(u64, u64)>,
}

impl PySymbol {
    /// The first byte the symbol covers: `address` without a Thumb bit.
    fn start(&self) -> u64 {
        self.address & !u64::from(self.thumb)
    }

    /// One past the last address covered; an unsized symbol covers one byte.
    fn covered_end(&self) -> u64 {
        self.start().saturating_add(self.size.unwrap_or(1))
    }
}

#[pymethods]
impl PySymbol {
    /// Whether the ELF types this `STT_FUNC` (or `STT_GNU_IFUNC`), rather
    /// than inferring it from the section the symbol lives in.
    #[getter]
    fn is_function(&self) -> bool {
        self.is_function
    }

    /// One past the last byte, or `None` when `size` is. Measured from the
    /// first instruction, so a Thumb function's ISA bit does not count.
    #[getter]
    fn end(&self) -> Option<u64> {
        self.size.map(|s| self.start().saturating_add(s))
    }

    /// Whether this is an ARM Thumb function: `address` then carries the
    /// Thumb bit, on which `analyze` enters in Thumb mode, and the first
    /// instruction is at `address & ~1`.
    #[getter]
    fn is_thumb(&self) -> bool {
        self.thumb
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
    /// Indices into `syms`, ascending by first byte, ties in load order.
    by_addr: Vec<usize>,
    /// Which symbols cover an address, over `[start, covered_end)`.
    extents: RegionIndex,
}

/// A build id as the hex string every tool prints it in.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl PyLoadedElf {
    /// Refuses a symbol file that describes a different binary: by build id
    /// when both carry one, else by architecture.
    fn check_describes_a_loaded_image(
        &self,
        candidate: &strider_reader::OwnedElf,
        path: &str,
    ) -> PyResult<()> {
        let Some(image) = self.elfs.first() else {
            return Ok(());
        };
        let image = image.checked_file().map_err(into_strider_err)?;
        let cand = candidate.checked_file().map_err(into_strider_err)?;
        use object::read::Object as _;

        if let (Ok(Some(a)), Ok(Some(b))) = (image.build_id(), cand.build_id()) {
            if a != b {
                return Err(into_strider_err(anyhow::anyhow!(
                    "{path}: build id {} does not match the loaded image's {}; \
                     it describes a different binary",
                    hex(b),
                    hex(a),
                )));
            }
            return Ok(());
        }
        if image.architecture() != cand.architecture() {
            return Err(into_strider_err(anyhow::anyhow!(
                "{path}: architecture {:?} does not match the loaded image's {:?}",
                cand.architecture(),
                image.architecture(),
            )));
        }
        Ok(())
    }

    /// The architecture, class, byte order and ARM `EF_ARM_BE8` of `candidate`
    /// match the first loaded image's: one handle decodes with one arch.
    fn check_same_target(&self, candidate: &strider_reader::OwnedElf, path: &str) -> PyResult<()> {
        use object::read::Object as _;
        let Some(image) = self.elfs.first() else {
            return Ok(());
        };
        let target = |elf: &strider_reader::OwnedElf| -> anyhow::Result<String> {
            let file = elf.checked_file()?;
            Ok(format!(
                "{:?} {}-bit {}-endian{}",
                file.architecture(),
                if file.is_64() { 64 } else { 32 },
                if file.is_little_endian() {
                    "little"
                } else {
                    "big"
                },
                if elf.is_arm_be8()? { " BE8" } else { "" },
            ))
        };
        let (want, got) = (
            target(image).map_err(into_strider_err)?,
            target(candidate).map_err(into_strider_err)?,
        );
        if want != got {
            return Err(into_strider_err(anyhow::anyhow!(
                "add_elf: {path} is {got}, the loaded image {want}; one handle \
                 decodes one target"
            )));
        }
        Ok(())
    }

    /// Merge another ELF (e.g. a shared library) into this one, extending
    /// the regions and symbol set. The decoder then rebuilds over what `symbol`
    /// and `read` see. The earlier-loaded ELF wins a name collision, and
    /// `relocated_got_is_rom` is this image's own claim.
    ///
    /// Errors if the new ELF's architecture, class, byte order or ARM
    /// `EF_ARM_BE8` differs from the first image's, or if it maps an address
    /// already loaded with different bytes. Re-merging an image already fully
    /// loaded does nothing.
    pub(crate) fn add_elf(
        &mut self,
        path: &str,
        apply_relocations: bool,
        relocated_got_is_rom: bool,
    ) -> PyResult<()> {
        let elf = strider_reader::load_elf(path).map_err(into_strider_err)?;
        self.check_same_target(&elf, path)?;
        let mode = relocation_mode(apply_relocations, relocated_got_is_rom)?;
        let image = elf.image(self.source, mode).map_err(into_strider_err)?;
        let rom_regions = without_ranges(image.rom, &self.writable);
        if let Some((lo, hi)) =
            differing_overlap(&self.mem.inner.lock_shared().regions, &image.fetch)
        {
            return Err(into_strider_err(anyhow::anyhow!(
                "add_elf: {path} maps [{lo:#x}, {hi:#x}) with bytes that differ from what is \
                 already loaded there; add_elf merges shared objects at DISTINCT addresses, so \
                 two ELFs linked at the same base cannot be merged into one address space"
            )));
        }
        // Already loaded: merging would append a second copy. An image mapping
        // nothing still goes through, for its symbols.
        if !image.fetch.is_empty()
            && regions_covered(&self.mem.inner.lock_shared().regions, &image.fetch)
            && image
                .writable
                .as_slice()
                .iter()
                .all(|&(lo, hi)| self.writable.covers(lo, hi))
        {
            return Ok(());
        }
        invalidate_and_extend(&self.mem, image.fetch);
        {
            let mut rom = self.rom.inner.lock_shared();
            let kept = without_ranges(std::mem::take(&mut rom.regions), &image.writable);
            rom.regions = kept;
        }
        invalidate_and_extend(&self.rom, rom_regions);
        self.writable = AddressSet::merged(
            self.writable
                .as_slice()
                .iter()
                .chain(image.writable.as_slice())
                .copied()
                .collect(),
        );
        self.elfs.push(elf);
        self.memo = ElfMemo::default();
        Ok(())
    }

    /// Stats the mappings before a build, so a changed file errors rather than
    /// SIGBUSes.
    fn with_symbols<T>(&self, f: impl FnOnce(&SymbolTable) -> T) -> PyResult<T> {
        if self.memo.symbols.get().is_none() {
            self.check_unchanged()?;
        }
        Ok(f(self
            .memo
            .symbols
            .get_or_init(|| self.build_symbol_table())))
    }

    /// Bytes inside executable sections that ARM / AArch64 mapping symbols
    /// mark as data, over every ELF and symbol file.
    pub(crate) fn data_ranges(&self) -> PyResult<strider_cfg::DataRanges> {
        if let Some(ranges) = self.memo.data_ranges.get() {
            return Ok(ranges.clone());
        }
        self.check_unchanged()?;
        let mut all = Vec::new();
        for obj in self.elfs.iter().chain(&self.symbol_elfs) {
            let Ok(file) = obj.checked_file() else {
                continue;
            };
            all.extend(strider_reader::elf::mapping_symbol_data_ranges(&file));
        }
        Ok(self
            .memo
            .data_ranges
            .get_or_init(|| strider_cfg::DataRanges::new(all))
            .clone())
    }

    /// The local entries of every ELF and symbol file; empty off ppc64.
    pub(crate) fn local_entries(&self) -> PyResult<Arc<strider_cfg::LocalEntries>> {
        if let Some(entries) = self.memo.local_entries.get() {
            return Ok(Arc::clone(entries));
        }
        let mut ppc64 = false;
        for obj in self.elfs.iter().chain(&self.symbol_elfs) {
            let file = obj.checked_file().map_err(into_strider_err)?;
            ppc64 |= file.architecture() == object::Architecture::PowerPc64;
        }
        let entries = if ppc64 {
            self.with_symbols(|t| {
                strider_cfg::LocalEntries::new(t.syms.iter().map(|s| (s.address, s.local_entry)))
            })?
        } else {
            strider_cfg::LocalEntries::default()
        };
        Ok(Arc::clone(
            self.memo.local_entries.get_or_init(|| Arc::new(entries)),
        ))
    }

    /// For a name with several symbols, a code symbol wins within an ELF, and
    /// the first ELF in load order wins across them.
    fn build_symbol_table(&self) -> SymbolTable {
        let mem = self.mem.inner.lock_shared();
        let index = RegionIndex::new(&mem.regions);
        // The highest-start covering region, as a read resolves an overlap.
        let region_of = |address: u64| -> Option<(u64, u64)> {
            let r = &mem.regions[index.covering(address, 1).next()?];
            Some((r.start_addr(), r.end_addr()))
        };
        let mut syms: Vec<PySymbol> = Vec::new();
        let mut by_name: HashMap<String, usize> = HashMap::new();
        // Names only an undefined symbol carries, taken once no ELF defines
        // them.
        let mut imports: Vec<(String, usize)> = Vec::new();
        for obj in self.elfs.iter().chain(&self.symbol_elfs) {
            // `with_symbols` already statted; a failure here is a torn-read
            // race, skipped rather than panicking.
            let Ok(file) = obj.checked_file() else {
                continue;
            };
            let layout = ElfSectionLayout::new(&file);
            let arm = file.architecture() == object::Architecture::Arm;
            // `None` but for a linked ppc64 ELFv1 image.
            let opd = OpdTable::new(&file);
            // Dot symbols name an ELFv1 function's code; the plain name names
            // the descriptor.
            let dot_names: std::collections::HashSet<&str> = if opd.is_some() {
                file.symbols()
                    .filter_map(|s| s.name().ok())
                    .filter(|n| n.starts_with('.'))
                    .collect()
            } else {
                std::collections::HashSet::new()
            };
            let mut per_elf: HashMap<String, usize> = HashMap::new();
            // An exported symbol is in both `.symtab` and `.dynsym`.
            let mut seen: std::collections::HashSet<(String, u64)> =
                std::collections::HashSet::new();
            for sym in file.symbols().chain(file.dynamic_symbols()) {
                let Ok(name) = sym.name() else { continue };
                if name.is_empty() {
                    continue;
                }
                let place = symbol_place(&sym, &layout);
                let declared = match place {
                    SymbolPlace::Defined => layout.symbol_address(&sym),
                    SymbolPlace::Import(addr) => addr,
                    SymbolPlace::Nowhere => continue,
                };
                let is_function = sym.kind() == object::SymbolKind::Text;
                // On ppc64 ELFv1 an `STT_FUNC` `st_value` addresses an `.opd`
                // descriptor. Its `st_size` is the code's unless a dot symbol
                // names the code or the size is a descriptor's (16 or 24).
                let followed = opd
                    .as_ref()
                    .filter(|_| is_function && matches!(place, SymbolPlace::Defined))
                    .and_then(|t| t.entry_at(declared));
                let measures_opd = || {
                    matches!(sym.size(), 16 | 24) || dot_names.contains(format!(".{name}").as_str())
                };
                let address = followed.unwrap_or(declared);
                let size = (sym.size() != 0 && !(followed.is_some() && measures_opd()))
                    .then(|| sym.size());
                if !seen.insert((name.to_string(), address)) {
                    continue;
                }
                let ix = syms.len();
                syms.push(PySymbol {
                    name: name.to_string(),
                    address,
                    size,
                    is_function,
                    thumb: arm && is_function && address & 1 == 1,
                    local_entry: address
                        .wrapping_add(strider_reader::elf::local_entry_offset(&file, &sym)),
                    region: None,
                });
                if matches!(place, SymbolPlace::Import(_)) {
                    imports.push((name.to_string(), ix));
                    continue;
                }
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
        for (name, ix) in imports {
            by_name.entry(name).or_insert(ix);
        }
        // Last, so an ELF keeps a colliding name.
        for extra in &self.extra_symbols {
            let ix = syms.len();
            syms.push(extra.clone());
            by_name.entry(extra.name.clone()).or_insert(ix);
        }
        for sym in &mut syms {
            sym.region = region_of(sym.start());
        }
        let mut by_addr: Vec<usize> = (0..syms.len()).collect();
        by_addr.sort_by_key(|&i| syms[i].start());
        let extents: Vec<(u64, u64)> = syms.iter().map(|s| (s.start(), s.covered_end())).collect();
        SymbolTable {
            extents: RegionIndex::from_ranges(&extents),
            syms,
            by_name,
            by_addr,
        }
    }
}

/// The symbol of `group` covering `address`, ranking aliases by having a
/// recorded extent, then by being code. `functions()` filters to code instead,
/// so the two can name different symbols at one address.
fn covering<'a>(group: impl Iterator<Item = &'a PySymbol>, address: u64) -> Option<&'a PySymbol> {
    let rank = |s: &PySymbol| (s.size.is_some(), s.is_function);
    let mut best: Option<&PySymbol> = None;
    for sym in group {
        let covers = sym.start() <= address && address < sym.covered_end();
        if covers && best.is_none_or(|b| rank(sym) > rank(b)) {
            best = Some(sym);
        }
    }
    best
}

#[pymethods]
impl PyLoadedElf {
    /// Re-stat every mapped file, erroring if one changed since it was mapped.
    /// A change racing a read already in progress is still a torn read or a
    /// SIGBUS.
    fn check_unchanged(&self) -> PyResult<()> {
        for elf in self.elfs.iter().chain(&self.symbol_elfs) {
            elf.check_unchanged().map_err(into_strider_err)?;
        }
        Ok(())
    }

    /// The instruction-fetch / raw-read `BufferReader` for this ELF.
    pub(crate) fn reader(&self) -> PyBufferReader {
        self.mem.clone()
    }

    /// The runtime-immutable `BufferReader` (code + read-only sections
    /// only).
    pub(crate) fn ro_reader(&self) -> PyBufferReader {
        self.rom.clone()
    }

    /// The `Symbol` named `name`, taking the first ELF in load order that
    /// defines it and preferring a code symbol over a data one of the same
    /// name, else an undefined symbol that still has an address (a PLT stub,
    /// or an object file's extern).  Raises `StriderError` when there is
    /// neither.
    pub(crate) fn symbol(&self, name: &str) -> PyResult<PySymbol> {
        self.symbol_opt(name)?.ok_or_else(|| {
            into_strider_err(anyhow::anyhow!(
                "symbol {name:?} not found in any ELF loaded into this \
                 ElfLifter ({} loaded)",
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
    /// covers only its own address, so an ARM `$a` / `$d` mapping symbol does
    /// not hide its function. Aliases sharing an address resolve by recorded
    /// extent, then by being code.  `None` when nothing covers `address`.
    fn symbol_at(&self, address: u64) -> PyResult<Option<PySymbol>> {
        self.with_symbols(|t| {
            // Highest start first, so the nearest start's aliases lead.
            let mut hits = t.extents.covering(address, 1).peekable();
            let base = t.syms[*hits.peek()?].start();
            let mut group: Vec<usize> = hits.take_while(|&i| t.syms[i].start() == base).collect();
            group.sort_unstable();
            covering(group.iter().map(|&i| &t.syms[i]), address).cloned()
        })
    }

    /// Whether the first loaded ELF sets ARM's `EF_ARM_BE8`: instructions are
    /// stored little-endian while data stays big-endian. Reported only; it
    /// selects no arch. `False` off ARM.
    #[getter]
    fn is_arm_be8(&self) -> PyResult<bool> {
        match self.elfs.first() {
            Some(elf) => elf.is_arm_be8().map_err(into_strider_err),
            None => Ok(false),
        }
    }

    /// Whether the first loaded ELF's `EI_DATA` is little-endian.
    #[getter]
    fn is_little_endian(&self) -> PyResult<bool> {
        match self.elfs.first() {
            Some(elf) => Ok(object::read::Object::is_little_endian(
                &elf.checked_file().map_err(into_strider_err)?,
            )),
            None => Ok(true),
        }
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
                    Some(prev) if prev.start() == sym.start() => {
                        if prev.size.is_none() && sym.size.is_some() {
                            *prev = sym.clone();
                        }
                    }
                    _ => out.push(sym.clone()),
                }
            }
            out
        })?;
        Ok(PySymbolIter(syms.into_iter()))
    }

    /// Every symbol, each `Symbol` built only when pulled.
    fn iter_symbols(&self) -> PyResult<PySymbolIter> {
        Ok(PySymbolIter(
            self.with_symbols(|t| t.syms.clone())?.into_iter(),
        ))
    }

    /// ELF entry-point address from the first loaded ELF.
    fn entry_point(&self) -> PyResult<u64> {
        self.elfs.first().map_or(Ok(0), |o| {
            o.checked_file()
                .map(|f| {
                    // `e_entry` addresses a descriptor on ppc64 ELFv1.
                    let entry = f.entry();
                    OpdTable::new(&f)
                        .and_then(|t| t.entry_at(entry))
                        .unwrap_or(entry)
                })
                .map_err(into_strider_err)
        })
    }

    /// Read up to `size` raw bytes at `addr`.  Returns fewer bytes near a
    /// region edge or before a relocation this loader does not compute, and
    /// `None` when `addr` is unmapped or inside such a relocation's field. In
    /// writable, non-executable data such a field is not cut out: it serves
    /// the file's bytes, which the loader overwrites.
    fn read<'py>(
        &self,
        py: Python<'py>,
        addr: u64,
        size: usize,
    ) -> PyResult<Option<Bound<'py, PyBytes>>> {
        self.mem.read(py, addr, size)
    }

    /// Take the symbols of `path` and none of its bytes, for a separate debug
    /// or symbol file linked at the loaded image's addresses.
    ///
    /// The already-loaded ELFs keep a colliding name; a symbol outside every
    /// mapped region is recorded with no region.
    fn add_symbol_file(&mut self, path: &str) -> PyResult<()> {
        let obj = strider_reader::load_elf(path).map_err(into_strider_err)?;
        self.check_describes_a_loaded_image(&obj, path)?;
        self.symbol_elfs.push(obj);
        self.memo = ElfMemo::default();
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
        // Extracted whole before anything is committed, so a failed batch adds
        // nothing. Snapshotted before extracting, as in `MapArg`.
        let symbols: Vec<_> = symbols.iter().collect();
        let mut added = Vec::with_capacity(symbols.len());
        for (name, value) in symbols {
            let name: String = name.extract()?;
            let (address, size) = if let Ok((a, n)) = value.extract::<(u64, u64)>() {
                (a, (n != 0).then_some(n))
            } else {
                (value.extract::<u64>()?, None)
            };
            added.push(PySymbol {
                name,
                address,
                size,
                is_function,
                thumb: false,
                local_entry: address,
                region: None,
            });
        }
        self.extra_symbols.append(&mut added);
        // `$d` spans come from the ELFs alone.
        self.memo = ElfMemo {
            data_ranges: std::mem::take(&mut self.memo.data_ranges),
            ..ElfMemo::default()
        };
        Ok(())
    }
}

/// Load the ELF at `path`. `from_segments` collects regions from PT_LOAD
/// program headers (falling back to the section walker for header-less
/// ET_REL); off, it walks section headers even on a linked binary, for
/// section-granular regions (`.text` / `.rodata` / `.plt` as separate
/// mappings) instead of coalesced PT_LOAD ranges.
///
/// Set `apply_relocations` for ET_DYN binaries (kernels, PIE userland)
/// whose `.text` or function-pointer tables ship with unresolved
/// relocations: section coverage widens to `.data.rel.ro` / `.got` and
/// every understood relocation is applied.
///
/// `relocated_got_is_rom` additionally serves `.got` / `.got2` /
/// `.data.rel.ro` as read-only memory; see [`relocation_mode`]. It is refused
/// without `apply_relocations`, which defaults to `false` here.
#[pyfunction]
#[pyo3(name = "_load_elf", signature = (path, from_segments=true, apply_relocations=false, relocated_got_is_rom=false))]
pub fn load_elf(
    path: &str,
    from_segments: bool,
    apply_relocations: bool,
    relocated_got_is_rom: bool,
) -> PyResult<PyLoadedElf> {
    let source = if from_segments {
        RegionSource::Auto
    } else {
        RegionSource::Sections
    };
    let elf = strider_reader::load_elf(path).map_err(into_strider_err)?;
    let mode = relocation_mode(apply_relocations, relocated_got_is_rom)?;
    let image = elf.image(source, mode).map_err(into_strider_err)?;
    Ok(PyLoadedElf {
        elfs: vec![elf],
        writable: image.writable,
        symbol_elfs: Vec::new(),
        extra_symbols: Vec::new(),
        mem: PyBufferReader::from_regions(image.fetch),
        rom: PyBufferReader::from_regions(image.rom),
        source,
        memo: ElfMemo::default(),
    })
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
    /// `__cause__`; `KeyboardInterrupt` and `SystemExit` propagate unchanged.
    /// `ReadOnlyMemory.read` is the opposite: see its docstring.
    // The raising base reads no parameter; the names are the subclass keywords.
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
        Ok(format!("{}()", slf.get_type().name()?))
    }
}

/// Shared `read`-callback prologue for both Python reader adapters.
///
/// `KeyboardInterrupt` / `SystemExit` are stashed, not `PyErr::restore`d,
/// which the next callback would destroy; a stash short-circuits every later
/// call until the outer boundary drains it.
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

/// What a Python `read` returned, as bytes; `bytes` is borrowed.
fn read_result_bytes<'a>(bound: &'a Bound<'_, PyAny>) -> PyResult<std::borrow::Cow<'a, [u8]>> {
    Ok(match bound.downcast::<PyBytes>() {
        Ok(b) => b.as_bytes().into(),
        Err(_) => bound.extract::<Vec<u8>>()?.into(),
    })
}

/// `rsleigh::MemReader` over a Python reader object, taking the GIL per call.
/// The `Py<PyAny>` is shared so `PyLifter::__traverse__` visits the same
/// reference and a reader/lifter cycle stays collectable.
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
            if result.is_none(py) {
                anyhow::bail!(
                    "address {:#x} is not mapped (Python read returned None)",
                    addr.off
                );
            }
            let bytes = read_result_bytes(result.bind(py))
                .map_err(|e| anyhow::anyhow!("PyMemReader.read must return bytes: {e}"))?;
            // A short read is allowed; an over-long one is a caller bug.
            if bytes.len() > out_buf.len() {
                anyhow::bail!(
                    "PyMemReader.read({:#x}, {}) returned {} bytes, more than requested",
                    addr.off,
                    out_buf.len(),
                    bytes.len()
                );
            }
            let n = bytes.len();
            out_buf[..n].copy_from_slice(&bytes);
            Ok(n)
        })
        .map_err(strider_reader::MemReadError::from)
    }
}

/// Read-only memory backed by Python, read by constant-load folding and
/// jump-table probes. Subclass and override `read(addr, size)` to return the
/// raw bytes at `addr`.  Only RAM loads reach it.
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
    /// An exception here is swallowed, as if `None` were returned, except
    /// `KeyboardInterrupt` and `SystemExit`, which stop the run.
    // The raising base reads no parameter; the names are the subclass keywords.
    #[allow(unused_variables)]
    fn read(&self, addr: u64, size: usize) -> PyResult<Option<Vec<u8>>> {
        Err(pyo3::exceptions::PyNotImplementedError::new_err(
            "ReadOnlyMemory.read must be overridden by subclass",
        ))
    }

    fn __repr__(slf: Bound<'_, Self>) -> PyResult<String> {
        Ok(format!("{}()", slf.get_type().name()?))
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
            let bytes = read_result_bytes(result.bind(py)).map_err(|e| {
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

/// Point-in-time snapshot of a `PyBufferReader`'s region table. Both `read`
/// impls fill the caller buffer with raw, never byte-swapped, bytes.
#[derive(Clone)]
pub struct PyBufferReaderView {
    pub table: Arc<MemRegionsLookupTable>,
}

impl rsleigh::MemReader for PyBufferReaderView {
    type Err = strider_reader::MemReadError;

    fn read(&self, addr: rsleigh::VnAddr, out_buf: &mut [u8]) -> Result<usize, Self::Err> {
        rsleigh::MemReader::read(&*self.table, addr, out_buf)
    }
}

/// Fill-all-or-error: a partial or unmapped range errors.
impl ReadOnlyMemory for PyBufferReaderView {
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        self.table.read_exact(addr, buf)
    }
}

/// The memory argument every Python entry point accepts: a `BufferReader`
/// or any Python object with a `read(...)` method.
pub struct MemInput {
    /// The object itself, shared with a callback's adapter.
    pub obj: Arc<Py<PyAny>>,
    /// `None` for a callback.
    buffer: Option<PyBufferReader>,
}

impl<'py> FromPyObject<'py> for MemInput {
    fn extract_bound(ob: &Bound<'py, PyAny>) -> PyResult<Self> {
        let buffer = ob.extract::<PyBufferReader>().ok();
        if buffer.is_none() && !ob.hasattr("read")? {
            return Err(pyo3::exceptions::PyTypeError::new_err(
                "expected a BufferReader or an object with a `read(...)` method",
            ));
        }
        Ok(Self {
            obj: Arc::new(ob.clone().unbind()),
            buffer,
        })
    }
}

impl MemInput {
    pub fn calls_python(&self) -> bool {
        self.buffer.is_none()
    }

    /// This input in the rom role.
    pub fn into_box(self) -> Box<dyn ReadOnlyMemory> {
        match self.buffer {
            Some(m) => Box::new(m.reader_view()),
            None => Box::new(PyReadOnlyMemoryAdapter { py_obj: self.obj }),
        }
    }

    pub fn into_any(self) -> AnyMemReader {
        match self.buffer {
            Some(m) => AnyMemReader::Buffer(m.reader_view()),
            None => AnyMemReader::Cb(PyMemReaderAdapter { py_obj: self.obj }),
        }
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyBufferReader>()?;
    // `_LoadedElf` stays out of the namespace; `_load_elf` still returns one.
    m.add_class::<PySymbol>()?;
    m.add_class::<PySymbolIter>()?;
    m.add_class::<PyMemReader>()?;
    m.add_class::<PyReadOnlyMemory>()?;
    Ok(())
}

/// Yields `Symbol`s one at a time.
#[pyo3::pyclass(name = "SymbolIter", module = "strider.reader")]
pub struct PySymbolIter(std::vec::IntoIter<PySymbol>);

#[pyo3::pymethods]
impl PySymbolIter {
    fn __iter__(slf: pyo3::PyRef<'_, Self>) -> pyo3::PyRef<'_, Self> {
        slf
    }

    fn __next__(&mut self) -> Option<PySymbol> {
        self.0.next()
    }

    /// What is left, not the total.
    fn __len__(&self) -> usize {
        self.0.len()
    }
}
