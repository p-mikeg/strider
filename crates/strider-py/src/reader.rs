use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use object::{Object, ObjectSection, ObjectSymbol};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyMemoryView};

use crate::errors::into_strider_err;
use strider_reader::elf::{
    AddressSet, ElfSectionLayout, OpdTable, RegionSource, RelocationMode, mapping_symbol_kind,
    without_ranges,
};
use strider_reader::{MemRegion, MemRegionsLookupTable, ReadOnlyMemory, RegionIndex};

/// Every lock is taken under the GIL (a GIL-free `analyze` reads a
/// `reader_view` snapshot); never hold a guard across a call that can release
/// it. Poisoning is ignored: no panic leaves the plain data half-written.
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
    /// Cleared on every region change.
    pub(crate) table: Option<Arc<MemRegionsLookupTable>>,
}

/// Raw byte reader, usable as both `mem` and `rom`. Clones share state.
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

    /// Point-in-time snapshot of the regions.
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
    /// Map `data` at `base_addr`. Raises `ValueError` if the region runs past
    /// the address space, `TypeError` for a buffer whose items are not one
    /// byte.
    #[new]
    fn new(base_addr: crate::bigint::Addr, data: &Bound<'_, PyAny>) -> PyResult<Self> {
        let region = MemRegion::new(base_addr.0, extract_bytes(data)?.into_owned())
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
        Ok(Self::from_regions(vec![region]))
    }

    fn __repr__(&self) -> String {
        format!(
            "BufferReader({} region(s))",
            self.inner.lock_shared().regions.len()
        )
    }

    /// Up to `size` bytes at `addr` (fewer at a region edge or before an
    /// uncomputed relocation), or `None` when `addr` is unmapped or inside
    /// such a relocation's field. Writable, non-executable data serves the
    /// file's bytes for that field.
    fn read<'py>(
        &self,
        py: Python<'py>,
        addr: crate::bigint::Addr,
        size: usize,
    ) -> PyResult<Option<Bound<'py, PyBytes>>> {
        let table = self.lookup_table();
        table.check_unchanged().map_err(into_strider_err)?;
        Ok(table
            .read_vec(addr.0, size)
            .map(|buf| PyBytes::new_bound(py, &buf)))
    }
}

/// Errors if a file mapped behind a `BufferReader` in `objs` changed.
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

enum SymbolPlace {
    Defined,
    /// An address the file defines no bytes at: a PLT stub, or an object
    /// file's synthetic extern (an undefined, common or defined IFUNC
    /// symbol's).
    Import(u64),
    Nowhere,
}

fn symbol_place<'d, S: ObjectSymbol<'d>>(sym: &S, layout: &ElfSectionLayout) -> SymbolPlace {
    // A TLS symbol's value is an offset into the per-thread block.
    if sym.kind() == object::SymbolKind::Tls {
        return SymbolPlace::Nowhere;
    }
    if let Some(addr) = layout.extern_address(sym.index().0) {
        return SymbolPlace::Import(addr);
    }
    match sym.section() {
        object::SymbolSection::Section(_) => SymbolPlace::Defined,
        // `SHN_ABS` at 0 is a synthetic entry, such as an `STT_FILE` name.
        object::SymbolSection::Absolute if sym.address() != 0 => SymbolPlace::Defined,
        object::SymbolSection::Undefined if sym.address() != 0 => {
            SymbolPlace::Import(sym.address())
        }
        _ => SymbolPlace::Nowhere,
    }
}

/// One file's ppc64 ELFv1 descriptors: its own `.opd`, or, where that is
/// `SHT_NOBITS` as a debug file's is, the loaded ELFs'.
enum Opds<'a, 'd> {
    Own(OpdTable<'d>),
    Loaded(&'a [OpdTable<'d>]),
}

impl<'a, 'd> Opds<'a, 'd> {
    /// `None` off ppc64 ELFv1, or for a `SHT_NOBITS` `.opd` with no loaded
    /// ELFv1 image.
    fn new(file: &object::File<'d>, loaded: &'a [OpdTable<'d>]) -> Option<Self> {
        if let Some(own) = OpdTable::new(file) {
            return Some(Self::Own(own));
        }
        let nobits = file
            .section_by_name(".opd")
            .is_some_and(|s| s.kind() == object::SectionKind::UninitializedData);
        (nobits && !loaded.is_empty()).then_some(Self::Loaded(loaded))
    }

    fn table(&self, descriptor: u64) -> Option<&OpdTable<'d>> {
        match self {
            Self::Own(t) => Some(t),
            Self::Loaded(ts) => ts.iter().find(|t| t.entry_at(descriptor).is_some()),
        }
    }

    fn entry_at(&self, descriptor: u64) -> Option<u64> {
        self.table(descriptor)?.entry_at(descriptor)
    }

    fn toc_at(&self, descriptor: u64) -> Option<u64> {
        self.table(descriptor)?.toc_at(descriptor)
    }
}

/// `load_elf`'s two relocation flags; `relocated_got_is_rom` needs
/// `apply_relocations`.
fn relocation_mode(
    apply_relocations: bool,
    relocated_got_is_rom: bool,
) -> PyResult<RelocationMode> {
    match (apply_relocations, relocated_got_is_rom) {
        (true, got_is_rom) => Ok(RelocationMode::Applied { got_is_rom }),
        (false, false) => Ok(RelocationMode::Unapplied),
        (false, true) => Err(into_strider_err(anyhow::anyhow!(
            "relocated_got_is_rom=True needs apply_relocations=True: unrelocated GOT \
             slots serve their on-disk bytes (0 on a PIE)"
        ))),
    }
}

/// Parsed ELF binary, from `strider.lift.load_elf(path)`.
#[pyclass(name = "_LoadedElf", module = "strider.reader")]
pub struct PyLoadedElf {
    /// Load order; the first wins on symbol-name collisions.
    elfs: Vec<strider_reader::OwnedElf>,
    /// Never served by `rom`.
    writable: AddressSet,
    /// Includes writable sections when relocations were applied.
    mem: PyBufferReader,
    /// The runtime-immutable subset.
    rom: PyBufferReader,
    source: RegionSource,
    /// ELFs consulted for symbols only, never for bytes.
    symbol_elfs: Vec<strider_reader::OwnedElf>,
    /// Appended last, so an ELF keeps a colliding name.
    extra_symbols: Vec<PySymbol>,
    memo: ElfMemo,
}

/// Dropped when a source it reads is added.
#[derive(Default)]
struct ElfMemo {
    symbols: OnceLock<SymbolTable>,
    data_ranges: OnceLock<strider_cfg::DataRanges>,
    local_entries: OnceLock<Arc<strider_cfg::LocalEntries>>,
    /// Whether an ELF or symbol file has a `<base>.cold` function symbol.
    has_cold_symbols: OnceLock<bool>,
    /// ppc64 ELFv1 function entries to their descriptors' TOC pointers;
    /// `None` where two descriptors disagree.
    entry_tocs: OnceLock<HashMap<u64, Option<u64>>>,
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

/// A sub-range both map where `new` serves different bytes than `existing`.
/// An address resolves as a read does, through the highest-start covering
/// region.
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

/// One ELF symbol. Equal when every field is.
#[pyclass(name = "Symbol", module = "strider.reader", frozen, eq, hash)]
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PySymbol {
    /// The name as spelled in the symbol table.
    #[pyo3(get)]
    name: String,
    /// `st_value`; for a ppc64 ELFv1 function, the code its `.opd`
    /// descriptor names. A Thumb function keeps the Thumb bit. An object
    /// file's undefined, common or defined IFUNC symbol has an unmapped
    /// address past the image.
    #[pyo3(get)]
    pub(crate) address: u64,
    /// `st_size`; `None` when 0, for an object file's defined IFUNC, or when
    /// a ppc64 ELFv1 function's `st_size` measures its `.opd` descriptor.
    #[pyo3(get)]
    pub(crate) size: Option<u64>,
    is_function: bool,
    /// An ARM Thumb function, whose `address` keeps the ISA bit.
    thumb: bool,
    /// `address` plus the ppc64 ELFv2 local-entry offset, else `address`.
    #[pyo3(get)]
    local_entry: u64,
    pub(crate) region: Option<(u64, u64)>,
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
    /// Whether the ELF types this `STT_FUNC` or `STT_GNU_IFUNC`, but `False`
    /// for an object file's defined IFUNC, which has no code at its extern
    /// address.
    #[getter]
    fn is_function(&self) -> bool {
        self.is_function
    }

    /// One past the last byte, or `None` when `size` is. Ignores the Thumb
    /// bit.
    #[getter]
    fn end(&self) -> Option<u64> {
        self.size.map(|s| self.start().saturating_add(s))
    }

    /// An ARM Thumb function: `address` carries the Thumb bit, so `analyze`
    /// enters in Thumb mode at `address & ~1`.
    #[getter]
    fn is_thumb(&self) -> bool {
        self.thumb
    }

    /// `(start, end)` of the loaded region holding the symbol, or `None`.
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

struct SymbolTable {
    /// Load order, then symbol-table order within an ELF.
    syms: Vec<PySymbol>,
    /// The winner for each name; two symbols can share one.
    by_name: HashMap<String, usize>,
    /// Indices into `syms`, ascending by first byte, ties in load order.
    by_addr: Vec<usize>,
    /// Over `[start, covered_end)`.
    extents: RegionIndex,
    /// Sized cold fragments by function name.
    cold_fragments: HashMap<String, Vec<std::ops::Range<u64>>>,
}

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

    /// Refuses an ELF whose architecture, class, byte order or ARM
    /// `EF_ARM_BE8` differs from the first image's.
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

    /// Merge another ELF's regions and symbols. The earlier ELF wins a name
    /// collision; `relocated_got_is_rom` applies to this image only. Errors
    /// on a different target or an address already loaded with other bytes;
    /// re-merging a loaded image does nothing.
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
        // An image mapping nothing still goes through, for its symbols.
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

    /// The `$d` mapping-symbol spans of every ELF and symbol file.
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

    /// The cold fragments of every function symbol at `entry`.
    pub(crate) fn cold_fragments(&self, entry: u64) -> PyResult<strider_cfg::DataRanges> {
        let names_cold = |name: &str| cold_fragment_base(name).is_some();
        let has_cold = match self.memo.has_cold_symbols.get() {
            Some(&has) => has,
            None => {
                self.check_unchanged()?;
                let has = self.elfs.iter().chain(&self.symbol_elfs).any(|obj| {
                    obj.checked_file().is_ok_and(|file| {
                        file.symbols()
                            .chain(file.dynamic_symbols())
                            .any(|s| s.name().is_ok_and(names_cold))
                    })
                });
                *self.memo.has_cold_symbols.get_or_init(|| has)
            }
        };
        if !has_cold && !self.extra_symbols.iter().any(|s| names_cold(&s.name)) {
            return Ok(strider_cfg::DataRanges::default());
        }
        self.with_symbols(|t| {
            // A Thumb symbol's `start` is one below its address.
            let first = t
                .by_addr
                .partition_point(|&i| t.syms[i].start() < entry.saturating_sub(1));
            let names = t.by_addr[first..]
                .iter()
                .map(|&i| &t.syms[i])
                .take_while(|s| s.start() <= entry)
                .filter(|s| s.is_function && s.address == entry)
                .map(|s| s.name.as_str());
            strider_cfg::DataRanges::new(
                names
                    .filter_map(|name| t.cold_fragments.get(name))
                    .flatten()
                    .cloned(),
            )
        })
    }

    /// `r2` on entry to the ppc64 ELFv1 function whose code starts at
    /// `entry`: the TOC pointer of the `.opd` descriptor a function symbol
    /// names there. `None` off ELFv1, for an address no descriptor enters,
    /// or where two descriptors give it different TOCs.
    pub(crate) fn entry_toc(&self, entry: u64) -> PyResult<Option<u64>> {
        if let Some(tocs) = self.memo.entry_tocs.get() {
            return Ok(tocs.get(&entry).copied().flatten());
        }
        let mut tocs: HashMap<u64, Option<u64>> = HashMap::new();
        let loaded = self.loaded_opds();
        for obj in self.elfs.iter().chain(&self.symbol_elfs) {
            let file = obj.checked_file().map_err(into_strider_err)?;
            let Some(opd) = Opds::new(&file, &loaded) else {
                continue;
            };
            let layout = ElfSectionLayout::new(&file);
            for sym in file.symbols().chain(file.dynamic_symbols()) {
                if sym.kind() != object::SymbolKind::Text
                    || !matches!(symbol_place(&sym, &layout), SymbolPlace::Defined)
                {
                    continue;
                }
                let descriptor = layout.symbol_address(&sym);
                let (Some(code), Some(toc)) = (opd.entry_at(descriptor), opd.toc_at(descriptor))
                else {
                    continue;
                };
                tocs.entry(code)
                    .and_modify(|seen| {
                        if *seen != Some(toc) {
                            *seen = None;
                        }
                    })
                    .or_insert(Some(toc));
            }
        }
        let tocs = self.memo.entry_tocs.get_or_init(|| tocs);
        Ok(tocs.get(&entry).copied().flatten())
    }

    /// The loaded ELFs' `.opd` tables, for a symbol file's descriptors; empty
    /// without one.
    fn loaded_opds(&self) -> Vec<OpdTable<'_>> {
        if self.symbol_elfs.is_empty() {
            return Vec::new();
        }
        self.elfs
            .iter()
            .filter_map(|o| OpdTable::new(&o.checked_file().ok()?))
            .collect()
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
        // Taken only for names no ELF defines.
        let mut imports: Vec<(String, usize)> = Vec::new();
        let loaded = self.loaded_opds();
        for obj in self.elfs.iter().chain(&self.symbol_elfs) {
            // `with_symbols` already statted; a failure is a torn-read race.
            let Ok(file) = obj.checked_file() else {
                continue;
            };
            let layout = ElfSectionLayout::new(&file);
            let arm = file.architecture() == object::Architecture::Arm;
            let marks_spans = arm
                || matches!(
                    file.architecture(),
                    object::Architecture::Aarch64 | object::Architecture::Aarch64_Ilp32
                );
            // `None` off ppc64 ELFv1.
            let opd = Opds::new(&file, &loaded);
            // ELFv1: `.f` names the code, `f` the descriptor.
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
                // A mapping symbol marks a code or data span, not a symbol.
                if name.is_empty() || (marks_spans && mapping_symbol_kind(name).is_some()) {
                    continue;
                }
                let place = symbol_place(&sym, &layout);
                let declared = match place {
                    SymbolPlace::Defined => layout.symbol_address(&sym),
                    SymbolPlace::Import(addr) => addr,
                    SymbolPlace::Nowhere => continue,
                };
                // A defined IFUNC at its extern address: no code there, and
                // its `st_size` is the resolver's.
                let ifunc_extern = matches!(place, SymbolPlace::Import(_))
                    && matches!(sym.section(), object::SymbolSection::Section(_));
                let is_function = sym.kind() == object::SymbolKind::Text && !ifunc_extern;
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
                let size =
                    (sym.size() != 0 && !ifunc_extern && !(followed.is_some() && measures_opd()))
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
                    local_entry: match place {
                        SymbolPlace::Defined => address
                            .wrapping_add(strider_reader::elf::local_entry_offset(&file, &sym)),
                        _ => address,
                    },
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
        let mut cold_fragments: HashMap<String, Vec<std::ops::Range<u64>>> = HashMap::new();
        for sym in syms.iter().filter(|s| s.is_function) {
            if let (Some(base), Some(size)) = (cold_fragment_base(&sym.name), sym.size) {
                cold_fragments
                    .entry(base.to_owned())
                    .or_default()
                    .push(sym.start()..sym.start().saturating_add(size));
            }
        }
        SymbolTable {
            cold_fragments,
            extents: RegionIndex::from_ranges(&extents),
            syms,
            by_name,
            by_addr,
        }
    }
}

/// `<base>` of `<base>.cold` or `<base>.cold.<digits>`. A `.part.N` /
/// `.constprop.N` / `.isra.N` clone is a separate callee, not a fragment.
fn cold_fragment_base(name: &str) -> Option<&str> {
    let unnumbered = match name.rsplit_once('.') {
        Some((head, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => head,
        _ => name,
    };
    unnumbered
        .strip_suffix(".cold")
        .filter(|base| !base.is_empty())
}

/// The symbol of `group` covering `address`, ranking aliases by having a
/// size, then by being code; so it can differ from `functions()`' pick.
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
    /// Raise if a mapped file changed since it was mapped. A change racing a
    /// read in progress is still a torn read or a SIGBUS.
    fn check_unchanged(&self) -> PyResult<()> {
        for elf in self.elfs.iter().chain(&self.symbol_elfs) {
            elf.check_unchanged().map_err(into_strider_err)?;
        }
        Ok(())
    }

    /// The instruction-fetch `BufferReader`.
    pub(crate) fn reader(&self) -> PyBufferReader {
        self.mem.clone()
    }

    /// The ROM `BufferReader`: non-writable mappings, an object file's
    /// synthetic GOT, and `.got` / `.data.rel.ro` under `relocated_got_is_rom`.
    pub(crate) fn ro_reader(&self) -> PyBufferReader {
        self.rom.clone()
    }

    /// The `Symbol` named `name`: from the first ELF defining it, code over
    /// data, else an undefined symbol with an address (a PLT stub or an
    /// object file's extern). Raises `StriderError` when there is none.
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

    /// The nearest symbol at or below `address` whose extent reaches it, or
    /// `None`. An unsized symbol covers only its own address; aliases rank
    /// by having a size, then by being code.
    fn symbol_at(&self, address: crate::bigint::Addr) -> PyResult<Option<PySymbol>> {
        let address = address.0;
        self.with_symbols(|t| {
            // Highest start first, so the nearest start's aliases lead.
            let mut hits = t.extents.covering(address, 1).peekable();
            let base = t.syms[*hits.peek()?].start();
            let mut group: Vec<usize> = hits.take_while(|&i| t.syms[i].start() == base).collect();
            group.sort_unstable();
            covering(group.iter().map(|&i| &t.syms[i]), address).cloned()
        })
    }

    /// Whether the first ELF sets ARM's `EF_ARM_BE8` (little-endian code,
    /// big-endian data). Reported only; it selects no arch.
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

    /// Every symbol but ARM / AArch64 mapping symbols as `dict[str, Symbol]`,
    /// keyed by the name it resolves under.
    fn symbols(&self) -> PyResult<HashMap<String, PySymbol>> {
        self.with_symbols(|t| {
            t.by_name
                .iter()
                .map(|(name, &ix)| (name.clone(), t.syms[ix].clone()))
                .collect()
        })
    }

    /// Function symbols in address order, one per address, preferring a
    /// sized alias. An unsized function is yielded with `size` `None`.
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

    /// Iterate every symbol.
    fn iter_symbols(&self) -> PyResult<PySymbolIter> {
        Ok(PySymbolIter(
            self.with_symbols(|t| t.syms.clone())?.into_iter(),
        ))
    }

    /// The first ELF's entry point.
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

    /// Up to `size` bytes at `addr`, as `BufferReader.read`.
    fn read<'py>(
        &self,
        py: Python<'py>,
        addr: crate::bigint::Addr,
        size: usize,
    ) -> PyResult<Option<Bound<'py, PyBytes>>> {
        self.mem.read(py, addr, size)
    }

    /// Add the symbols, not the bytes, of a debug or symbol file for this
    /// image. Loaded ELFs keep a colliding name.
    fn add_symbol_file(&mut self, path: &str) -> PyResult<()> {
        let obj = strider_reader::load_elf(path).map_err(into_strider_err)?;
        self.check_describes_a_loaded_image(&obj, path)?;
        self.symbol_elfs.push(obj);
        self.memo = ElfMemo::default();
        Ok(())
    }

    /// Add symbols from a `{name: address | (address, size)}` dict.
    /// `is_function` marks them all as code. An ELF keeps a colliding name.
    #[pyo3(signature = (symbols, *, is_function=true))]
    fn add_symbols(&mut self, symbols: &Bound<'_, PyDict>, is_function: bool) -> PyResult<()> {
        // A failed batch adds nothing.
        let symbols: Vec<_> = symbols.iter().collect();
        let mut added = Vec::with_capacity(symbols.len());
        for (name, value) in symbols {
            let name: String = name.extract()?;
            let (address, size) = if value.is_instance_of::<pyo3::types::PyTuple>() {
                let (a, n): (crate::bigint::Addr, u64) = value.extract()?;
                (a.0, (n != 0).then_some(n))
            } else {
                (value.extract::<crate::bigint::Addr>()?.0, None)
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
        // `$d` spans, the files' cold symbols and TOCs come from the ELFs alone.
        self.memo = ElfMemo {
            data_ranges: std::mem::take(&mut self.memo.data_ranges),
            has_cold_symbols: std::mem::take(&mut self.memo.has_cold_symbols),
            entry_tocs: std::mem::take(&mut self.memo.entry_tocs),
            ..ElfMemo::default()
        };
        Ok(())
    }
}

/// Load the ELF at `path`, as `strider.lift.load_elf`.
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

/// Instruction source backed by Python: subclass and override `read`.
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
    /// unmapped. Raising, another type, or more than `size` bytes fails the
    /// lift with `StriderError` (the exception as `__cause__`);
    /// `KeyboardInterrupt` and `SystemExit` propagate.
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

/// `KeyboardInterrupt` / `SystemExit` are stashed, not `PyErr::restore`d
/// (the next callback would destroy it); a stash short-circuits every later
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

/// `bytes`, a one-byte-item buffer, or a sequence of ints. A buffer of wider
/// items raises `TypeError`: reading it per item would drop bytes.
fn extract_bytes<'a>(bound: &'a Bound<'_, PyAny>) -> PyResult<std::borrow::Cow<'a, [u8]>> {
    if let Ok(b) = bound.downcast::<PyBytes>() {
        return Ok(b.as_bytes().into());
    }
    let Ok(view) = PyMemoryView::from_bound(bound) else {
        return Ok(bound.extract::<Vec<u8>>()?.into());
    };
    let itemsize: usize = view.getattr("itemsize")?.extract()?;
    if itemsize != 1 {
        let format: String = view.getattr("format")?.extract()?;
        return Err(pyo3::exceptions::PyTypeError::new_err(format!(
            "a buffer of {itemsize}-byte items (format {format:?}) is not bytes; \
             memoryview(x).cast('B') gives its raw bytes"
        )));
    }
    let bytes = view.call_method0("tobytes")?;
    Ok(bytes.downcast::<PyBytes>()?.as_bytes().to_vec().into())
}

/// Takes the GIL per call. The `Py<PyAny>` is shared so
/// `PyLifter::__traverse__` visits it and a reader/lifter cycle stays
/// collectable.
#[derive(Clone)]
pub struct PyMemReaderAdapter {
    pub py_obj: std::sync::Arc<Py<PyAny>>,
}

impl rsleigh::MemReader for PyMemReaderAdapter {
    type Err = strider_reader::MemReadError;

    fn read(&self, addr: rsleigh::VnAddr, out_buf: &mut [u8]) -> Result<usize, Self::Err> {
        let size = out_buf.len();
        Python::with_gil(|py| -> anyhow::Result<usize> {
            // Fail the lift: Sleigh would read the error as unmapped.
            let fail = |msg: String, cause: Option<PyErr>| {
                if !crate::pattern::peek_pending_query_error() {
                    crate::pattern::stash_pending_query_error(crate::errors::reader_failure(
                        msg.clone(),
                        cause,
                    ));
                }
                anyhow::anyhow!(msg)
            };
            let call = format!("MemReader.read({:#x}, {size})", addr.off);
            let result = call_py_read(py, &self.py_obj, (addr.off, size), "MemReader.read", |e| {
                fail(format!("{call} raised: {e}"), Some(e))
            })?;
            if result.is_none(py) {
                anyhow::bail!("{call} returned None");
            }
            let result = result.bind(py);
            let bytes = extract_bytes(result).map_err(|e| {
                let ty = result
                    .get_type()
                    .name()
                    .map_or_else(|_| "?".into(), |n| n.to_string());
                fail(
                    format!("{call} must return bytes or None, got {ty}: {e}"),
                    Some(e),
                )
            })?;
            if bytes.len() > size {
                return Err(fail(
                    format!("{call} returned {} bytes, more than requested", bytes.len()),
                    None,
                ));
            }
            let n = bytes.len();
            out_buf[..n].copy_from_slice(&bytes);
            Ok(n)
        })
        .map_err(strider_reader::MemReadError::from)
    }
}

/// Read-only memory backed by Python: subclass and override `read`. Only RAM
/// loads reach it.
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

    /// Override to return the `size` raw (not byte-swapped) bytes at `addr`,
    /// or `None` for unmapped. An exception, or another type or length, reads
    /// as `None`; `KeyboardInterrupt` and `SystemExit` stop the run.
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

/// Shares one `Py<>` for the same cyclic-GC reason as
/// [`PyMemReaderAdapter`].
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
            let bytes = extract_bytes(result.bind(py)).map_err(|e| {
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

/// Point-in-time snapshot of a `PyBufferReader`'s regions.
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

    fn try_read(&self, addr: u64, buf: &mut [u8]) -> bool {
        self.table.read(addr, buf) == Some(buf.len())
    }
}

/// A `BufferReader` or any Python object with a `read(...)` method.
pub struct MemInput {
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

    /// Symbols left.
    fn __len__(&self) -> usize {
        self.0.len()
    }
}
