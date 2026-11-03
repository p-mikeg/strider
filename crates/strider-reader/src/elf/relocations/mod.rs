//! ELF relocation application, as one sorted patch list per image that every
//! region shares and a read applies where it crosses a site.
//!
//! An image that has not been through its linker, or through `ld.so`, holds
//! only an addend, or nothing, in every cross-reference field. An ET_REL's
//! `call rel32` sits as `e8 fc ff ff ff` or `e8 00 00 00 00` under an
//! `R_X86_64_PLT32` the linker would resolve, and an AArch64 `bl` as a branch
//! to itself; an ET_DYN's dispatch-table slots and GOT/PLT entries hold what
//! `ld` left under `.rela.dyn` / `.rela.plt` until `ld.so` fills them. Every
//! one of those decodes as a plausible reference into the image itself. This
//! applies the relocations statically.
//!
//! A relocation whose value is not computed is not left in place where it
//! would be decoded or folded: its field becomes a hole no read serves, by the
//! rule [`Ctx::unmodelled`] states.

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

/// Attaches one relocation patch list, sorted by address, to every region,
/// walking the tables the loader reads: for a linked image with a
/// `PT_DYNAMIC` the ones its tags place
/// ([`loader_view`](crate::elf::dynamic::loader_view)), else
/// `obj.dynamic_relocations()` for ET_EXEC / ET_DYN and per-section
/// `section.relocations()` for ET_REL. Each
/// region keeps the part that can overlap it, so a read searches no more
/// patches than a list of its own would hold.
///
/// # Supported
///
/// Every row [`classify`](crate::elf::encodings::classify) lists, which says what each `(arch, r_type)`
/// computes and the field it is encoded into: instruction immediates, GOT and
/// TOC references, `R_*_RELATIVE` (image base modelled 0; an `SHT_REL` one's
/// field already holds the whole value and is served as assembled), GOT/PLT
/// slots and PC-relative words. A GOT reference resolves
/// through a synthetic GOT slot holding the symbol's address, which exists
/// only for an ET_REL.
///
/// Otherwise the plain data fields `object` describes: `Absolute` is `S + A`
/// (`R_X86_64_64`), `Relative` `S + A - P` (`R_X86_64_PC32`,
/// `R_AARCH64_PREL32`), and `PltRelative` valued the same, no PLT being
/// materialised (`R_X86_64_PLT32`).
///
/// `S` exists only for a symbol target: a value reading it against
/// STN_UNDEF is not computed.
///
/// On an ET_REL an undefined or `SHN_COMMON` symbol resolves to its
/// [`ElfSectionLayout::extern_address`](crate::elf::sections::ElfSectionLayout::extern_address),
/// outside every mapping. An undefined `STB_WEAK` one is the exception: see
/// `Target::weak_undefined`.
///
/// An instruction field is read and written in the instruction byte order,
/// which on AArch64 is little-endian whatever `EI_DATA` says; a data field
/// follows `EI_DATA`.
///
/// # Not computed
///
/// A relocation whose value is not computed (a kind not listed above, a symbol
/// with no address or a weak undefined one, a value its field cannot hold, a
/// branch needing a veneer, an IFUNC's in data) leaves no plausible bytes
/// behind where they would be decoded or folded: [`Ctx::unmodelled`] says
/// which fields become holes, through which decoding or folding fails.
///
/// An IFUNC's value is what its resolver returns, which only running it
/// computes: `R_*_IRELATIVE`, and every value reading `S` of a defined
/// `STT_GNU_IFUNC` symbol, whose `st_value` is the resolver, outside an
/// executable mapping. A GOT reference to one still addresses its slot, which
/// is left unmapped; inside an executable mapping `S` is the symbol's address,
/// as [`Ctx::in_code`] says.
///
/// An `R_*_COPY` object holds what the loader copies from the library
/// defining it: every byte of its `st_size` is a hole, see [`hole_copies`].
///
/// A MIPS image's primary GOT holds entries no relocation record covers:
/// [`mips_implicit_got`] holes the ones the loader writes, and
/// [`lazy_got_header`] the ARM and PPC32 lazy-binding words.
///
/// Any patch list the regions already carry is replaced, so re-applying over
/// one region set is idempotent.
///
/// `layout` must be the one built for `obj`, `owners` the sections the regions
/// were loaded from ([`LoadedImage::owners`](crate::elf::sections::LoadedImage::owners))
/// and `writable` the image's writable mappings.
///
/// Returns the GOT slots the relocations referenced, as regions of their own.
///
/// # Errors
///
/// When the relocation tables walked name more entries than
/// [`RELOCATION_ENTRY_BUDGET`] times what the file holds (see
/// `without_repeated_tables` and
/// [`loader_view`](crate::elf::dynamic::loader_view)); and when a table the
/// dynamic tags place cannot be read or decoded and the loader may relocate
/// code (see `relocates_code`).
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
    // One lookup per relocation instead of a scan of every region; an ET_REL
    // carries one region per SHF_ALLOC section.
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
    // A linked image with a `PT_DYNAMIC` is relocated from the tables its tags
    // place, as an ELF of their own; anything else from its section headers,
    // a repeated table dropped. Both are rewritten bytes for `object` to walk
    // because only its relocation iterators derive the kind, size and
    // encoding `apply_plain` reads, from a parser it does not export.
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
    // Stable, so the last patch recorded at a site is the one a read applies.
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
    /// Unique starts (`collapse_equal_starts`), so the highest-start region
    /// holding a field is the one a read of it reaches.
    regions: &'a [MemRegion],
    region_index: &'a RegionIndex,
    writable: &'a AddressSet,
    executable: &'a AddressSet,
    endian_le: bool,
    /// Byte order of an instruction field: A64 instructions are little-endian
    /// whatever `EI_DATA` says, and only data follows it. ARM BE32 instructions
    /// really are big-endian, BE8's swap being the link's.
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

    /// The highest-start region holding all of `[addr, addr + width)`: the
    /// one a read of the whole field is served from.
    fn holding(&self, addr: u64, width: usize) -> Option<&'a MemRegion> {
        let regions = self.regions;
        let i = self.region_index.covering(addr, width as u64).next()?;
        Some(&regions[i])
    }

    /// [`holding`](Self::holding) for a field of `site`, and `None` too when
    /// the field runs past `site.avail`, what is left of the section owning
    /// the site. A field past its section, or past the end of every mapping
    /// holding its first byte, would write a neighbour's bytes, so it is
    /// not patched. [`holder_or_hole`](Self::holder_or_hole) holes the
    /// latter, and the walk of tables off `.dynsym` asks for a hole's first
    /// byte only, which leaves no mapped byte of the field served.
    fn holder(&self, site: RelocSite, addr: u64, width: usize) -> Option<&'a MemRegion> {
        // `addr` is `site.addr` plus a non-negative `mips_half_field_skew` and
        // `width` is at most 8, so neither wraps; saturating anyway keeps a
        // malformed pair on the `None` path.
        if addr.wrapping_sub(site.addr).saturating_add(width as u64) > site.avail {
            return None;
        }
        self.holding(addr, width)
    }

    /// [`holder`](Self::holder), and a hole over the field when no one region
    /// holds it but one holds its first byte, inside `site.avail`: a field
    /// over two adjacent mappings, or past the end of the one holding its
    /// start, can be neither read nor patched whole.
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
    /// the field. Every region overlapping it then serves the patch, clipped
    /// to its own bytes: a narrow read inside a field that straddles an inner
    /// region's end is that region's, a wide one the outer region's.
    fn record(
        &mut self,
        holder: Option<&MemRegion>,
        addr: u64,
        value: u64,
        width: usize,
        endian_le: bool,
    ) {
        if holder.is_some()
            && let Some(patch) = Patch::new(addr, value, width, endian_le)
        {
            self.patches.push(patch);
        }
    }

    /// Whether the field lies in an executable mapping, where it may be part
    /// of an instruction. There a relocation against a defined IFUNC takes
    /// the symbol's own address: a hole would stop the decoder, and the
    /// symbol is how the image names the function a `call` reaches (a
    /// FreeBSD kernel's `call memcpy`, which its linker binds at boot).
    ///
    /// A PC-relative call and a PC-relative `lea` look alike, so two limits
    /// follow: an address taken in code (`lea f(%rip)`, `mov $f`) and a data
    /// word in an executable mapping (FreeBSD 12's `.rodata`) take the
    /// symbol's address too, where at run time they hold the resolver's
    /// result.
    fn in_code(&self, addr: u64, width: usize) -> bool {
        self.executable.touches(addr, width)
    }

    /// A relocation whose value is not computed: a hole over the field, when
    /// a region holds it, unless the field is writable and NOT executable at
    /// runtime.
    ///
    /// Writable data the program rewrites anyway is not worth a hole.  A
    /// writable EXECUTABLE field is a different thing: `CodeAndReadOnly` keeps
    /// an RWX mapping on purpose (firmware, `ld -N`), so those bytes reach the
    /// decoder, and serving them unrelocated hands it a plausible wrong
    /// instruction with nothing said about it. A ROM view's served `.got` /
    /// `.data.rel.ro` has left `writable`, so a field there is a hole too.
    ///
    /// A field wider than a patch takes one hole per patch-wide piece.
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

/// A relocation site: where the field lands, and how many bytes of the section
/// owning it remain from there.
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
        // `ElfSectionLayout::got_slot`, the only producer, yields the image's
        // word, 4 or 8, so this cannot fire -- but both slices below index
        // blind, and `8 - width` underflows for a wider one. Its twin
        // `Patch::new` already checks.
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

/// Whether the loader may relocate code: `DT_TEXTREL`, `DF_TEXTREL`, or a
/// `PT_LOAD` both writable and executable. A dynamic array that cannot be
/// read says nothing, so it may.
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

    /// Re-applying over one region set overwrites the patch list rather than
    /// appending to it, so the bytes a second pass serves are the first pass's.
    /// Only reachable from inside the crate: a caller gets its relocations
    /// through [`crate::OwnedElf::image`], which loads a fresh set each time.
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

    /// Which region serves a read depends on the read's WIDTH, so a field
    /// straddling an inner region's end has to read back patched at every
    /// width: every region overlapping it keeps the patch.
    #[test]
    fn a_field_over_an_inner_regions_end_reads_back_patched_at_every_width() {
        let field = 0x1084;
        let value = 0xdead_beef_feed_face_u64;
        let mut regions = vec![
            MemRegion::new(0x1000, vec![0u8; 0x100]).unwrap(),
            MemRegion::new(0x1080, vec![0u8; 8]).unwrap(),
        ];
        let all: Arc<[Patch]> =
            vec![Patch::new(field, value, 8, true).expect("an 8 byte field")].into();
        for region in &mut regions {
            region.attach_patches(&all);
        }

        let table = crate::MemRegionsLookupTable::new(regions);
        let mut narrow = [0u8; 4];
        table
            .read_exact(field, &mut narrow)
            .expect("the inner region");
        let mut wide = [0u8; 8];
        table
            .read_exact(field, &mut wide)
            .expect("the outer region");
        assert_eq!(wide, value.to_le_bytes());
        assert_eq!(narrow, wide[..4]);
    }

    /// A field starting at a region's last byte is that region's too: a
    /// one-byte read there serves the field's first byte patched.
    #[test]
    fn a_field_at_an_inner_regions_last_byte_reads_back_patched() {
        let field = 0x1087;
        let value = 0xdead_beef_feed_face_u64;
        let mut regions = vec![
            MemRegion::new(0x1000, vec![0u8; 0x100]).unwrap(),
            MemRegion::new(0x1080, vec![0u8; 8]).unwrap(),
        ];
        let all: Arc<[Patch]> =
            vec![Patch::new(field, value, 8, true).expect("an 8 byte field")].into();
        for region in &mut regions {
            region.attach_patches(&all);
        }

        let table = crate::MemRegionsLookupTable::new(regions);
        let mut last = [0u8; 1];
        table
            .read_exact(field, &mut last)
            .expect("the inner region");
        assert_eq!(last, value.to_le_bytes()[..1]);
    }

    /// A field starting below a region's start still lands in it: a region
    /// keeps the patches up to `MAX_PATCH_LEN - 1` bytes before it, so the
    /// inner region serves the field's tail patched.
    #[test]
    fn a_field_over_an_inner_regions_start_reads_back_patched() {
        let field = 0x107c;
        let value = 0xdead_beef_feed_face_u64;
        let mut regions = vec![
            MemRegion::new(0x1000, vec![0u8; 0x100]).unwrap(),
            MemRegion::new(0x1080, vec![0u8; 8]).unwrap(),
        ];
        let all: Arc<[Patch]> =
            vec![Patch::new(field, value, 8, true).expect("an 8 byte field")].into();
        for region in &mut regions {
            region.attach_patches(&all);
        }

        let table = crate::MemRegionsLookupTable::new(regions);
        let mut tail = [0u8; 4];
        table
            .read_exact(0x1080, &mut tail)
            .expect("the inner region");
        assert_eq!(tail, value.to_le_bytes()[4..]);
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
