use object::{Architecture as A, Object};

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;

mod apply;
mod implicit;
mod walk;

use apply::apply_one_relocation;
pub use apply::local_entry_offset;
use implicit::{hole_copies, lazy_got_header, mips_implicit_got};
pub(crate) use walk::{RELOCATION_ENTRY_BUDGET, RelocInfo, raw_entries};
use walk::{for_each_reloc_site, relocations_off_dynsym, without_repeated_tables};

use super::encodings::Field;
use crate::elf::dynamic::LoaderView;
use crate::elf::sections::AddressSet;
use crate::{MemRegion, Patch, RegionIndex, Result};

/// Attaches one address-sorted relocation patch list to every region,
/// replacing any it carries. Walks the tables the loader reads (a
/// `PT_DYNAMIC`'s, else the section headers'), with image base 0. Holed: an
/// unlisted kind, a symbol with no address or weak undefined, a value its field
/// cannot hold (a veneer branch too), an IFUNC's value, `R_*_COPY` bytes, the
/// MIPS implicit GOT and the ARM / PPC32 lazy-binding words.
///
/// `layout` must be `obj`'s, `owners` the sections the regions came from.
/// Returns the referenced GOT slots (ET_REL only) as regions.
///
/// # Errors
///
/// When the tables name more than [`RELOCATION_ENTRY_BUDGET`] times the
/// entries the file holds, or a tag-placed table is unreadable and the loader
/// may relocate code.
pub(crate) fn apply_elf_relocations_with(
    regions: &mut [MemRegion],
    obj: &object::File<'_>,
    owners: &std::collections::BTreeSet<usize>,
    layout: &crate::elf::sections::ElfSectionLayout,
    writable: &AddressSet,
    executable: &AddressSet,
    view: &LoaderView,
) -> Result<Vec<MemRegion>> {
    debug_assert!(
        {
            let mut starts: Vec<u64> = regions.iter().map(MemRegion::start_addr).collect();
            starts.sort_unstable();
            starts.windows(2).all(|w| w[0] != w[1])
        },
        "two regions share a start"
    );
    match *view {
        LoaderView::Unreadable(why) if relocates_code(obj) => anyhow::bail!(
            "the image relocates code, but {why}: what it relocates is unknown, \
             and its code would be served unrelocated. Load it with relocations \
             unapplied"
        ),
        LoaderView::OverBudget { entries, budget } => anyhow::bail!(
            "the dynamic relocation tables name {entries} relocation entries, more \
             than the {budget} the file's length allows"
        ),
        _ => {}
    }
    let region_index = RegionIndex::new(regions);
    let mut ctx = Ctx {
        obj,
        layout,
        regions,
        region_index: &region_index,
        writable,
        executable,
        endian_le: matches!(obj.endianness(), object::Endianness::Little),
        insn_le: matches!(obj.endianness(), object::Endianness::Little)
            || obj.architecture() == A::Aarch64,
        opd_entries: crate::elf::sections::opd_entries(obj, layout),
        got: BTreeMap::new(),
        copies: Vec::new(),
        patches: Vec::new(),
    };
    // Rewritten as an ELF because only `object`'s relocation iterators derive
    // the kind, size and encoding.
    let from_sections = matches!(view, LoaderView::NotDynamic);
    let rewritten: Option<Cow<'_, [u8]>> = match view {
        LoaderView::NotDynamic => without_repeated_tables(obj)?.map(Cow::Owned),
        LoaderView::Tables { walked } => walked.as_deref().map(Cow::Borrowed),
        LoaderView::Unreadable(_) | LoaderView::OverBudget { .. } => None,
    };
    let rewritten = rewritten.as_deref().map(object::File::parse).transpose()?;
    if let Some(walked) = rewritten.as_ref().or(from_sections.then_some(obj)) {
        for_each_reloc_site(walked, owners, layout, |site_addr, avail, r, pair| {
            apply_one_relocation(
                &mut ctx,
                RelocSite {
                    addr: site_addr,
                    avail,
                },
                r,
                pair,
            );
        });
        if from_sections {
            relocations_off_dynsym(&mut ctx, walked);
        }
    }
    hole_copies(&mut ctx);
    mips_implicit_got(&mut ctx);
    lazy_got_header(&mut ctx);
    let got = got_regions(&ctx.got, ctx.endian_le)?;
    let mut patches = ctx.patches;
    // Stable: the last patch recorded at a site wins.
    patches.sort_by_key(|p| p.addr);
    let patches: Arc<[Patch]> = patches.into();
    for region in regions {
        region.attach_patches(&patches);
    }
    Ok(got)
}

/// What every relocation in one walk reads, and what it records.
struct Ctx<'a, 'd> {
    obj: &'a object::File<'d>,
    layout: &'a crate::elf::sections::ElfSectionLayout,
    /// Unique starts (`collapse_equal_starts`).
    regions: &'a [MemRegion],
    region_index: &'a RegionIndex,
    writable: &'a AddressSet,
    executable: &'a AddressSet,
    endian_le: bool,
    /// Byte order of an instruction field: A64 is always little-endian; ARM
    /// BE32 is big-endian (BE8's swap is the link's).
    insn_le: bool,
    /// ELFv1 descriptor address -> the code entry its first word relocates to.
    opd_entries: BTreeMap<u64, u64>,
    /// GOT slot address -> (the symbol address it holds, slot width).
    got: BTreeMap<u64, (u64, usize)>,
    /// `[start, end)` of each `R_*_COPY` object, and its relocation type.
    copies: Vec<(u64, u64, u32)>,
    /// Every patch the walk records, in record order.
    patches: Vec<Patch>,
}

impl<'a> Ctx<'a, '_> {
    /// The byte order `field` is read and patched in.
    fn field_le(&self, field: Field) -> bool {
        match field {
            Field::Data { .. } => self.endian_le,
            _ => self.insn_le,
        }
    }

    /// The highest-start region holding all of `[addr, addr + width)`, which
    /// serves a read of the whole field.
    fn holding(&self, addr: u64, width: usize) -> Option<&'a MemRegion> {
        let regions = self.regions;
        let i = self.region_index.covering(addr, width as u64).next()?;
        Some(&regions[i])
    }

    /// [`holding`](Self::holding), or `None` when the field runs past
    /// `site.avail` into a neighbouring section.
    fn holder(&self, site: RelocSite, addr: u64, width: usize) -> Option<&'a MemRegion> {
        // Saturating keeps a malformed MIPS pair on the `None` path.
        if addr.wrapping_sub(site.addr).saturating_add(width as u64) > site.avail {
            return None;
        }
        self.holding(addr, width)
    }

    /// [`holder`](Self::holder), plus a hole over a field inside
    /// `site.avail` that no one region holds whole but one holds the start of.
    fn holder_or_hole(
        &mut self,
        site: RelocSite,
        addr: u64,
        width: usize,
        r_type: u32,
    ) -> Option<&'a MemRegion> {
        let holder = self.holder(site, addr, width);
        if holder.is_none()
            && addr.wrapping_sub(site.addr).saturating_add(width as u64) <= site.avail
        {
            let first = self.holding(addr, 1);
            self.unmodelled(first, addr, width, r_type);
        }
        holder
    }

    /// Records the low `width` bytes of `value` at `addr` when a region holds
    /// the field; every overlapping region serves it, clipped to its bytes.
    fn record(
        &mut self,
        holder: Option<&MemRegion>,
        addr: u64,
        value: u64,
        width: usize,
        endian_le: bool,
    ) {
        if holder.is_some() {
            self.patches.push(Patch::new(addr, value, width, endian_le));
        }
    }

    /// Whether the field lies in an executable mapping.
    fn in_code(&self, addr: u64, width: usize) -> bool {
        self.executable.touches(addr, width)
    }

    /// A relocation whose value is not computed: when a region holds the
    /// field, a hole over it per patch-wide piece, skipping writable
    /// non-executable bytes.
    fn unmodelled(&mut self, holder: Option<&MemRegion>, addr: u64, width: usize, r_type: u32) {
        if holder.is_none() {
            return;
        }
        let end = addr.saturating_add(width as u64);
        let mut at = addr;
        while at < end {
            let piece = (end - at).min(crate::MAX_PATCH_LEN) as usize;
            if !self.writable.touches(at, piece) || self.executable.touches(at, piece) {
                self.patches.push(Patch::hole(at, piece, r_type));
            }
            at += piece as u64;
        }
    }
}

/// Where a field lands, and the bytes of its owning section left from there.
#[derive(Clone, Copy)]
struct RelocSite {
    addr: u64,
    avail: u64,
}

/// The referenced GOT slots, adjacent ones sharing a region.
fn got_regions(got: &BTreeMap<u64, (u64, usize)>, endian_le: bool) -> Result<Vec<MemRegion>> {
    let mut out = Vec::new();
    let mut run: Option<(u64, Vec<u8>)> = None;
    for (&addr, &(value, width)) in got {
        // `got_slot` yields 4 or 8; the slices below index blind.
        if width == 0 || width > 8 {
            anyhow::bail!("GOT slot at {addr:#x} has width {width}, not 1..=8");
        }
        let mut bytes = vec![0u8; width];
        crate::bytes::write_uint(&mut bytes, value, endian_le);
        match &mut run {
            Some((start, data)) if *start + data.len() as u64 == addr => data.extend(bytes),
            _ => {
                if let Some((start, data)) = run.take() {
                    out.push(MemRegion::new(start, data)?);
                }
                run = Some((addr, bytes));
            }
        }
    }
    if let Some((start, data)) = run {
        out.push(MemRegion::new(start, data)?);
    }
    Ok(out)
}

/// Whether the loader may relocate code: `DT_TEXTREL`, `DF_TEXTREL`, an RWX
/// `PT_LOAD`, or an unreadable dynamic array.
fn relocates_code(obj: &object::File<'_>) -> bool {
    fn rwx<Elf: object::read::elf::FileHeader>(f: &object::read::elf::ElfFile<'_, Elf>) -> bool {
        use object::read::elf::ProgramHeader as _;
        let endian = f.endian();
        let rwx = object::elf::PF_W | object::elf::PF_X;
        f.elf_program_headers()
            .iter()
            .any(|ph| ph.p_type(endian) == object::elf::PT_LOAD && ph.p_flags(endian) & rwx == rwx)
    }
    let textrel = match crate::elf::dynamic::dynamic_tags(obj) {
        None => false,
        Some(None) => true,
        Some(Some(t)) => {
            t.get(object::elf::DT_TEXTREL).is_some()
                || t.get(object::elf::DT_FLAGS)
                    .is_some_and(|v| v & u64::from(object::elf::DF_TEXTREL) != 0)
        }
    };
    textrel
        || match obj {
            object::File::Elf32(f) => rwx(f),
            object::File::Elf64(f) => rwx(f),
            _ => false,
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elf::encodings::{Kind, Value, classify};

    /// Re-applying over one region set replaces the patch list.
    #[test]
    fn applying_twice_serves_what_applying_once_served() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/out/x64/elf_relocs.elf");
        assert!(
            path.exists(),
            "{} is not built; run `make -C fixtures`",
            path.display()
        );
        let filter = crate::elf::sections::LoadFilter::AllAllocatable;
        let owned = crate::OwnedElf::open(&path).expect("open");
        let obj = owned.checked_file().expect("the mapped file is unchanged");
        let layout = crate::elf::sections::ElfSectionLayout::new(&obj);
        let image = owned
            .regions_with(
                &obj,
                &layout,
                crate::elf::sections::RegionSource::Auto,
                filter,
                false,
                &LoaderView::NotDynamic,
            )
            .expect("regions");
        let (mut regions, writable, executable, owners) = (
            image.regions,
            image.writable,
            image.executable,
            image.owners,
        );

        let bytes = |regions: &[MemRegion]| -> Vec<Vec<u8>> {
            regions
                .iter()
                .map(|r| {
                    let mut out = vec![0u8; (r.end_addr() - r.start_addr()) as usize];
                    r.read(r.start_addr(), &mut out);
                    out
                })
                .collect()
        };
        apply_elf_relocations_with(
            &mut regions,
            &obj,
            &owners,
            &layout,
            &writable,
            &executable,
            &crate::elf::dynamic::loader_view(&obj),
        )
        .expect("apply");
        let once = bytes(&regions);
        apply_elf_relocations_with(
            &mut regions,
            &obj,
            &owners,
            &layout,
            &writable,
            &executable,
            &crate::elf::dynamic::loader_view(&obj),
        )
        .expect("re-apply");
        assert_eq!(once, bytes(&regions));
    }

    /// An 8-byte field near a region nested in another reads back patched
    /// from either: straddling its end at every width, starting at its last
    /// byte, and starting below it (a region keeps the patches up to
    /// `MAX_PATCH_LEN - 1` bytes before it).
    #[test]
    fn a_field_around_an_inner_region_reads_back_patched() {
        let value = 0xdead_beef_feed_face_u64.to_le_bytes();
        // (field, read at, bytes of `value` the read serves)
        for (field, at, want) in [
            (0x1084, 0x1084, 0..4),
            (0x1084, 0x1084, 0..8),
            (0x1087, 0x1087, 0..1),
            (0x107c, 0x1080, 4..8),
        ] {
            let mut regions = vec![
                MemRegion::new(0x1000, vec![0u8; 0x100]).unwrap(),
                MemRegion::new(0x1080, vec![0u8; 8]).unwrap(),
            ];
            let all: Arc<[Patch]> =
                vec![Patch::new(field, u64::from_le_bytes(value), 8, true)].into();
            for region in &mut regions {
                region.attach_patches(&all);
            }
            let table = crate::MemRegionsLookupTable::new(regions);
            let mut got = vec![0u8; want.len()];
            table
                .read_exact(at, &mut got)
                .unwrap_or_else(|e| panic!("{field:#x} at {at:#x}: {e:?}"));
            assert_eq!(got, value[want], "{field:#x} at {at:#x}");
        }
    }

    /// A relocation the word domain builds: `r_type` against symbol 1 or
    /// STN_UNDEF, MIPS64's second and third types `r_type2` / `r_type3`.
    fn word_info(r_type: u32, (r_type2, r_type3): (u32, u32), sym: bool) -> RelocInfo {
        RelocInfo {
            r_type,
            r_type2,
            r_type3,
            sym: sym.then_some(object::SymbolIndex(1)),
            rela_addend: Some(0),
            generic: None,
        }
    }

    /// Every type number below 1101 on every arch the word rows serve, against
    /// a symbol and none, MIPS64 with no composite, with `R_MIPS_64` or
    /// `R_MIPS_SUB` second, and with `R_MIPS_64` then `R_MIPS_SUB`.
    fn word_domain() -> impl Iterator<Item = (A, RelocInfo)> {
        [
            A::X86_64,
            A::I386,
            A::Aarch64,
            A::Arm,
            A::PowerPc,
            A::PowerPc64,
            A::Mips,
            A::Mips64,
        ]
        .into_iter()
        .flat_map(|arch| {
            use object::elf::{R_MIPS_64, R_MIPS_NONE, R_MIPS_SUB};
            let composites: &'static [(u32, u32)] = if arch == A::Mips64 {
                &[
                    (R_MIPS_NONE, R_MIPS_NONE),
                    (R_MIPS_64, R_MIPS_NONE),
                    (R_MIPS_SUB, R_MIPS_NONE),
                    (R_MIPS_64, R_MIPS_SUB),
                ]
            } else {
                &[(R_MIPS_NONE, R_MIPS_NONE)]
            };
            composites.iter().flat_map(move |&rest| {
                (0..=1100u32).flat_map(move |r_type| {
                    [false, true].map(move |sym| (arch, word_info(r_type, rest, sym)))
                })
            })
        })
    }

    const D4: Field = Field::Data {
        bytes: 4,
        signed: false,
    };
    const D8: Field = Field::Data {
        bytes: 8,
        signed: false,
    };
    const S4: Field = Field::Data {
        bytes: 4,
        signed: true,
    };
    const S8: Field = Field::Data {
        bytes: 8,
        signed: true,
    };

    /// `(arch, r_type, r_type2, symbol, value, field)`; a third type changes
    /// no row.
    type WordRow = (A, u32, u32, Option<bool>, Value, Field);

    /// The word rows, a `None` symbol matching both.
    const WORD_ROWS: &[WordRow] = {
        use Value::{Abs, ImageRelative as Ir, Indirect as Ind, Pcrel, Slot};
        use object::elf as e;
        &[
            (A::X86_64, e::R_X86_64_RELATIVE, 0, None, Ir, D8),
            (A::X86_64, e::R_X86_64_IRELATIVE, 0, None, Ind, D8),
            (A::X86_64, e::R_X86_64_GLOB_DAT, 0, None, Slot, D8),
            (A::X86_64, e::R_X86_64_JUMP_SLOT, 0, None, Slot, D8),
            (A::I386, e::R_386_RELATIVE, 0, None, Ir, D4),
            (A::I386, e::R_386_IRELATIVE, 0, None, Ind, D4),
            (A::I386, e::R_386_GLOB_DAT, 0, None, Slot, D4),
            (A::I386, e::R_386_JMP_SLOT, 0, None, Slot, D4),
            (A::Aarch64, e::R_AARCH64_RELATIVE, 0, None, Ir, D8),
            (A::Aarch64, e::R_AARCH64_IRELATIVE, 0, None, Ind, D8),
            (A::Aarch64, e::R_AARCH64_GLOB_DAT, 0, None, Slot, D8),
            (A::Aarch64, e::R_AARCH64_JUMP_SLOT, 0, None, Slot, D8),
            (A::Arm, e::R_ARM_RELATIVE, 0, None, Ir, D4),
            (A::Arm, e::R_ARM_IRELATIVE, 0, None, Ind, D4),
            (A::Arm, e::R_ARM_GLOB_DAT, 0, None, Slot, D4),
            (A::Arm, e::R_ARM_JUMP_SLOT, 0, None, Slot, D4),
            (A::Arm, e::R_ARM_REL32, 0, None, Pcrel, S4),
            (A::PowerPc, e::R_PPC_RELATIVE, 0, None, Ir, D4),
            (A::PowerPc, e::R_PPC_IRELATIVE, 0, None, Ind, D4),
            (A::PowerPc, e::R_PPC_GLOB_DAT, 0, None, Slot, D4),
            (A::PowerPc, e::R_PPC_JMP_SLOT, 0, None, Slot, D4),
            (A::PowerPc, e::R_PPC_REL32, 0, None, Pcrel, S4),
            (A::PowerPc64, e::R_PPC64_RELATIVE, 0, None, Ir, D8),
            (A::PowerPc64, e::R_PPC64_IRELATIVE, 0, None, Ind, D8),
            (A::PowerPc64, e::R_PPC64_GLOB_DAT, 0, None, Slot, D8),
            (A::PowerPc64, e::R_PPC64_JMP_SLOT, 0, None, Slot, D8),
            (A::PowerPc64, e::R_PPC64_REL32, 0, None, Pcrel, S4),
            (A::PowerPc64, e::R_PPC64_REL64, 0, None, Pcrel, S8),
            (A::Mips, e::R_MIPS_REL32, 0, Some(false), Ir, D4),
            (A::Mips, e::R_MIPS_REL32, 0, Some(true), Abs, D4),
            (A::Mips, e::R_MIPS_GLOB_DAT, 0, None, Slot, D4),
            (A::Mips, e::R_MIPS_JUMP_SLOT, 0, None, Slot, D4),
            (A::Mips64, e::R_MIPS_REL32, 0, Some(false), Ir, D4),
            (
                A::Mips64,
                e::R_MIPS_REL32,
                e::R_MIPS_64,
                Some(false),
                Ir,
                D8,
            ),
            (A::Mips64, e::R_MIPS_REL32, 0, Some(true), Abs, D4),
            (
                A::Mips64,
                e::R_MIPS_REL32,
                e::R_MIPS_64,
                Some(true),
                Abs,
                D8,
            ),
            (
                A::Mips64,
                e::R_MIPS_REL32,
                e::R_MIPS_SUB,
                Some(false),
                Ir,
                D4,
            ),
            (
                A::Mips64,
                e::R_MIPS_REL32,
                e::R_MIPS_SUB,
                Some(true),
                Abs,
                D4,
            ),
            (A::Mips64, e::R_MIPS_GLOB_DAT, 0, None, Slot, D8),
            (A::Mips64, e::R_MIPS_GLOB_DAT, e::R_MIPS_64, None, Slot, D8),
            (A::Mips64, e::R_MIPS_GLOB_DAT, e::R_MIPS_SUB, None, Slot, D8),
            (A::Mips64, e::R_MIPS_JUMP_SLOT, 0, None, Slot, D8),
            (A::Mips64, e::R_MIPS_JUMP_SLOT, e::R_MIPS_64, None, Slot, D8),
            (
                A::Mips64,
                e::R_MIPS_JUMP_SLOT,
                e::R_MIPS_SUB,
                None,
                Slot,
                D8,
            ),
        ]
    };

    /// The row `WORD_ROWS` gives `r` on `arch`.
    fn word_row(arch: A, r: &RelocInfo) -> Option<(Value, Field)> {
        WORD_ROWS
            .iter()
            .find(|row| {
                row.0 == arch
                    && row.1 == r.r_type
                    && row.2 == r.r_type2
                    && row.3.is_none_or(|sym| sym == r.sym.is_some())
            })
            .map(|row| (row.4, row.5))
    }

    /// The word rows `classify` answers, over every type number they could
    /// be; no other type takes an image-relative, indirect or slot row.
    #[test]
    fn classify_pins_the_word_rows() {
        let mut rows = 0;
        for (arch, r) in word_domain() {
            match word_row(arch, &r) {
                Some((value, field)) => {
                    rows += 1;
                    assert_eq!(
                        classify(arch, &r),
                        Some(Kind::Compute(value, field)),
                        "{arch:?} {r:?}"
                    );
                }
                None => assert!(
                    !matches!(
                        classify(arch, &r),
                        Some(Kind::Compute(
                            Value::ImageRelative | Value::Indirect | Value::Slot,
                            _
                        ))
                    ),
                    "{arch:?} {r:?} takes a word row WORD_ROWS lacks"
                ),
            }
        }
        assert_eq!(
            rows, 86,
            "44 rows, the 36 either-symbol ones counted twice, and the 4 \
             `R_MIPS_64` ones 6 more times under a third type"
        );
    }

    /// `R_*_COPY` is the one copy row on each arch, and only as a single
    /// MIPS type.
    #[test]
    fn classify_pins_the_copy_rows() {
        use object::elf as e;
        let copy = |arch: A| match arch {
            A::X86_64 => e::R_X86_64_COPY,
            A::I386 => e::R_386_COPY,
            A::Aarch64 => e::R_AARCH64_COPY,
            A::Arm => e::R_ARM_COPY,
            A::PowerPc | A::PowerPc64 => e::R_PPC_COPY,
            _ => e::R_MIPS_COPY,
        };
        let mut rows = 0;
        for (arch, r) in word_domain() {
            let want = r.r_type == copy(arch) && r.r_type2 == e::R_MIPS_NONE;
            rows += usize::from(want);
            assert_eq!(
                classify(arch, &r) == Some(Kind::Copy),
                want,
                "{arch:?} {r:?}"
            );
        }
        assert_eq!(rows, 16, "8 arches, against a symbol and none");
    }

    /// The TLS rows and their widths; PPC32 shares the numbers of its 64-bit
    /// twins, whose 4-byte fields take no row, and the MIPS rows are
    /// MIPS64's alone.
    #[test]
    fn classify_pins_the_tls_rows() {
        use object::elf as e;
        let rows: &[(A, u32, u8)] = &[
            (A::X86_64, e::R_X86_64_DTPMOD64, 8),
            (A::X86_64, e::R_X86_64_DTPOFF64, 8),
            (A::X86_64, e::R_X86_64_TPOFF64, 8),
            (A::X86_64, e::R_X86_64_TLSDESC, 16),
            (A::I386, e::R_386_TLS_DESC, 8),
            (A::Aarch64, e::R_AARCH64_TLS_DTPMOD, 8),
            (A::Aarch64, e::R_AARCH64_TLS_DTPREL, 8),
            (A::Aarch64, e::R_AARCH64_TLS_TPREL, 8),
            (A::Aarch64, e::R_AARCH64_TLSDESC, 16),
            (A::Arm, e::R_ARM_TLS_DESC, 8),
            (A::PowerPc64, e::R_PPC64_DTPMOD64, 8),
            (A::PowerPc64, e::R_PPC64_TPREL64, 8),
            (A::PowerPc64, e::R_PPC64_DTPREL64, 8),
            (A::Mips64, e::R_MIPS_TLS_DTPMOD64, 8),
            (A::Mips64, e::R_MIPS_TLS_DTPREL64, 8),
            (A::Mips64, e::R_MIPS_TLS_TPREL64, 8),
        ];
        let mut seen = 0;
        for (arch, r) in word_domain() {
            let want = rows
                .iter()
                .find(|row| row.0 == arch && row.1 == r.r_type && r.r_type2 == e::R_MIPS_NONE)
                .map(|row| {
                    Kind::Compute(
                        Value::Tls,
                        Field::Data {
                            bytes: row.2,
                            signed: false,
                        },
                    )
                });
            seen += usize::from(want.is_some());
            let got = classify(arch, &r);
            if want.is_some() || matches!(got, Some(Kind::Compute(Value::Tls, _))) {
                assert_eq!(got, want, "{arch:?} {r:?}");
            }
        }
        assert_eq!(seen, 32, "16 rows, against a symbol and none");
    }
}
