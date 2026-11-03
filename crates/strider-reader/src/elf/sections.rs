//! ELF to [`MemRegion`] loaders.
//!
//! # Dispatch
//!
//! ET_EXEC / ET_DYN walk **program headers** (PT_LOAD), the canonical runtime
//! layout: section headers can be stripped entirely without affecting it.
//! A linked image with a `PT_DYNAMIC` is relocated from the tables its dynamic
//! tags place, as the loader reads them, so stripping the headers changes
//! none of its relocations either (`dynamic::loader_view`).
//!
//! ET_REL and every other kind fall back to walking **sections**, since a
//! relocatable object has no program headers at all (PT_LOAD only appears
//! post-link). Pre-link `sh_addr` is typically 0, so `.text`, `.text.startup`,
//! `.text.foo` commonly share VMA 0. [`ElfSectionLayout`] resolves that the way
//! a linker (and GHIDRA's `.o` import) does, by giving each colliding section
//! its own synthetic base. Every address a caller sees goes through it: a
//! region start, a relocation site, a symbol.
//!
//! Both walks dedup by **loaded** address, since `MemRegionsLookupTable` keeps
//! one region per start: sections first-wins, segments widest-wins. What
//! reaches the section dedup is the non-empty, allocatable, non-TLS sections,
//! and the rebase hands exactly those strictly increasing bases, so on ET_REL
//! it never fires; on a linked image forced down the section walk the bases are
//! the raw `sh_addr`s, where a linker-script overlay can collide. Deciding here
//! keeps the choice deterministic rather than leaving it to that table's
//! last-insert-wins rule.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context as _;
use object::{Object, ObjectKind, ObjectSection, ObjectSegment, ObjectSymbol};

use crate::{FileBytes, MemRegion, Result};

/// Which walk builds the regions.
#[derive(Clone, Copy)]
pub enum RegionSource {
    /// PT_LOAD headers for ET_EXEC / ET_DYN, sections for everything else.
    Auto,
    /// The section walk, even on an image carrying PT_LOAD headers.
    Sections,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadFilter {
    /// `.text`, `.rodata`, `.plt`, `.eh_frame`, plus a writable-but-executable
    /// mapping: what an instruction FETCH may reference.
    CodeAndReadOnly,
    /// Immutable mappings only. The ROM feeds `LoadReadOnly`, which folds a
    /// constant-address load without consulting the memory chain, so a writable
    /// mapping here makes a store-then-reload fold to its file-initial value.
    ImmutableOnly,
    /// [`ImmutableOnly`](Self::ImmutableOnly) plus `.got` (and PPC32's
    /// per-object `.got2`) and `.data.rel.ro`, writable mappings that the
    /// dynamic loader fills before the program runs and that a full-RELRO link
    /// then maps read-only.
    ///
    /// A slot whose load-time value only the loader computes is a hole, not
    /// its file bytes: one under a relocation the applier does not compute,
    /// an IFUNC's among them (it holds what the resolver returns), every byte
    /// of an `R_*_COPY` object (the defining library's copy), the two GOT
    /// header words ARM and secure-PLT PPC32 lazy binding writes, and on
    /// MIPS each primary GOT entry the loader writes without a relocation
    /// record: the lazy-resolver word, the module-pointer word when its top
    /// bit asks for one, and every global entry except those for symbols the
    /// image defines that already hold their address.
    ///
    /// The rest is an UNCHECKED claim about the runtime, which the image
    /// cannot prove: that nothing rewrites a slot after the loader has run (a
    /// program that writes its own GOT, as PPC32 code relocating its own
    /// `.got2` does: Linux's `prom_init` `reloc_got2`, `-mrelocatable`
    /// `.fixup`), and that a symbol the image defines
    /// binds to that definition (a copy relocation, `LD_PRELOAD` or a
    /// `dlopen`ed library interposes another). Where it is false, a load
    /// through the slot folds to the wrong value. What it buys is the PIC
    /// table base: on `-mabicalls` MIPS the jump-table base is loaded through
    /// the GOT, so nothing in the dispatch folds while those slots are
    /// unmapped.
    ///
    /// `.got.plt` is NOT included. Lazy binding rewrites those slots on the
    /// first call through each one, so their load-time contents are the PLT
    /// resolver stub rather than the callee, whatever the image claims.
    ///
    /// A section of either name is carved out only where the link really maps
    /// it writable and not as TLS (`SHF_ALLOC | SHF_WRITE`, no `SHF_TLS`), and
    /// `relocate = false` is refused outright: it denies the very claim this
    /// filter makes. None is carved out of an image with a table its dynamic
    /// tags place that cannot be read or decoded.
    ImmutableAndRelocatedGot,
    /// Every allocatable mapping, `.data` / `.got` / `.data.rel.ro` included.
    AllAllocatable,
}

impl LoadFilter {
    /// Exec beats write for the fetch role, since a firmware image can ship a
    /// single RWX PT_LOAD. A writable mapping the GOT lives in is taken whole,
    /// and `collect_regions` cuts the rest of it back out.
    fn accepts(self, is_writable: bool, is_exec: bool) -> bool {
        match self {
            LoadFilter::CodeAndReadOnly => is_exec || !is_writable,
            LoadFilter::ImmutableOnly => !is_writable,
            LoadFilter::ImmutableAndRelocatedGot | LoadFilter::AllAllocatable => true,
        }
    }

    /// PT_LOAD only, see [`accepts`](Self::accepts).
    fn segment_accepts(self, p_flags: u32) -> bool {
        self.accepts(
            p_flags & object::elf::PF_W != 0,
            p_flags & object::elf::PF_X != 0,
        )
    }

    /// Sections always require `SHF_ALLOC` and reject `SHF_TLS`; `SHF_WRITE` /
    /// `SHF_EXECINSTR` then pick exec-or-rodata vs include-writable.
    ///
    /// A `SHF_TLS` section is addressed as an offset into the per-thread block,
    /// so [`ElfSectionLayout`] leaves it at its `sh_addr`. Loading it would map
    /// a non-empty `.tdata` at its `sh_addr`, 0 in a `.o`, at an address that
    /// means nothing and that the rebase deliberately leaves unmapped.
    fn section_accepts(self, sh_flags: u64) -> bool {
        is_mapped(sh_flags)
            && self.accepts(
                has(sh_flags, object::elf::SHF_WRITE),
                has(sh_flags, object::elf::SHF_EXECINSTR),
            )
    }
}

/// `sec`'s `sh_flags`.
fn sh_flags<'d>(sec: &impl ObjectSection<'d>) -> Option<u64> {
    match sec.flags() {
        object::SectionFlags::Elf { sh_flags } => Some(sh_flags),
        _ => None,
    }
}

fn has(sh_flags: u64, flag: u32) -> bool {
    sh_flags & u64::from(flag) != 0
}

/// `SHF_ALLOC` and not `SHF_TLS`: a section in the flat address space. A
/// TLS section's `sh_addr` is an offset into the per-thread block.
fn is_mapped(sh_flags: u64) -> bool {
    has(sh_flags, object::elf::SHF_ALLOC) && !has(sh_flags, object::elf::SHF_TLS)
}

/// Whether `name` is one of the sections
/// [`LoadFilter::ImmutableAndRelocatedGot`] serves although they are writable:
/// `.got`, PPC32's `.got2`, `.data.rel.ro` and its `.local` / per-object
/// suffixes.
///
/// `.got.plt` fails the exact-name test, for the reason the filter gives.
fn is_relocated_got_name(name: &str) -> bool {
    name == ".got" || name == ".got2" || name == ".data.rel.ro" || name.starts_with(".data.rel.ro.")
}

/// `[start, end)` of every such section, at its [`ElfSectionLayout`] base,
/// over its declared extent up to the next allocated section's start and the
/// end of the PT_LOAD holding it: an `sh_size` reaching past either is
/// malformed, and would serve writable bytes the section does not hold.
///
/// The name alone is not enough, because every address a range here names stops
/// being cut out of the read-only image: the section has to be one the loader
/// really maps and really leaves writable, so it needs the flags
/// [`LoadFilter::section_accepts`] demands of a mapped writable section
/// (`SHF_ALLOC | SHF_WRITE`, no `SHF_TLS`, whose addresses are offsets into the
/// per-thread block). A name-only match carves out whatever sits at the
/// address, which for an unmapped section is another section's writable bytes.
///
/// Empty when `unread`, a table the image's dynamic tags place that cannot
/// be read or decoded: what it relocates could land in any of them.
fn relocated_got_ranges(
    obj: &object::File<'_>,
    layout: &ElfSectionLayout,
    unread: bool,
) -> AddressSet {
    if unread {
        return AddressSet::default();
    }
    let mut starts = Vec::new();
    let mut served = Vec::new();
    for sec in obj.sections() {
        let Some(flags) = sh_flags(&sec) else {
            continue;
        };
        if sec.size() == 0 || !is_mapped(flags) {
            continue;
        }
        let base = layout.section_base(&sec);
        starts.push(base);
        if has(flags, object::elf::SHF_WRITE) && sec.name().is_ok_and(is_relocated_got_name) {
            served.push((base, base.saturating_add(sec.size())));
        }
    }
    starts.sort_unstable();
    for (lo, hi) in &mut served {
        if let Some(&next) = starts.get(starts.partition_point(|&s| s <= *lo)) {
            *hi = (*hi).min(next);
        }
        let segment_end = obj
            .segments()
            .map(|seg| (seg.address(), seg.address().saturating_add(seg.size())))
            .find(|&(start, end)| start <= *lo && *lo < end);
        if let Some((_, end)) = segment_end {
            *hi = (*hi).min(end);
        }
    }
    AddressSet::merged(served)
}

/// The mappings one walk accepted, and which addresses are writable.
pub(crate) struct LoadedImage {
    /// One per start, as a lookup table over them keeps.
    pub(crate) regions: Vec<MemRegion>,
    /// Every writable mapping the walk SAW, whether or not it survived the
    /// filter and the dedup: a writable PT_LOAD overlapping an accepted
    /// read-only one leaves no region behind, and a [`crate::ReadOnlyMemory`]
    /// view that did not know about it would serve the accepted mapping's
    /// file-initial bytes for an address that is RW at runtime.
    pub(crate) writable: AddressSet,
    /// Every executable mapping the walk saw.  A field that is writable AND
    /// executable is code the program may rewrite, so an uncomputed relocation
    /// over one stays a hole: the bytes reach the decoder, where serving them
    /// unrelocated is a wrong instruction rather than stale data.
    pub(crate) executable: AddressSet,
    /// The sections the section walk loaded, by index: those an ET_REL's
    /// relocation walk sites into. Empty off the section walk.
    pub(crate) owners: BTreeSet<usize>,
}

/// What a walk saw, its ranges as noted: [`LoadedImage`] before its sets are
/// merged.
#[derive(Default)]
struct Walk {
    regions: Vec<MemRegion>,
    writable: Vec<(u64, u64)>,
    executable: Vec<(u64, u64)>,
    owners: BTreeSet<usize>,
}

impl Walk {
    /// Address space, not file bytes: a mapping's BSS tail is writable too.
    fn note_writable(&mut self, start: u64, size: u64) {
        if size != 0 {
            self.writable.push((start, start.saturating_add(size)));
        }
    }

    fn note_executable(&mut self, start: u64, size: u64) {
        if size != 0 {
            self.executable.push((start, start.saturating_add(size)));
        }
    }

    /// The image, every region kept whole.
    fn into_image(self) -> LoadedImage {
        LoadedImage {
            regions: self.regions,
            writable: AddressSet::merged(self.writable),
            executable: AddressSet::merged(self.executable),
            owners: self.owners,
        }
    }

    /// The image with every writable address but `served`'s cut out of the
    /// regions, so that no read of one can serve bytes the program can
    /// overwrite.
    ///
    /// `served` leaves `writable`, so a relocation the applier cannot compute
    /// over a served address is a hole rather than the field's file bytes:
    /// [`crate::ReadOnlyMemory`] answers from these regions alone.
    fn without_writable_except(self, served: &AddressSet) -> LoadedImage {
        let writable = AddressSet::merged_without(self.writable, served);
        LoadedImage {
            // Mappings the cut ends inside all leave a piece at its end.
            regions: crate::collapse_equal_starts(without_ranges(self.regions, &writable)),
            writable,
            executable: AddressSet::merged(self.executable),
            owners: self.owners,
        }
    }
}

/// `regions` with every address of `ranges` cut out: a region a range splits
/// becomes the pieces either side of it, sharing its bytes.
///
/// How a read-only view of one image bars another image's
/// [`ElfImage::writable`](super::ElfImage::writable) addresses.
pub fn without_ranges(regions: Vec<MemRegion>, ranges: &AddressSet) -> Vec<MemRegion> {
    let mut kept = Vec::with_capacity(regions.len());
    for region in regions {
        let (start, end) = (region.start_addr(), region.end_addr());
        // An uncut region is kept whole: `collapse_equal_starts` runs over the
        // output, so a slice's start must still be the original's.
        if ranges.overlapping(start, end).is_empty() {
            kept.push(region);
            continue;
        }
        ranges.pieces_outside(start, end, |lo, hi| kept.push(region.slice(lo, hi)));
    }
    kept
}

/// Non-empty `[start, end)` address ranges, ascending, disjoint and
/// non-touching. Only [`merged`](Self::merged) builds a non-empty set, alone
/// or before a cut, so every set is merged, which the queries' binary searches
/// rely on.
#[derive(Clone, Debug, Default)]
pub struct AddressSet(Vec<(u64, u64)>);

impl AddressSet {
    /// `ranges` sorted, with everything that overlaps or touches merged. A
    /// range with `start >= end` holds no address and is dropped.
    pub fn merged(mut ranges: Vec<(u64, u64)>) -> Self {
        ranges.retain(|&(lo, hi)| lo < hi);
        ranges.sort_unstable();
        let mut out: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
        for (lo, hi) in ranges {
            match out.last_mut() {
                Some(last) if lo <= last.1 => last.1 = last.1.max(hi),
                _ => out.push((lo, hi)),
            }
        }
        Self(out)
    }

    /// [`merged`](Self::merged) with every address of `cut_out` removed, by a
    /// binary search over `cut_out`, which is merged as every set is.
    fn merged_without(ranges: Vec<(u64, u64)>, cut_out: &Self) -> Self {
        let merged = Self::merged(ranges);
        let mut out: Vec<(u64, u64)> = Vec::with_capacity(merged.0.len());
        for (lo, hi) in merged.0 {
            cut_out.pieces_outside(lo, hi, |lo, hi| out.push((lo, hi)));
        }
        Self(out)
    }

    /// Each piece of `[lo, hi)` this set does not hold, ascending, to `f`.
    fn pieces_outside(&self, mut lo: u64, hi: u64, mut f: impl FnMut(u64, u64)) {
        for &(c_lo, c_hi) in self.overlapping(lo, hi) {
            if c_lo > lo {
                f(lo, c_lo);
            }
            lo = lo.max(c_hi);
        }
        if lo < hi {
            f(lo, hi);
        }
    }

    /// The ranges overlapping `[lo, hi)`: a binary search, which holds
    /// because the set is merged.
    pub(crate) fn overlapping(&self, lo: u64, hi: u64) -> &[(u64, u64)] {
        let first = self.0.partition_point(|&(_, end)| end <= lo);
        let last = first + self.0[first..].partition_point(|&(start, _)| start < hi);
        &self.0[first..last]
    }

    /// The addresses of `self` not in `cut`.
    pub(crate) fn without(&self, cut: &Self) -> Self {
        Self::merged_without(self.0.clone(), cut)
    }

    /// The ranges, ascending.
    pub fn as_slice(&self) -> &[(u64, u64)] {
        &self.0
    }

    /// Whether `[addr, addr + len)` touches any of the ranges, through
    /// [`overlapping`](Self::overlapping)'s binary search.
    pub(crate) fn touches(&self, addr: u64, len: usize) -> bool {
        len != 0
            && !self
                .overlapping(addr, addr.saturating_add(len as u64))
                .is_empty()
    }

    /// Whether the set holds every address of the non-empty `[lo, hi)`.
    ///
    /// O(log n): the ranges never touch, so only the last one starting at or
    /// below `lo` can hold it.
    pub fn covers(&self, lo: u64, hi: u64) -> bool {
        let i = self.0.partition_point(|&(start, _)| start <= lo);
        i.checked_sub(1).is_some_and(|i| hi <= self.0[i].1)
    }
}

/// Where each ET_REL section is loaded: a synthetic image base upwards, so
/// address 0 stays unmapped.
///
/// An ET_REL object is unlinked, and every toolchain leaves `sh_addr` at 0
/// there, so `.text`, `.text.startup` and `.rodata` all claim VMA 0. Assigning
/// them distinct addresses is the linker's job, and this does it: sections are
/// walked in index order and any whose `sh_addr` runs into already-placed space
/// moves up to `align_up(watermark, sh_addralign)`, the watermark starting at
/// the image base.
///
/// `None` for a linked image, whose `sh_addr`s are real and disjoint, so every
/// address passes through untouched. The layout is filter-independent (every
/// section participates, loaded or not), so the fetch and ROM views agree on
/// where a section is even when only one of them maps it.
pub struct ElfSectionLayout(Option<Rebased>);

/// An ET_REL's synthetic link.
struct Rebased {
    /// Section index -> loaded base, for every section header: an index
    /// absent from it is out of range.
    bases: BTreeMap<usize, u64>,
    /// Sections whose base plus declared size runs past the address space: the
    /// base is where their region would go, and nothing can be addressed
    /// through it.
    unseated: BTreeSet<usize>,
    /// Symbol index -> synthetic address, for every ET_REL symbol the link
    /// would place outside this object: undefined and `SHN_COMMON` ones.
    externs: BTreeMap<usize, u64>,
    /// Where the synthetic GOT starts: slot `i` holds symbol `i`'s address.
    got_base: u64,
    /// Bytes per GOT slot.
    word: u64,
    /// The PowerPC64 TOC pointer, `.TOC.`.
    toc_base: Option<u64>,
}

/// Where an ET_REL's first rebased section is seated, so that address 0 stays
/// unmapped: a mapped 0 makes a null dereference a readable, foldable ROM read.
///
/// Round, obviously not a link-time address, and inside the low 2 GiB so an
/// absolute 32-bit relocation field still holds it.
const ET_REL_IMAGE_BASE: u64 = 0x1000_0000;

/// Page granularity of the synthetic ranges past an ET_REL image: each starts
/// on a page boundary one unmapped page past the previous one.
const ET_REL_PAGE: u64 = 0x1000;

/// Address space per undefined symbol in the extern range.
const EXTERN_STRIDE: u64 = 0x10;

/// `.TOC.` sits this far past the start of the TOC it addresses, so a signed
/// 16-bit offset reaches the first 64 KiB of it.
const PPC64_TOC_BIAS: u64 = 0x8000;

impl ElfSectionLayout {
    /// A rebase whose alignment round-up would run past `u64::MAX` seats the
    /// section at the bare watermark; building its [`MemRegion`] is what
    /// reports an `address + length` overflow. A section whose declared size
    /// runs past `u64::MAX` from its base leaves the watermark alone and
    /// addresses none of its symbols.
    pub fn new(obj: &object::File<'_>) -> Self {
        // A linked image's section addresses are the real ones.
        if obj.kind() != ObjectKind::Relocatable {
            return Self(None);
        }
        let mut bases = BTreeMap::new();
        let mut unseated = BTreeSet::new();
        let mut watermark = ET_REL_IMAGE_BASE;
        let mut toc_section = None;
        for sec in obj.sections() {
            // `SHF_TLS` is allocatable but lives in the per-thread block, not
            // the flat address space: a `.tdata` / `.tbss` symbol's `st_value`
            // is an offset into that block, so giving the section a base here
            // would mint an address that means nothing.
            let alloc = sh_flags(&sec).is_some_and(is_mapped);
            // `sh_size`, not the file bytes: SHT_NOBITS (`.bss`, `.tbss`) has
            // no bytes yet still occupies address space, and its symbols
            // resolve through this base.
            let size = sec.size();
            let addr = sec.address();
            // A non-allocatable or zero-size section occupies no address
            // space, so it stays at its `sh_addr`. The watermark is monotone,
            // so a section DECLARING an address below it is rebased even
            // though that `sh_addr` is authoritative; gcc/clang/`ld -r` emit 0
            // for every ET_REL section, which is what makes the rule safe on
            // toolchain output and only a risk on a hand-crafted object.
            let base = if !alloc || size == 0 || addr >= watermark {
                addr
            } else {
                align_up(watermark, sec.align())
            };
            // A `sh_size` overflowing the address space is malformed and no
            // base holds it. The watermark stays put, everywhere the sections
            // after it could go being inside the range this one claims, and
            // its symbols get no address rather than one resolving into
            // whichever section took their offsets. SHT_NOBITS never validates
            // `sh_size`, having no file bytes to bound it; a section carrying
            // bytes fails the load where its region is built.
            if alloc && size != 0 {
                match base.checked_add(size) {
                    Some(end) => watermark = end,
                    None => {
                        unseated.insert(sec.index().0);
                    }
                }
            }
            if alloc && toc_section.is_none() && sec.name() == Ok(".toc") {
                toc_section = Some(base);
            }
            bases.insert(sec.index().0, base);
        }

        // Past the image, one unmapped page apart: the undefined and common
        // symbols, then the GOT.
        let past = |end: u64| align_up(end, ET_REL_PAGE).saturating_add(ET_REL_PAGE);
        let word = if obj.is_64() { 8 } else { 4 };
        let arch = obj.architecture();
        let mut externs = BTreeMap::new();
        let mut cursor = past(watermark);
        let mut deferred = Vec::new();
        for sym in obj.symbols() {
            if sym.is_common() {
                // `st_value` is the alignment.
                cursor = align_up(cursor, sym.address());
                externs.insert(sym.index().0, cursor);
                cursor = cursor.saturating_add(sym.size().max(1));
            } else if sym.is_undefined() && sym.index().0 != 0 {
                match sym.name() {
                    Ok(".TOC.") if arch == object::Architecture::PowerPc64 => {
                        deferred.push(sym.index().0);
                    }
                    Ok("_GLOBAL_OFFSET_TABLE_") => deferred.push(sym.index().0),
                    _ => {
                        cursor = align_up(cursor, EXTERN_STRIDE);
                        externs.insert(sym.index().0, cursor);
                        cursor = cursor.saturating_add(EXTERN_STRIDE);
                    }
                }
            }
        }
        let got_base = past(cursor);
        let toc_base = (arch == object::Architecture::PowerPc64).then(|| {
            toc_section
                .unwrap_or(got_base)
                .saturating_add(PPC64_TOC_BIAS)
        });
        for index in deferred {
            externs.insert(index, toc_base.unwrap_or(got_base));
        }
        Self(Some(Rebased {
            bases,
            unseated,
            externs,
            got_base,
            word,
            toc_base,
        }))
    }

    /// The address an ET_REL gives a symbol the link would place outside the
    /// object: an undefined symbol gets a distinct one in an unmapped range
    /// past the image, a `SHN_COMMON` one room for its size there. `.TOC.` is
    /// the PowerPC64 TOC pointer and `_GLOBAL_OFFSET_TABLE_` the GOT's start.
    ///
    /// `None` for a defined symbol and for any symbol of a linked image.
    pub fn extern_address(&self, symbol_index: usize) -> Option<u64> {
        self.0.as_ref()?.externs.get(&symbol_index).copied()
    }

    /// Where the synthetic GOT starts, `_GLOBAL_OFFSET_TABLE_`; only an
    /// ET_REL has one.
    pub(crate) fn got_base(&self) -> Option<u64> {
        Some(self.0.as_ref()?.got_base)
    }

    /// The GOT slot holding symbol `symbol_index`'s address, and its width.
    pub(crate) fn got_slot(&self, symbol_index: usize) -> Option<(u64, usize)> {
        let r = self.0.as_ref()?;
        let offset = (symbol_index as u64).checked_mul(r.word)?;
        Some((r.got_base.checked_add(offset)?, r.word as usize))
    }

    /// The PowerPC64 TOC pointer: `.TOC.`, `0x8000` past the object's `.toc`.
    pub(crate) fn toc_base(&self) -> Option<u64> {
        self.0.as_ref()?.toc_base
    }

    /// Where `sec` is loaded.
    pub fn section_base<'d>(&self, sec: &impl ObjectSection<'d>) -> u64 {
        self.0
            .as_ref()
            .and_then(|r| r.bases.get(&sec.index().0).copied())
            .unwrap_or_else(|| sec.address())
    }

    /// Where `sym` resolves to. gABI: an ET_REL `st_value` is an offset from
    /// the start of the section `st_shndx` names, so the address is that
    /// section's base plus it; a linked image's `st_value` is already the
    /// address and no base is recorded. An undefined, absolute or `SHN_COMMON`
    /// symbol has no section index and is returned as-is.
    ///
    /// `None` when an ET_REL `st_shndx` names no section header, or one whose
    /// declared size the address space cannot hold: the offset it declares has
    /// no base, so the symbol has no address.
    pub fn try_symbol_address<'d>(&self, sym: &impl object::ObjectSymbol<'d>) -> Option<u64> {
        let (Some(index), Some(r)) = (sym.section_index(), &self.0) else {
            return Some(sym.address());
        };
        if r.unseated.contains(&index.0) {
            return None;
        }
        Some(r.bases.get(&index.0)?.wrapping_add(sym.address()))
    }

    /// [`try_symbol_address`], reading an out-of-range `st_shndx` as the bare
    /// `st_value`. Naming a symbol is what that serves; patching one is
    /// [`try_symbol_address`]'s.
    ///
    /// [`try_symbol_address`]: Self::try_symbol_address
    pub fn symbol_address<'d>(&self, sym: &impl object::ObjectSymbol<'d>) -> u64 {
        self.try_symbol_address(sym)
            .unwrap_or_else(|| sym.address())
    }
}

/// The spans of executable sections that ARM / AArch64 mapping symbols mark as
/// data: each run of `$d` marks spans to the next `$a` / `$t` / `$x` in its
/// section, or to the section's end. Sorted by start; empty for an image with
/// no `$d`.
///
/// A literal pool sits inside a function's extent and decodes as plausible
/// instructions, so these are the only record that those bytes are not code.
pub fn mapping_symbol_data_ranges(obj: &object::File<'_>) -> Vec<std::ops::Range<u64>> {
    use object::{ObjectSymbol, SymbolKind};
    let layout = ElfSectionLayout::new(obj);
    // Per executable section: (address, is data) for every mapping symbol.
    let mut marks: BTreeMap<usize, Vec<(u64, bool)>> = BTreeMap::new();
    for sym in obj.symbols() {
        let (Ok(name), Some(index)) = (sym.name(), sym.section_index()) else {
            continue;
        };
        if sym.kind() == SymbolKind::Section {
            continue;
        }
        // `$d`, `$a`, `$t`, `$x`, optionally suffixed `.<anything>`.
        let kind = match name.as_bytes() {
            [b'$', k] | [b'$', k, b'.', ..] => *k,
            _ => continue,
        };
        let is_data = match kind {
            b'd' => true,
            b'a' | b't' | b'x' => false,
            _ => continue,
        };
        let Some(address) = layout.try_symbol_address(&sym) else {
            continue;
        };
        marks.entry(index.0).or_default().push((address, is_data));
    }
    let mut out = Vec::new();
    for (index, mut section_marks) in marks {
        let Ok(section) = obj.section_by_index(object::SectionIndex(index)) else {
            continue;
        };
        // By flag, not kind: a debug companion's `.text` is `SHT_NOBITS`.
        let executable = sh_flags(&section).is_some_and(|f| has(f, object::elf::SHF_EXECINSTR));
        if !executable {
            continue;
        }
        let end = layout.section_base(&section).saturating_add(section.size());
        section_marks.sort_unstable();
        // One descending pass carrying the next code mark, so a run of `$d`
        // marks costs O(k) rather than O(k^2) and emits one range instead of k
        // nested ones.
        let mut stop = end;
        let mut run_start: Option<u64> = None;
        for &(start, is_data) in section_marks.iter().rev() {
            if is_data {
                run_start = Some(start);
                continue;
            }
            if let Some(from) = run_start.take()
                && from < stop
            {
                out.push(from..stop);
            }
            stop = start;
        }
        if let Some(from) = run_start
            && from < stop
        {
            out.push(from..stop);
        }
    }
    out.sort_unstable_by_key(|r| r.start);
    out
}

/// Where each ppc64 ELFv1 `.opd` descriptor of an ET_REL enters its code: the
/// `S + A` of the `R_PPC64_ADDR64` on the descriptor's first word. Empty for
/// anything else.
///
/// An unlinked `.opd` holds zeros, so the relocations are the only record of
/// the entry; `S` is rebased through `layout` exactly as a patch's is.
///
/// The tables are those `.symtab`-linked headers name for `.opd`.
pub(crate) fn opd_entries(obj: &object::File<'_>, layout: &ElfSectionLayout) -> BTreeMap<u64, u64> {
    let mut out = BTreeMap::new();
    if obj.architecture() != object::Architecture::PowerPc64
        || obj.kind() != ObjectKind::Relocatable
    {
        return out;
    }
    let (object::File::Elf64(f), Some(opd)) = (obj, obj.section_by_name(".opd")) else {
        return out;
    };
    let base = layout.section_base(&opd);
    for (offset, r_type, sym, addend) in opd_relocations(f, opd.index().0) {
        if r_type != object::elf::R_PPC64_ADDR64 || sym == 0 {
            continue;
        }
        let idx = object::SymbolIndex(sym as usize);
        let Ok(sym) = obj.symbol_by_index(idx) else {
            continue;
        };
        let Some(s) = layout
            .extern_address(idx.0)
            .or_else(|| layout.try_symbol_address(&sym))
        else {
            continue;
        };
        // 0 is never an entry, and leaving it out keeps a descriptor whose
        // relocation names no symbol visible as one.
        let entry = s.wrapping_add(addend as u64);
        if entry != 0 {
            out.insert(base.wrapping_add(offset), entry);
        }
    }
    out
}

/// `(r_offset, r_type, r_sym, r_addend)` of every relocation against the
/// section `target`, see [`opd_entries`].
fn opd_relocations(
    f: &object::read::elf::ElfFile64<'_, object::Endianness>,
    target: usize,
) -> Vec<(u64, u32, u32, i64)> {
    use object::read::elf::SectionHeader as _;
    let endian = f.endian();
    let symtab = f.elf_symbol_table().section();
    f.elf_section_table()
        .iter()
        .filter(|sh| {
            matches!(
                sh.sh_type(endian),
                object::elf::SHT_REL | object::elf::SHT_RELA | object::elf::SHT_CREL
            ) && sh.link(endian) == symtab
                && sh.sh_info(endian) as usize == target
        })
        .flat_map(|sh| super::relocations::raw_entries(f, sh))
        .map(|(offset, r_type, sym, addend)| (offset, r_type, sym, addend.unwrap_or(0)))
        .collect()
}

/// ppc64 ELFv1 function descriptors.
///
/// On that ABI `st_value` of an `STT_FUNC` symbol addresses an 8-byte-aligned
/// {entry, TOC, env} triple in `.opd` rather than code, and the first
/// doubleword is the address to decode from. ELFv2 has no descriptors, and no
/// other architecture defines them.
pub struct OpdTable<'d> {
    descriptors: Descriptors<'d>,
}

/// Where a descriptor's entry word comes from.
enum Descriptors<'d> {
    /// A linked image's `.opd` file bytes, from `base`.
    Bytes {
        base: u64,
        data: &'d [u8],
        endian_le: bool,
    },
    /// An ET_REL's, from [`opd_entries`]: the section's own bytes are zeros.
    Relocated(BTreeMap<u64, u64>),
}

impl<'d> OpdTable<'d> {
    /// `None` unless `obj` is a ppc64 image whose `e_flags` ABI level is not
    /// ELFv2 and which carries a non-empty `.opd`.
    pub fn new(obj: &object::File<'d>) -> Option<Self> {
        if obj.architecture() != object::Architecture::PowerPc64 {
            return None;
        }
        // `e_flags` bits 0..1 are the ABI level. 2 is ELFv2; 1 is ELFv1, and
        // so is the 0 that means "unspecified", which is what a toolchain
        // emitting an `.opd` at all has to mean.
        let object::FileFlags::Elf { e_flags, .. } = obj.flags() else {
            return None;
        };
        if e_flags & 0x3 == 2 {
            return None;
        }
        let opd = obj.section_by_name(".opd")?;
        if obj.kind() == ObjectKind::Relocatable {
            let entries = opd_entries(obj, &ElfSectionLayout::new(obj));
            return (!entries.is_empty()).then_some(Self {
                descriptors: Descriptors::Relocated(entries),
            });
        }
        let data = opd.data().ok()?;
        (data.len() >= 8).then(|| Self {
            descriptors: Descriptors::Bytes {
                base: opd.address(),
                data,
                endian_le: matches!(obj.endianness(), object::Endianness::Little),
            },
        })
    }

    /// The code entry the descriptor at `addr` names, or `None` when `addr` is
    /// not a whole descriptor word in this table, or when the word reads zero.
    ///
    /// "Whole word" is the 8-byte stride the descriptor triple is built from:
    /// a `.opd` offset that is not a multiple of it straddles the {entry, TOC}
    /// boundary and would read half of each as one address. A multiple naming
    /// a descriptor's TOC or environment word is followed as if it were an
    /// entry: which words start a descriptor takes a stride the section does
    /// not carry, the triple being 24 bytes in compiler output and 16 in the
    /// hand-written asm that leaves the environment word off. An ET_REL is
    /// exact instead, each entry word carrying a relocation of its own.
    ///
    /// A zero word is a descriptor nothing filled in: address 0 is never the
    /// entry, and passing `addr` through unfollowed leaves the descriptor
    /// visible.
    pub fn entry_at(&self, addr: u64) -> Option<u64> {
        let (base, data, endian_le) = match &self.descriptors {
            Descriptors::Relocated(entries) => return entries.get(&addr).copied(),
            Descriptors::Bytes {
                base,
                data,
                endian_le,
            } => (*base, *data, *endian_le),
        };
        let off = usize::try_from(addr.checked_sub(base)?).ok()?;
        if off % 8 != 0 {
            return None;
        }
        let entry = crate::bytes::read_uint(data.get(off..off.checked_add(8)?)?, endian_le);
        (entry != 0).then_some(entry)
    }
}

/// `value` rounded up to a multiple of `align`; `align` 0 or 1, or a round-up
/// that would exceed `u64::MAX`, leaves `value` alone.
fn align_up(value: u64, align: u64) -> u64 {
    value
        .checked_next_multiple_of(align.max(1))
        .unwrap_or(value)
}

/// The regions `source` and `filter` select, as windows into `bytes`, the
/// whole image `obj` was parsed from.
///
/// # Errors
///
/// When an accepted segment or section's `data()` can't be read, or its
/// `address + length` would exceed `u64::MAX`.
pub(crate) fn collect_regions(
    obj: &object::File<'_>,
    bytes: &FileBytes,
    source: RegionSource,
    filter: LoadFilter,
    layout: &ElfSectionLayout,
    unread: bool,
) -> Result<LoadedImage> {
    let image = match (source, obj.kind()) {
        (RegionSource::Auto, ObjectKind::Executable | ObjectKind::Dynamic) => {
            collect_loadable_segments(obj, bytes, filter)?
        }
        // ET_REL plus any unknown / core kind. An `.o` has no program headers,
        // and a core dump's segment layout isn't what the analyser wants
        // either, so the section walk is the safer fallback for both.
        _ => collect_loadable_sections_dedup(obj, bytes, filter, layout)?,
    };
    Ok(match filter {
        // A read-only mapping a writable one overlaps is writable there.
        LoadFilter::ImmutableOnly => image.without_writable_except(&AddressSet::default()),
        LoadFilter::ImmutableAndRelocatedGot => {
            image.without_writable_except(&relocated_got_ranges(obj, layout, unread))
        }
        _ => image.into_image(),
    })
}

/// One mapping, `range` being its file extent, as a window into `bytes`.
///
/// # Errors
///
/// When a mapping with bytes has no file extent, or the window does not fit
/// `bytes` or the address space.
fn region_from(bytes: &FileBytes, addr: u64, range: Option<(u64, u64)>) -> Result<MemRegion> {
    let (offset, len) = range
        .ok_or_else(|| anyhow::anyhow!("the mapping at {addr:#x} has bytes but no file extent"))?;
    // The byte at `u64::MAX` is unmappable, so a mapping ending exactly at
    // 2^64 loses that byte rather than failing the load.
    let tops_out = len != 0 && addr.checked_add(len).is_none() && addr.wrapping_add(len) == 0;
    MemRegion::window(addr, bytes, offset, len - u64::from(tops_out))
}

/// One accepted PT_LOAD, carrying everything the segment walk reads.
struct AcceptedSegment {
    addr: u64,
    /// The bytes it both holds and maps.
    file_range: (u64, u64),
}

/// One [`MemRegion`] per accepted PT_LOAD segment, from the bytes it both
/// holds and maps, in program-header order under **widest-wins dedup** on
/// `p_vaddr`. A segment with nothing to load is skipped: a BSS-only one
/// (`p_filesz == 0`), and one mapping nothing (`p_memsz == 0`) however much
/// file it holds.
///
/// The dedup rule is the section walk's, resolved the other way: only one
/// region per start address survives [`crate::MemRegionsLookupTable`], so
/// keeping a narrower mapping would leave the bytes past its end unfetchable
/// even though a wider mapping declares them. "Widest" is by MAPPED extent,
/// so a segment declaring more file than it maps does not win on the strength
/// of bytes that belong to no mapping.
fn collect_loadable_segments(
    obj: &object::File<'_>,
    bytes: &FileBytes,
    filter: LoadFilter,
) -> Result<Walk> {
    let mut out = Walk::default();
    let mut accepted: Vec<AcceptedSegment> = Vec::new();
    for seg in obj.segments() {
        // `obj.segments()` already yields PT_LOAD only, so `p_flags` is read
        // purely for the writable / executable filter axis.
        let object::SegmentFlags::Elf { p_flags } = seg.flags() else {
            continue;
        };
        if p_flags & object::elf::PF_W != 0 {
            out.note_writable(seg.address(), seg.size());
        }
        if p_flags & object::elf::PF_X != 0 {
            out.note_executable(seg.address(), seg.size());
        }
        if !filter.segment_accepts(p_flags) {
            continue;
        }
        // Fails a segment whose file bytes run past the end of the file.
        seg.data().context("failed to parse ELF")?;
        // `p_filesz` is what the FILE holds, `p_memsz` what the image MAPS.
        // The gABI specifies only the `p_memsz > p_filesz` zero-fill
        // direction and leaves the reverse undefined; `object` parses such an
        // image happily, and the two real loaders disagree (Linux `elf_map`
        // sizes the mapping from `p_filesz`, glibc rejects the segment). The
        // mapped extent is taken as `p_memsz` because `writable` /
        // `executable` are tracked on it, so serving the tail would let a ROM
        // view that takes every segment and cuts `writable` back out
        // (`ImmutableAndRelocatedGot`) answer for the tail of a WRITABLE
        // segment, and would answer a straddling fetch from the wrong one.
        // Clamping under-approximates rather than over-serves: an address in
        // the tail reads as unmapped.
        let (file_off, file_len) = seg.file_range();
        let file_len = file_len.min(seg.size());
        if file_len == 0 {
            continue;
        }
        accepted.push(AcceptedSegment {
            addr: seg.address(),
            file_range: (file_off, file_len),
        });
    }

    let mut widest: BTreeMap<u64, usize> = BTreeMap::new();
    for (i, seg) in accepted.iter().enumerate() {
        widest
            .entry(seg.addr)
            // First of a tie, so an image with no collisions is untouched.
            .and_modify(|w| {
                if accepted[*w].file_range.1 < seg.file_range.1 {
                    *w = i;
                }
            })
            .or_insert(i);
    }
    let mut keep: Vec<usize> = widest.into_values().collect();
    keep.sort_unstable();

    for i in keep {
        let seg = &accepted[i];
        out.regions
            .push(region_from(bytes, seg.addr, Some(seg.file_range))?);
    }
    Ok(out)
}

/// One [`MemRegion`] per accepted file-backed section, at its
/// [`ElfSectionLayout`] base, in section-index order under **first-wins dedup**
/// on that base.
///
/// Which sections survive decides both where regions land and which
/// relocation sites an ET_REL owns ([`LoadedImage::owners`]), so both are read
/// off this one walk: two sections holding equal bytes are indistinguishable
/// afterwards, and guessing would write `.rela.data` straight over `.text.f`.
fn collect_loadable_sections_dedup(
    obj: &object::File<'_>,
    bytes: &FileBytes,
    filter: LoadFilter,
    layout: &ElfSectionLayout,
) -> Result<Walk> {
    let mut out = Walk::default();
    let mut seen = BTreeSet::new();
    for sec in obj.sections() {
        let Some(flags) = sh_flags(&sec) else {
            continue;
        };
        let base = layout.section_base(&sec);
        if is_mapped(flags) {
            if has(flags, object::elf::SHF_WRITE) {
                out.note_writable(base, sec.size());
            }
            if has(flags, object::elf::SHF_EXECINSTR) {
                out.note_executable(base, sec.size());
            }
        }
        if !filter.section_accepts(flags)
            || sec.data().context("failed to parse ELF")?.is_empty()
            || !seen.insert(base)
        {
            continue;
        }
        out.regions
            .push(region_from(bytes, base, sec.file_range())?);
        out.owners.insert(sec.index().0);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use object::elf::{PF_R, PF_W, PF_X};
    use proptest::prelude::*;

    /// An address low in the space or within 80 bytes of its top, so ends
    /// saturate at `u64::MAX`.
    fn address() -> impl Strategy<Value = u64> {
        prop_oneof![0u64..72, (u64::MAX - 80)..=u64::MAX]
    }

    proptest! {
        /// `covers` holds exactly when one range of the set holds the query,
        /// and, for a non-empty query, when every address of it is in the
        /// set: touching, overlapping and empty inputs included.
        #[test]
        fn covers_holds_exactly_when_every_address_is_in_the_set(
            ranges in proptest::collection::vec((address(), 0u64..8), 0..8),
            lo in address(),
            len in 0u64..12,
        ) {
            let ranges: Vec<(u64, u64)> =
                ranges.into_iter().map(|(s, n)| (s, s.saturating_add(n))).collect();
            let hi = lo.saturating_add(len);
            let model: std::collections::BTreeSet<u64> =
                ranges.iter().flat_map(|&(s, e)| s..e).collect();
            let set = AddressSet::merged(ranges);
            let in_one = set.as_slice().iter().any(|&(s, e)| s <= lo && hi <= e);
            prop_assert_eq!(set.covers(lo, hi), in_one);
            if lo < hi {
                prop_assert_eq!(set.covers(lo, hi), (lo..hi).all(|a| model.contains(&a)));
            }
        }
    }

    /// An inverted range holds no address, so it is dropped rather than left
    /// to break the order the queries binary-search.
    #[test]
    fn an_inverted_range_is_dropped() {
        let set = AddressSet::merged(vec![(7, 8), (10, 5)]);
        assert_eq!(set.as_slice(), &[(7, 8)]);
        assert!(!set.covers(5, 10));
    }

    /// An empty range holds no address: it neither touches a query across
    /// it nor splits a region cut by the set.
    #[test]
    fn an_empty_range_is_dropped() {
        let set = AddressSet::merged(vec![(10, 10)]);
        assert!(set.as_slice().is_empty());
        assert!(!set.touches(9, 2));
        let region = MemRegion::new(0, vec![0; 20]).unwrap();
        let kept = without_ranges(vec![region], &set);
        assert_eq!(
            kept.iter()
                .map(|r| (r.start_addr(), r.end_addr()))
                .collect::<Vec<_>>(),
            [(0, 20)]
        );
    }

    /// An RWX PT_LOAD is the whole image on firmware / bare-metal / `ld -N`
    /// binaries. Excluding it leaves the instruction-fetch reader with no
    /// regions, reported as "address is not mapped" on the first decode.
    #[test]
    fn code_and_read_only_keeps_an_executable_writable_segment() {
        let f = LoadFilter::CodeAndReadOnly;
        assert!(f.segment_accepts(PF_R | PF_W | PF_X), "RWX is still code");
        assert!(f.segment_accepts(PF_R | PF_X), "RX");
        assert!(f.segment_accepts(PF_R), "RO");
        assert!(
            !f.segment_accepts(PF_R | PF_W),
            "RW data stays out of the ROM"
        );
    }

    /// The segment and section filters must agree on the exec-over-write rule,
    /// since the same `LoadFilter` serves both walks.
    #[test]
    fn segment_and_section_filters_agree_on_exec_over_write() {
        let f = LoadFilter::CodeAndReadOnly;
        let alloc = u64::from(object::elf::SHF_ALLOC);
        let write = u64::from(object::elf::SHF_WRITE);
        let exec = u64::from(object::elf::SHF_EXECINSTR);
        assert_eq!(
            f.segment_accepts(PF_R | PF_W | PF_X),
            f.section_accepts(alloc | write | exec)
        );
        assert_eq!(
            f.segment_accepts(PF_R | PF_W),
            f.section_accepts(alloc | write)
        );
    }

    #[test]
    fn all_allocatable_takes_everything() {
        let f = LoadFilter::AllAllocatable;
        assert!(f.segment_accepts(PF_R | PF_W));
        assert!(f.section_accepts(u64::from(object::elf::SHF_ALLOC)));
    }

    /// "Allocatable" is the floor for every section filter, so membership in
    /// [`LoadedImage::owners`] already implies `SHF_ALLOC`. ET_REL relocation
    /// siting relies on that to avoid a second alloc check.
    #[test]
    fn every_section_filter_requires_alloc() {
        let write = u64::from(object::elf::SHF_WRITE);
        let exec = u64::from(object::elf::SHF_EXECINSTR);
        for f in [
            LoadFilter::CodeAndReadOnly,
            LoadFilter::ImmutableOnly,
            LoadFilter::ImmutableAndRelocatedGot,
            LoadFilter::AllAllocatable,
        ] {
            assert!(!f.section_accepts(0), "no flags at all");
            assert!(!f.section_accepts(write | exec), "SHF_ALLOC absent");
        }
    }

    /// [`ElfSectionLayout::new`] leaves a `SHF_TLS` section at its `sh_addr`,
    /// so a filter that loaded one would seat `.tdata` at VMA 0 on top of
    /// `.text`. Every filter must reject it, `AllAllocatable` included.
    #[test]
    fn no_section_filter_accepts_a_tls_section() {
        let alloc = u64::from(object::elf::SHF_ALLOC);
        let write = u64::from(object::elf::SHF_WRITE);
        let tls = u64::from(object::elf::SHF_TLS);
        for f in [
            LoadFilter::CodeAndReadOnly,
            LoadFilter::ImmutableOnly,
            LoadFilter::ImmutableAndRelocatedGot,
            LoadFilter::AllAllocatable,
        ] {
            assert!(!f.section_accepts(alloc | write | tls), ".tdata");
            assert!(!f.section_accepts(alloc | tls), "SHF_TLS without SHF_WRITE");
            assert!(f.section_accepts(alloc), "a plain allocatable section");
        }
    }
}
