//! Relocation application, reached through `OwnedElf::regions(.., relocate)`,
//! against `fixtures/out/<arch>/elf_relocs.elf`, a `-shared -fPIC` fixture
//! whose `dispatch_table` has one relocation per slot pointing at
//! `helper_a..helper_d`. Unapplied the slots read zero; applied they read the
//! helper addresses.

use object::{Object, ObjectSymbol};
use std::path::PathBuf;

#[path = "common/mod.rs"]
mod common;

fn read_u32_be(regions: &[strider_reader::MemRegion], addr: u64) -> Option<u32> {
    for r in regions {
        let mut bytes = [0u8; 4];
        if r.read(addr, &mut bytes) == Some(4) {
            return Some(u32::from_be_bytes(bytes));
        }
    }
    None
}

#[test]
fn a_defined_mips_rel32_writes_the_symbol_value() {
    // A defined-symbol REL32 is `S + A`, not addend-only; the addend-only
    // reduction holds only for the undefined / index-0 (STN_UNDEF) case. The
    // fixture uses a `REL` (implicit-addend) section so A = 0, isolating the
    // symbol-value contribution.
    let fx = common::elf_fixture::build_mips32be_rel32_elf();

    // Include writable sections so `.data.rel.ro` has a region to patch.
    let regions = common::relocated(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);

    assert_eq!(
        read_u32_be(&regions, fx.slot_addr),
        Some(fx.sym_addr as u32),
        "defined-symbol REL32 must write S + A (= sym_addr), not addend-only (0)"
    );
}

#[test]
fn an_undefined_mips_rel32_stays_addend_only() {
    // The STN_UNDEF case stays addend-only: `S = 0` and the `REL` addend is 0,
    // so the slot reads 0. It is `classify`'s image-relative row, whose
    // `SHT_REL` field already holds the whole value and is served as is.
    let fx = common::elf_fixture::build_mips32be_rel32_elf_with(false);

    let regions = common::relocated(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);

    assert_eq!(
        read_u32_be(&regions, fx.slot_addr),
        Some(0),
        "undefined-symbol REL32 stays addend-only (0)"
    );
}

/// `SHT_REL` stores the addend A in the relocation field itself and `object`
/// reports `r_addend = 0` for it, so an applier that trusts `r_addend` writes
/// `S` and erases A. Every i386 / ARM32 / MIPS32 binary uses `SHT_REL` by
/// default.
#[test]
fn absolute_rel_keeps_the_implicit_in_field_addend() {
    let addend: u32 = 0x2c;
    let fx = common::elf_fixture::build_rel_elf(common::elf_fixture::RelOpts {
        endian: object::Endianness::Little,
        is_64: false,
        e_machine: object::elf::EM_386,
        r_type: object::elf::R_386_32,
        defined_symbol: true,
        slot_init: addend.to_le_bytes().to_vec(),
    });

    let regions = common::relocated(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);

    assert_eq!(
        read_u32_le_at(&regions, fx.slot_addr),
        Some(fx.sym_addr as u32 + addend),
        "R_386_32 in a REL table must write S + A, A being the field's own bytes"
    );
}

/// `SHT_CREL` without the header's addend bit is the `SHT_REL` shape: A is in
/// the field.
#[test]
fn crel_without_addends_keeps_the_implicit_in_field_addend() {
    let addend: u32 = 0x2c;
    let fx = common::elf_fixture::build_rel_elf_placed(
        common::elf_fixture::RelOpts {
            endian: object::Endianness::Little,
            is_64: false,
            e_machine: object::elf::EM_386,
            r_type: object::elf::R_386_32,
            defined_symbol: true,
            slot_init: addend.to_le_bytes().to_vec(),
        },
        common::elf_fixture::RelPlacement {
            table: common::elf_fixture::RelTable::Crel(None),
            ..Default::default()
        },
    );

    let regions = common::relocated(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);

    assert_eq!(
        read_u32_le_at(&regions, fx.slot_addr),
        Some(fx.sym_addr as u32 + addend),
    );
}

/// `SHT_CREL` with the addend bit carries A in the entry, and the field's
/// bytes are not an addend.
#[test]
fn crel_with_addends_writes_the_entry_addend_over_the_field() {
    let fx = common::elf_fixture::build_rel_elf_placed(
        common::elf_fixture::RelOpts {
            endian: object::Endianness::Little,
            is_64: true,
            e_machine: object::elf::EM_X86_64,
            r_type: object::elf::R_X86_64_64,
            defined_symbol: true,
            slot_init: 0x2cu64.to_le_bytes().to_vec(),
        },
        common::elf_fixture::RelPlacement {
            table: common::elf_fixture::RelTable::Crel(Some(-8)),
            ..Default::default()
        },
    );

    let regions = common::relocated(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);

    let mut got = [0u8; 8];
    let region = regions
        .iter()
        .find(|r| r.start_addr() <= fx.slot_addr && fx.slot_addr + 8 <= r.end_addr())
        .expect("slot region");
    assert_eq!(region.read(fx.slot_addr, &mut got), Some(8));
    assert_eq!(u64::from_le_bytes(got), fx.sym_addr.wrapping_sub(8));
}

#[test]
fn relative_rel_keeps_the_implicit_in_field_addend() {
    // `call rel32` sites carry A = -4 in the field, so a dropped A shifts every
    // resolved call target by four bytes.
    let addend: i32 = -4;
    let fx = common::elf_fixture::build_rel_elf(common::elf_fixture::RelOpts {
        endian: object::Endianness::Little,
        is_64: false,
        e_machine: object::elf::EM_386,
        r_type: object::elf::R_386_PC32,
        defined_symbol: true,
        slot_init: addend.to_le_bytes().to_vec(),
    });

    let regions = common::relocated(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);

    let expected = fx
        .sym_addr
        .wrapping_add(addend as u64)
        .wrapping_sub(fx.slot_addr) as u32;
    assert_eq!(
        read_u32_le_at(&regions, fx.slot_addr),
        Some(expected),
        "R_386_PC32 in a REL table must write S + A - P with A from the field"
    );
}

#[test]
fn defined_mips_rel32_keeps_the_implicit_in_field_addend() {
    let addend: u32 = 0x18;
    let fx = common::elf_fixture::build_rel_elf(common::elf_fixture::RelOpts {
        endian: object::Endianness::Big,
        is_64: false,
        e_machine: object::elf::EM_MIPS,
        r_type: object::elf::R_MIPS_REL32,
        defined_symbol: true,
        slot_init: addend.to_be_bytes().to_vec(),
    });

    let regions = common::relocated(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);

    assert_eq!(
        read_u32_be(&regions, fx.slot_addr),
        Some(fx.sym_addr as u32 + addend),
        "defined-symbol REL32 must write S + A with A from the field"
    );
}

/// `R_MIPS_16`'s storage unit is the 32-bit word at `r_offset` and its field is
/// that word's LOW half, so a big-endian target holds both the implicit addend
/// and the patch two bytes in. `mips-linux-gnu-as` sites two `R_MIPS_16`s over
/// `.data` bytes `aaaa 0000 0000 bbbb` at offsets 6 and 8; against a `target`
/// at `0x200c`, `mips-linux-gnu-ld` produces `aaaa 0000 200c dbc7`, i.e. each
/// field written at `r_offset + 2`. A two-byte write at `r_offset` hits the
/// high half instead, leaving the real field at its file-initial bytes.
#[test]
fn mips_r16_addend_and_patch_sit_in_the_low_half_of_the_storage_word() {
    for (name, endian, slot_init, expect) in [
        (
            "mips32be",
            object::Endianness::Big,
            vec![0x00, 0x00, 0xab, 0xcd],
            [0x00, 0x00, 0xbb, 0xcd],
        ),
        (
            "mips32le",
            object::Endianness::Little,
            vec![0xcd, 0xab, 0x00, 0x00],
            [0xcd, 0xbb, 0x00, 0x00],
        ),
    ] {
        let fx = common::elf_fixture::build_rel_elf(common::elf_fixture::RelOpts {
            endian,
            is_64: false,
            e_machine: object::elf::EM_MIPS,
            r_type: object::elf::R_MIPS_16,
            defined_symbol: true,
            slot_init,
        });
        let regions = common::relocated(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);

        // S = 0x1000, A = sign-extended 0xabcd, truncated to the 16-bit field.
        let mut got = [0u8; 4];
        assert!(
            regions
                .iter()
                .any(|r| r.read(fx.slot_addr, &mut got) == Some(4)),
            "{name}: the slot must be mapped"
        );
        assert_eq!(got, expect, "{name}: R_MIPS_16 patched the wrong halfword");
    }
}

/// `object` reports `Elf64_Rel::r_type` as the whole low 32 bits of MIPS64's
/// packed `r_info`, so a bare `r_type == R_MIPS_REL32` never matches and the
/// composite relocation real linkers emit is silently dropped.
#[test]
fn mips64_composite_rel32_writes_an_eight_byte_field() {
    let fx = common::elf_fixture::build_rel_elf(common::elf_fixture::RelOpts {
        endian: object::Endianness::Big,
        is_64: true,
        e_machine: object::elf::EM_MIPS,
        // r_type2 = R_MIPS_64, r_type = R_MIPS_REL32: the pair
        // `mips64-linux-gnuabi64-ld` emits for a 64-bit pointer slot.
        r_type: (object::elf::R_MIPS_64 << 8) | object::elf::R_MIPS_REL32,
        defined_symbol: true,
        slot_init: vec![0u8; 8],
    });

    let regions = common::relocated(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);

    assert_eq!(
        read_u64_be(&regions, fx.slot_addr),
        Some(fx.sym_addr),
        "R_MIPS_REL32 composed with R_MIPS_64 is one 64-bit field"
    );
}

/// The width comes from `r_type2`, not from the arch: an uncomposed
/// `R_MIPS_REL32` is 32 bits even on MIPS64.
#[test]
fn mips64_uncomposed_rel32_leaves_the_trailing_four_bytes_alone() {
    let mut slot_init = vec![0u8; 4];
    slot_init.extend_from_slice(&[0xAA; 4]);
    let fx = common::elf_fixture::build_rel_elf(common::elf_fixture::RelOpts {
        endian: object::Endianness::Big,
        is_64: true,
        e_machine: object::elf::EM_MIPS,
        r_type: object::elf::R_MIPS_REL32,
        defined_symbol: true,
        slot_init,
    });

    let regions = common::relocated(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);

    assert_eq!(
        read_u32_be(&regions, fx.slot_addr),
        Some(fx.sym_addr as u32)
    );
    assert_eq!(
        read_u32_be(&regions, fx.slot_addr + 4),
        Some(0xAAAA_AAAA),
        "a 32-bit field must not spill into the following word"
    );
}

/// Every ET_REL `sh_addr` is 0, so `.data` and `.text.f` collide at VMA 0 and
/// the layout rebases them apart. The code-and-readonly load takes only
/// `.text.f`. `.rela.data`, whose owner it did not load, must not be applied
/// through the one region present, which would replace the function body.
#[test]
fn et_rel_relocations_do_not_land_in_a_colliding_sections_bytes() {
    let fx = common::elf_fixture::build_et_rel_vma_collision_elf();

    let regions = common::relocated(&fx.bytes, strider_reader::elf::LoadFilter::CodeAndReadOnly);
    assert_eq!(
        regions.iter().map(common::region_bytes).collect::<Vec<_>>(),
        vec![fx.text_bytes.clone()],
        "fixture geometry: the code-and-readonly load materialises `.text.f`",
    );

    assert_eq!(
        common::region_bytes(&regions[0]),
        fx.text_bytes,
        "`.rela.data` must not be applied through the region `.text.f` owns"
    );
}

/// Both colliding sections materialise, each at its own base: `.data` keeps
/// VMA 0 and `.text.f` follows it. `.rela.data` must land in `.data` and leave
/// `.text.f` alone.
#[test]
fn et_rel_relocations_apply_to_each_rebased_section() {
    let fx = common::elf_fixture::build_et_rel_vma_collision_elf();

    let data_base = common::section_base(&fx.bytes, ".data");
    let text_base = common::section_base(&fx.bytes, ".text.f");
    let filter = strider_reader::elf::LoadFilter::AllAllocatable;
    assert_eq!(
        common::regions(&fx.bytes, filter)
            .iter()
            .map(|r| (r.start_addr(), common::region_bytes(r)))
            .collect::<Vec<_>>(),
        vec![
            (data_base, fx.data_bytes.clone()),
            (text_base, fx.text_bytes.clone()),
        ],
        "fixture geometry: `.data` (index 1) is seated first, `.text.f` follows it",
    );

    let regions = common::relocated(&fx.bytes, filter);
    assert_eq!(read_u64_le(&regions, data_base), Some(fx.sym_value));
    assert_eq!(
        common::region_bytes(&regions[1]),
        fx.text_bytes,
        "`.rela.data` must not reach `.text.f`"
    );
}

#[test]
fn a_slot_at_the_very_end_of_a_region_is_patched() {
    // The slot segment is exactly the 4-byte site, so the patch's last byte is
    // the region's last byte, where an off-by-one would reject or overrun. The
    // geometry is asserted first so a future fixture reshuffle that pads the
    // segment cannot silently demote this to an interior patch.
    let fx = common::elf_fixture::build_mips32be_rel32_elf();

    let regions = common::relocated(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);
    {
        let slot_region = regions
            .iter()
            .find(|r| r.contains(fx.slot_addr))
            .expect("slot must be mapped");
        assert_eq!(
            slot_region.end_addr(),
            fx.slot_addr + 4,
            "fixture geometry: the 4-byte slot must end exactly at its region's end",
        );
    }

    assert_eq!(
        read_u32_be(&regions, fx.slot_addr),
        Some(fx.sym_addr as u32),
        "patch touching the region's final bytes must apply cleanly",
    );
}

#[test]
fn a_field_straddling_a_section_end_is_not_patched() {
    // `.data.rel.ro` is 6 bytes with the 4-byte PC32 site at offset 4, so the
    // field `[4, 8)` runs past its file-backed bytes. The site's first byte
    // lands inside the region but the full field straddles its end, so the
    // patch cannot land and the slot stays zeroed.
    let fx =
        common::elf_fixture::build_x86_64_pc32_rela_elf(/* slot_len */ 6, /* off */ 4, 0);

    let regions = common::relocated(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);

    let region = regions
        .iter()
        .find(|r| r.contains(fx.site_addr))
        .expect("the section covering the site's first byte must be loaded");
    let mut got = [0u8; 1];
    assert_eq!(region.read(fx.site_addr, &mut got), Some(1));
    assert_eq!(
        got[0], 0,
        "a field straddling the region's end must NOT be patched"
    );
}

#[test]
fn a_negative_addend_pc_relative_site_wraps() {
    // A PC32 (`S + A - P`) with a negative addend. The applier bitcasts the
    // i64 to u64, wrapping-adds, and truncates to the 4-byte field, giving the
    // correct modular result. Guards against a future "fix" to a checked or
    // saturating add.
    let addend: i64 = -0x40;
    let fx = common::elf_fixture::build_x86_64_pc32_rela_elf(
        /* slot_len */ 4, /* off */ 0, addend,
    );

    let regions = common::relocated(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);

    let expected = fx
        .sym_addr
        .wrapping_add(addend as u64)
        .wrapping_sub(fx.site_addr) as u32;
    let got = read_u32_le_at(&regions, fx.site_addr).expect("site must be mapped");
    assert_eq!(
        got, expected,
        "negative-addend PC32 must write the modular (S + A - P) low 32 bits"
    );
}

fn read_u64_be(regions: &[strider_reader::MemRegion], addr: u64) -> Option<u64> {
    for r in regions {
        let mut bytes = [0u8; 8];
        if r.read(addr, &mut bytes) == Some(8) {
            return Some(u64::from_be_bytes(bytes));
        }
    }
    None
}

fn read_u32_le_at(regions: &[strider_reader::MemRegion], addr: u64) -> Option<u32> {
    for r in regions {
        let mut bytes = [0u8; 4];
        if r.read(addr, &mut bytes) == Some(4) {
            return Some(u32::from_le_bytes(bytes));
        }
    }
    None
}

fn fixture_path(arch: &str, case: &str) -> PathBuf {
    common::fixtures_out()
        .join(arch)
        .join(format!("{case}.elf"))
}

fn read_u64_le(regions: &[strider_reader::MemRegion], addr: u64) -> Option<u64> {
    for r in regions {
        let mut bytes = [0u8; 8];
        if r.read(addr, &mut bytes) == Some(8) {
            return Some(u64::from_le_bytes(bytes));
        }
    }
    None
}

fn sym_addr(obj: &object::File<'_>, name: &str) -> u64 {
    obj.symbol_by_name(name)
        .unwrap_or_else(|| panic!("symbol {name:?} not found"))
        .address()
}

#[test]
fn a_dispatch_table_is_patched_on_x86_64() {
    let path = fixture_path("x64", "elf_relocs");
    assert!(
        path.exists(),
        "{} is not built; run `make -C fixtures`",
        path.display()
    );
    let owned = strider_reader::load_elf(&path).expect("load_elf");
    let obj = owned.checked_file().expect("the mapped file is unchanged");
    // `dispatch_table` lives in `.data.rel.ro`, which the default
    // code-and-readonly loader skips as writable; the wider loader picks it up
    // so the applier has somewhere to patch.
    let regions = owned
        .regions(
            strider_reader::elf::RegionSource::Auto,
            strider_reader::elf::LoadFilter::AllAllocatable,
            true,
        )
        .expect("regions");

    let table_addr = sym_addr(&obj, "dispatch_table");
    let helper_a = sym_addr(&obj, "helper_a");
    let helper_b = sym_addr(&obj, "helper_b");
    let helper_c = sym_addr(&obj, "helper_c");
    let helper_d = sym_addr(&obj, "helper_d");

    // A well-formed ELF resolves cleanly, so all four slots are patched.
    assert_eq!(read_u64_le(&regions, table_addr), Some(helper_a));
    assert_eq!(read_u64_le(&regions, table_addr + 8), Some(helper_b));
    assert_eq!(read_u64_le(&regions, table_addr + 16), Some(helper_c));
    assert_eq!(read_u64_le(&regions, table_addr + 24), Some(helper_d));
}

#[test]
fn default_loader_omits_data_rel_ro() {
    // The default filter excludes `.data.rel.ro` as writable, so the table is
    // unmapped without an explicit relocation pass.
    let path = fixture_path("x64", "elf_relocs");
    assert!(
        path.exists(),
        "{} is not built; run `make -C fixtures`",
        path.display()
    );
    let obj = strider_reader::load_elf(&path).expect("load_elf");
    let obj = obj.checked_file().expect("the mapped file is unchanged");
    let regions = common::regions_of_file(
        &path,
        strider_reader::elf::LoadFilter::CodeAndReadOnly,
        false,
    );
    let table_addr = sym_addr(&obj, "dispatch_table");
    assert!(
        read_u64_le(&regions, table_addr).is_none(),
        "default loader must NOT cover `.data.rel.ro`; apply_relocations=True is required"
    );
}

#[test]
fn relocating_a_pre_resolved_binary_changes_nothing() {
    // `control.elf` is a dynamically-linked ET_EXEC whose only two dynamic
    // relocations (`R_386_GLOB_DAT`, `R_386_JUMP_SLOT`) name undefined externs
    // and sit in the writable `.got` the fetch image does not map. Nothing to
    // apply, which is why relocation processing is opt-in.
    let path = fixture_path("x86", "control");
    assert!(
        path.exists(),
        "{} is not built; run `make -C fixtures`",
        path.display()
    );
    let filter = strider_reader::elf::LoadFilter::CodeAndReadOnly;
    let before: Vec<Vec<u8>> = common::regions_of_file(&path, filter, false)
        .iter()
        .map(common::region_bytes)
        .collect();
    let after: Vec<Vec<u8>> = common::regions_of_file(&path, filter, true)
        .iter()
        .map(common::region_bytes)
        .collect();
    assert_eq!(
        before, after,
        "ET_EXEC pre-link-resolved binary must have nothing to apply"
    );
}
/// The colliding sections hold the SAME bytes, which a zero-initialised or
/// same-length pair does routinely. Nothing about the region's contents can
/// then say which section the load accepted, so ownership has to come from the
/// load itself.
#[test]
fn et_rel_relocations_apply_when_the_colliding_sections_are_byte_identical() {
    let fx = common::elf_fixture::build_et_rel_vma_collision_elf_with(vec![0u8; 8]);
    assert_eq!(fx.text_bytes, fx.data_bytes, "fixture geometry: bytes tie");

    let regions = common::relocated(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);

    assert_eq!(
        read_u64_le(&regions, common::section_base(&fx.bytes, ".data")),
        Some(fx.sym_value),
        "`.data` won the all-allocatable load, so its relocation must apply"
    );
}

/// The same with byte-identical section contents, where a bug siting
/// `.rela.data` through `.text.f`'s region would be invisible in the bytes
/// unless the relocation actually changes them.
#[test]
fn et_rel_byte_identical_collision_does_not_patch_the_other_section() {
    let fx = common::elf_fixture::build_et_rel_vma_collision_elf_with(vec![0u8; 8]);

    let regions = common::relocated(&fx.bytes, strider_reader::elf::LoadFilter::CodeAndReadOnly);

    assert_eq!(
        common::region_bytes(&regions[0]),
        fx.text_bytes,
        "`.rela.data` must not be applied through the region `.text.f` owns"
    );
}

/// Pins the ET_REL geometry the ownership rules are built on: pre-link every
/// `sh_addr` is 0, so the layout gives each allocatable section a synthetic
/// base and ALL of them stay reachable. Which ones a load materialises is then
/// purely the filter's choice, never a collision's.
#[test]
fn et_rel_sections_colliding_at_vma_zero_get_bases_of_their_own() {
    let fx = common::elf_fixture::build_et_rel_vma_collision_elf();
    let data_base = common::section_base(&fx.bytes, ".data");
    let text_base = common::section_base(&fx.bytes, ".text.f");
    assert_eq!(
        text_base,
        data_base + fx.data_bytes.len() as u64,
        "fixture geometry: `.text.f` is placed just past `.data`"
    );

    // `.data` is section index 1 and `.text.f` index 2, so `.data` is seated
    // first and `.text.f` is placed just past it.
    for (name, regions, expected) in [
        (
            "code-and-readonly",
            common::regions(&fx.bytes, strider_reader::elf::LoadFilter::CodeAndReadOnly),
            vec![(text_base, fx.text_bytes.clone())],
        ),
        (
            "all-allocatable",
            common::regions(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable),
            vec![
                (data_base, fx.data_bytes.clone()),
                (text_base, fx.text_bytes.clone()),
            ],
        ),
    ] {
        assert_eq!(
            regions
                .iter()
                .map(|r| (r.start_addr(), common::region_bytes(r)))
                .collect::<Vec<_>>(),
            expected,
            "{name}: every accepted section must be reachable at its own base",
        );
    }
}

/// A `SHN_COMMON` symbol stores its ALIGNMENT in `st_value`; the address only
/// exists once the link allocates it in `.bss`. It resolves to room of its own
/// past the image, never to the alignment as an address, nor to the site's
/// file-initial zero.
#[test]
fn a_common_symbol_resolves_past_the_image_not_to_its_alignment() {
    use object::{Object as _, ObjectSymbol as _};
    let fx = common::elf_fixture::build_et_rel_vma_collision_elf_full(
        vec![0u8; 8],
        object::elf::SHN_COMMON,
        4, // the alignment, not an address
    );
    let obj = object::File::parse(&fx.bytes[..]).expect("parse");
    let common_sym = obj.symbols().find(|s| s.is_common()).expect("the common");
    let expected = strider_reader::elf::ElfSectionLayout::new(&obj)
        .extern_address(common_sym.index().0)
        .expect("a common symbol gets an address");
    let site = common::section_base(&fx.bytes, ".data");
    let regions = common::load_with_relocations(&fx.bytes);
    let table = strider_reader::MemRegionsLookupTable::new(regions);
    let mut got = [0u8; 8];
    table
        .read_exact(site, &mut got)
        .expect("read the relocated site");
    assert_eq!(u64::from_le_bytes(got), expected);
    assert!(
        table.read(expected, &mut [0u8; 1]).is_none(),
        "the common's address maps nothing"
    );
}

/// An `st_shndx` past the section table names no section, so an ET_REL symbol
/// carrying one has no base to be an offset from and no address at all.
/// Resolving it against a zero base seats the raw `st_value` as if it were
/// absolute, patching a wild low address into the site.
#[test]
fn an_out_of_range_shndx_relocation_is_skipped_not_applied() {
    let fx = common::elf_fixture::build_et_rel_vma_collision_elf_full(
        vec![0u8; 8],
        999, // past the last section header
        0x40,
    );
    let site = common::section_base(&fx.bytes, ".data");
    let regions = common::load_with_relocations(&fx.bytes);
    let table = strider_reader::MemRegionsLookupTable::new(regions);
    let mut got = [0u8; 8];
    table
        .read_exact(site, &mut got)
        .expect("read the relocated site");
    assert_eq!(got, [0u8; 8], "no section index, so no address to patch");
}

/// mips64el emits its dynamic relocations as `SHT_REL`, whose `r_info` `object`
/// reads as one little-endian `u64`, transposing MIPS64's `r_sym` word against
/// its four type bytes. Both endiannesses must resolve the same symbol.
#[test]
fn mips64_rel32_resolves_in_both_endiannesses() {
    for (name, endian) in [
        ("mips64be", object::Endianness::Big),
        ("mips64el", object::Endianness::Little),
    ] {
        let fx = common::elf_fixture::build_mips64_rel32_elf(endian);
        let regions = common::load_with_relocations(&fx.bytes);
        let table = strider_reader::MemRegionsLookupTable::new(regions);
        let mut got = [0u8; 8];
        table.read_exact(fx.slot_addr, &mut got).expect("read slot");
        let value = if matches!(endian, object::Endianness::Big) {
            u64::from_be_bytes(got)
        } else {
            u64::from_le_bytes(got)
        };
        assert_eq!(
            value, fx.sym_addr,
            "{name}: R_MIPS_REL32 must resolve to the symbol address",
        );
    }
}

/// mips64el's transposed `r_info` means the `kind` / `size` `object` reports
/// were derived from the real `r_sym`. Dispatch is on the raw `r_type` alone,
/// so a symbol index colliding with `R_MIPS_16` / `R_MIPS_32` / `R_MIPS_64`
/// must not turn an unhandled relocation type into an absolute patch.
#[test]
fn mips64el_ignores_a_relocation_kind_read_from_the_symbol_index() {
    let fx = common::elf_fixture::build_mips64el_transposed_kind_elf();
    let regions = common::load_with_relocations(&fx.bytes);
    let table = strider_reader::MemRegionsLookupTable::new(regions);
    let mut got = [0u8; 8];
    table.read_exact(fx.slot_addr, &mut got).expect("read slot");
    assert_eq!(
        got, [0u8; 8],
        "R_MIPS_GPREL32 is unhandled; the site must keep its file-initial bytes"
    );
}

/// Relocations are a patch list applied to whatever part of a read they cover,
/// so reads of different widths and alignments over one site must agree.
#[test]
fn a_read_straddling_a_relocated_site_serves_the_patched_bytes() {
    let path = fixture_path("x64", "elf_relocs");
    assert!(
        path.exists(),
        "{} is not built; run `make -C fixtures`",
        path.display()
    );
    let owned = strider_reader::load_elf(&path).expect("load_elf");
    let obj = owned.checked_file().expect("the mapped file is unchanged");
    let table = strider_reader::MemRegionsLookupTable::new(
        owned
            .regions(
                strider_reader::elf::RegionSource::Auto,
                strider_reader::elf::LoadFilter::AllAllocatable,
                true,
            )
            .expect("regions"),
    );
    let unpatched = strider_reader::MemRegionsLookupTable::new(
        owned
            .regions(
                strider_reader::elf::RegionSource::Auto,
                strider_reader::elf::LoadFilter::AllAllocatable,
                false,
            )
            .expect("load"),
    );
    let (mut sites, mut differ) = (0usize, 0usize);
    for (site, _) in obj.dynamic_relocations().into_iter().flatten() {
        let mut wide = [0u8; 24];
        let base = site.saturating_sub(8);
        if table.read_exact(base, &mut wide).is_err() {
            continue;
        }
        sites += 1;
        for off in 0..16usize {
            let mut narrow = [0u8; 8];
            table
                .read_exact(base + off as u64, &mut narrow)
                .expect("inside the wide read");
            assert_eq!(
                &wide[off..off + 8],
                &narrow[..],
                "site {site:#x}: the 8 bytes at +{off} differ between a wide and a narrow read"
            );
        }
        let mut raw = [0u8; 24];
        unpatched.read_exact(base, &mut raw).expect("same range");
        differ += usize::from(raw != wide);
    }
    assert!(sites > 0, "fixture must carry dynamic relocations");
    assert!(differ > 0, "no site was patched at all");
}

/// `R_PPC_REL32` is what a PowerPC `.rodata` switch table is built out of, and
/// `object` surfaces it as `Unknown` with `size = 0`, so it needs the raw
/// `r_type` dispatch. Unpatched the table reads as eight zeros.
#[test]
fn ppc_rel32_patches_a_rodata_jump_table() {
    let path = common::fixtures_out().join("ppc32be/switch_masked_loop.o");
    let filter = strider_reader::elf::LoadFilter::CodeAndReadOnly;
    let regions = common::regions_of_file(&path, filter, true);
    let owned = strider_reader::load_elf(&path).expect("load_elf");
    let obj = owned.checked_file().expect("the mapped file is unchanged");

    let layout = strider_reader::elf::ElfSectionLayout::new(&obj);
    let base = |name: &str| {
        use object::Object as _;
        layout.section_base(&obj.section_by_name(name).expect(name))
    };
    let (text, rodata) = (base(".text"), base(".rodata"));

    // `S + A - P` for entry 0 is (.text + 0x90) - .rodata = -0x140.
    let raw = read_u32_be(&regions, rodata).expect("the rebased .rodata table");
    let first = raw as i32;
    assert_eq!(first, -0x140, "table[0] must be patched, not left at 0");
    assert_eq!(
        rodata as i64 + i64::from(first),
        text as i64 + 0x90,
        "resolves to .text + 0x90"
    );
}

/// A relocation whose field runs past the end of the section owning it must be
/// refused, not sited into whatever section follows.
///
/// `reloc.size()` is 0 for every type dispatched on the raw `r_type` (GOT/PLT
/// slots, `R_MIPS_REL32`, the PPC `REL32`/`REL64` pair, image-relative), so a
/// bounds check written against it is inert for exactly those. Here `.data` is
/// eight bytes and the entry sites an eight-byte `R_X86_64_JUMP_SLOT` at
/// `.data + 8`, the rebased start of `.text.f`, whose body would take the
/// write.
#[test]
fn reloc_field_past_its_own_section_does_not_patch_the_next_one() {
    let text = vec![0x90u8; 8];
    let fx = common::elf_fixture::build_et_rel_vma_collision_elf_sited(
        text.clone(),
        object::elf::SHN_ABS,
        0xdead_beef,
        8,
        object::elf::R_X86_64_JUMP_SLOT,
    );
    let regions = common::load_with_relocations(&fx.bytes);
    let table = strider_reader::MemRegionsLookupTable::new(regions);
    let mut got = [0u8; 8];
    table
        .read_exact(common::section_base(&fx.bytes, ".text.f"), &mut got)
        .expect("read `.text.f`, rebased past `.data`");
    assert_eq!(
        got[..],
        text[..],
        "an out-of-section relocation patched the next section's bytes"
    );
}

/// A byte-multiple `reloc.size()` does not make the field plain low bytes at
/// the offset. s390x's `R_390_PC32DBL` stores `(S + A - P) >> 1` in a 4-byte
/// field; patching it as a whole word writes a displacement twice the real one.
#[test]
fn a_non_generic_encoding_is_not_patched_as_a_plain_word() {
    let fx = common::elf_fixture::build_rel_elf(common::elf_fixture::RelOpts {
        endian: object::Endianness::Big,
        is_64: true,
        e_machine: object::elf::EM_S390,
        r_type: object::elf::R_390_PC32DBL,
        defined_symbol: true,
        slot_init: vec![0u8; 4],
    });
    let regions = common::load_with_relocations(&fx.bytes);
    let table = strider_reader::MemRegionsLookupTable::new(regions);
    let mut got = [0u8; 4];
    table.read_exact(fx.slot_addr, &mut got).expect("read slot");
    assert_eq!(
        got, [0u8; 4],
        "a halved-displacement field must keep its file-initial bytes"
    );
}

/// `r_offset` is attacker-controlled, and the MIPS half-field skew adds to it.
/// At the top of the address space that sum overflows, which is a debug-build
/// panic on a plain `+`.
#[test]
fn a_mips_half_field_at_the_top_of_the_address_space_is_skipped() {
    let fx = common::elf_fixture::build_rel_elf_placed(
        common::elf_fixture::RelOpts {
            endian: object::Endianness::Big,
            is_64: true,
            e_machine: object::elf::EM_MIPS,
            r_type: object::elf::R_MIPS_16,
            defined_symbol: true,
            slot_init: vec![0u8; 1],
        },
        common::elf_fixture::RelPlacement {
            slot_addr: u64::MAX - 1,
            ..Default::default()
        },
    );
    let regions = common::load_with_relocations(&fx.bytes);
    let table = strider_reader::MemRegionsLookupTable::new(regions);
    let mut got = [0u8; 1];
    table.read_exact(fx.slot_addr, &mut got).expect("read slot");
    assert_eq!(got, [0u8; 1], "the site must keep its file-initial bytes");
}

/// A read is served by whichever region covers the REQUEST, so a patch filed
/// only on the region that covers the FIELD leaves a wider read unpatched. The
/// outer mapping here spans the slot, and an 8-byte read falls through to it.
#[test]
fn every_region_covering_a_site_serves_the_patched_bytes() {
    let fx = common::elf_fixture::build_rel_elf_placed(
        common::elf_fixture::RelOpts {
            endian: object::Endianness::Little,
            is_64: false,
            e_machine: object::elf::EM_386,
            r_type: object::elf::R_386_32,
            defined_symbol: true,
            slot_init: vec![0u8; 4],
        },
        common::elf_fixture::RelPlacement {
            outer_load: true,
            ..Default::default()
        },
    );
    let table =
        strider_reader::MemRegionsLookupTable::new(common::load_with_relocations(&fx.bytes));

    let mut narrow = [0u8; 4];
    table
        .read_exact(fx.slot_addr, &mut narrow)
        .expect("read the site");
    assert_eq!(u32::from_le_bytes(narrow), fx.sym_addr as u32);

    let mut wide = [0u8; 8];
    table
        .read_exact(fx.slot_addr - 4, &mut wide)
        .expect("read across the site");
    assert_eq!(
        &wide[4..],
        &narrow[..],
        "a read served by the outer mapping must see the same relocated bytes"
    );
}

/// A site every one of 64 overlapping mappings covers is served patched by
/// each of them, from one shared patch list, so a crafted overlap does not
/// multiply the memory the patches take.
#[test]
fn every_one_of_many_overlapping_mappings_serves_a_sites_patch() {
    let bytes = common::elf_fixture::build_overlapping_loads_rela_elf(64, 512);
    let regions = strider_reader::OwnedElf::parse(bytes)
        .expect("parse")
        .regions(
            strider_reader::elf::RegionSource::Auto,
            strider_reader::elf::LoadFilter::AllAllocatable,
            true,
        )
        .expect("overlapping mappings load relocated");
    assert_eq!(regions.len(), 64);
    for region in &regions {
        for site in [0x2000u64, 0x2000 + 8 * 511] {
            let mut field = [0u8; 8];
            assert_eq!(
                region.read(site, &mut field),
                Some(8),
                "{region:?} at {site:#x}"
            );
            assert_eq!(u64::from_le_bytes(field), 0x1000, "{region:?} at {site:#x}");
        }
    }
}

/// An `SHT_REL` `R_386_RELATIVE` keeps `image_base + A` in its own field, so
/// with the base modelled as 0 the bytes are already right and no hole is
/// recorded over them: a read-only `.rodata` table whose every word is correct
/// stays served.
#[test]
fn an_implicit_addend_relative_in_a_read_only_mapping_serves_its_field() {
    let slot_init = vec![0x88, 0x11, 0x00, 0x00];
    let fx = common::elf_fixture::build_rel_elf_placed(
        common::elf_fixture::RelOpts {
            endian: object::Endianness::Little,
            is_64: false,
            e_machine: object::elf::EM_386,
            r_type: object::elf::R_386_RELATIVE,
            defined_symbol: false,
            slot_init: slot_init.clone(),
        },
        common::elf_fixture::RelPlacement {
            slot_exec: true,
            ..Default::default()
        },
    );

    for filter in [
        strider_reader::elf::LoadFilter::AllAllocatable,
        strider_reader::elf::LoadFilter::ImmutableOnly,
    ] {
        let table =
            strider_reader::MemRegionsLookupTable::new(common::relocated(&fx.bytes, filter));
        let mut got = [0u8; 4];
        table
            .read_exact(fx.slot_addr, &mut got)
            .expect("a read-only RELATIVE site is not a hole");
        assert_eq!(got.to_vec(), slot_init);
    }
}

/// `R_PPC64_REL32` is `S + A - P` in a 4-byte field. A displacement past
/// +-2 GiB does not fit it, so the site is a hole rather than a truncated word;
/// one that fits is still patched.
#[test]
fn a_word_sized_rel32_that_does_not_fit_is_a_hole() {
    let fixture = |slot_addr: u64| {
        common::elf_fixture::build_rel_elf_placed(
            common::elf_fixture::RelOpts {
                endian: object::Endianness::Big,
                is_64: true,
                e_machine: object::elf::EM_PPC64,
                r_type: object::elf::R_PPC64_REL32,
                defined_symbol: true,
                slot_init: vec![0u8; 4],
            },
            // Read-only, so an uncomputed field is a hole and not skipped.
            common::elf_fixture::RelPlacement {
                slot_addr,
                slot_exec: true,
                ..Default::default()
            },
        )
    };

    let near = fixture(0x2000);
    let regions = common::load_with_relocations(&near.bytes);
    assert_eq!(
        read_u32_be(&regions, near.slot_addr),
        Some(near.sym_addr.wrapping_sub(near.slot_addr) as u32),
        "a displacement that fits is patched"
    );

    let far = fixture(0x1_0000_2000);
    let table =
        strider_reader::MemRegionsLookupTable::new(common::load_with_relocations(&far.bytes));
    assert_eq!(
        table.unmodelled_relocation_at(far.slot_addr),
        Some(object::elf::R_PPC64_REL32),
        "a displacement past 32 signed bits must be a hole, not its low word"
    );
}

/// A mips64el `SHT_REL` entry's `r_info` is transposed in what `object`
/// reports, so the hole over an unhandled one must take its type from the
/// untransposed word: `R_MIPS_GPREL32`, not the symbol index 1.
#[test]
fn a_mips64el_rel_hole_names_the_relocation_type() {
    let fx = common::elf_fixture::build_rel_elf_placed(
        common::elf_fixture::RelOpts {
            endian: object::Endianness::Little,
            is_64: true,
            e_machine: object::elf::EM_MIPS,
            r_type: object::elf::R_MIPS_GPREL32,
            defined_symbol: true,
            slot_init: vec![0u8; 8],
        },
        // Read-only, so the unhandled relocation leaves a hole.
        common::elf_fixture::RelPlacement {
            slot_exec: true,
            ..Default::default()
        },
    );
    let table =
        strider_reader::MemRegionsLookupTable::new(common::load_with_relocations(&fx.bytes));
    assert_eq!(
        table.unmodelled_relocation_at(fx.slot_addr),
        Some(object::elf::R_MIPS_GPREL32)
    );
}

/// A sign-extended kseg address (`0xffff_ffff_8xxx_xxxx`) fits a 4-byte
/// `R_MIPS_REL32` on MIPS64: the unsigned field takes either signedness, so
/// the low word is patched rather than left a hole.
#[test]
fn a_mips64_rel32_to_a_kseg_address_is_patched() {
    let fx = common::elf_fixture::build_rel_elf_placed(
        common::elf_fixture::RelOpts {
            endian: object::Endianness::Big,
            is_64: true,
            e_machine: object::elf::EM_MIPS,
            r_type: object::elf::R_MIPS_REL32,
            defined_symbol: true,
            // A = -0x8000_0000, so S + A = 0xffff_ffff_8000_1000.
            slot_init: 0x8000_0000u32.to_be_bytes().to_vec(),
        },
        // Read-only, so a field that did not fit would be a hole.
        common::elf_fixture::RelPlacement {
            slot_exec: true,
            ..Default::default()
        },
    );
    let regions = common::load_with_relocations(&fx.bytes);
    assert_eq!(
        read_u32_be(&regions, fx.slot_addr),
        Some((fx.sym_addr as u32).wrapping_add(0x8000_0000)),
    );
    let table = strider_reader::MemRegionsLookupTable::new(regions);
    assert_eq!(table.unmodelled_relocation_at(fx.slot_addr), None);
}

/// A big-endian MIPS64 type word carries `r_ssym` in its top byte, which is
/// not part of the type: the hole names `R_MIPS_GPREL32`.
#[test]
fn a_mips64_be_hole_drops_r_ssym_from_the_type() {
    let fx = common::elf_fixture::build_rel_elf_placed(
        common::elf_fixture::RelOpts {
            endian: object::Endianness::Big,
            is_64: true,
            e_machine: object::elf::EM_MIPS,
            r_type: (1 << 24) | object::elf::R_MIPS_GPREL32,
            defined_symbol: true,
            slot_init: vec![0u8; 8],
        },
        // Read-only, so the unhandled relocation leaves a hole.
        common::elf_fixture::RelPlacement {
            slot_exec: true,
            ..Default::default()
        },
    );
    let table =
        strider_reader::MemRegionsLookupTable::new(common::load_with_relocations(&fx.bytes));
    assert_eq!(
        table.unmodelled_relocation_at(fx.slot_addr),
        Some(object::elf::R_MIPS_GPREL32)
    );
}

/// A relocation at the top of the address space: a slot ending at
/// `2^64 - 1` is patched; one whose mapping ends at `2^64` loses its last
/// byte, so no region holds the field to patch and its mapped bytes are a
/// hole.
#[test]
fn a_relocation_at_the_top_of_the_address_space_loads() {
    for (slot_addr, patched) in [(u64::MAX - 8, true), (u64::MAX - 7, false)] {
        let fx = common::elf_fixture::build_rel_elf_placed(
            common::elf_fixture::RelOpts {
                endian: object::Endianness::Little,
                is_64: true,
                e_machine: object::elf::EM_X86_64,
                r_type: object::elf::R_X86_64_GLOB_DAT,
                defined_symbol: true,
                slot_init: vec![0u8; 8],
            },
            // Read-only, so an uncomputed field would be a hole.
            common::elf_fixture::RelPlacement {
                slot_addr,
                slot_exec: true,
                ..Default::default()
            },
        );
        let table =
            strider_reader::MemRegionsLookupTable::new(common::load_with_relocations(&fx.bytes));
        assert_eq!(
            table.unmodelled_relocation_at(slot_addr),
            (!patched).then_some(object::elf::R_X86_64_GLOB_DAT),
            "{slot_addr:#x}"
        );
        if patched {
            let mut field = [0u8; 8];
            table
                .read_exact(slot_addr, &mut field)
                .expect("the slot is mapped whole");
            assert_eq!(u64::from_le_bytes(field), fx.sym_addr);
        } else {
            let mut field = [0u8; 7];
            assert!(table.read_exact(slot_addr, &mut field).is_err());
        }
    }
}

/// BFD opens a mips64el `.rel.dyn` with a null `R_MIPS_NONE` at `r_offset` 0,
/// inside the first PT_LOAD. A marker leaves its field as assembled, so the
/// ELF magic there stays served, on mips64el as on mips64be.
#[test]
fn a_null_mips64_relocation_leaves_the_image_start_served() {
    for arch in ["mips64le", "mips64be"] {
        let path = fixture_path(arch, "elf_relocs");
        assert!(
            path.exists(),
            "{} is not built; run `make -C fixtures`",
            path.display()
        );
        let table = strider_reader::MemRegionsLookupTable::new(common::regions_of_file(
            &path,
            strider_reader::elf::LoadFilter::AllAllocatable,
            true,
        ));
        let mut magic = [0u8; 4];
        assert_eq!(table.read(0, &mut magic), Some(4), "{arch}");
        assert_eq!(&magic, b"\x7fELF", "{arch}");
    }
}

/// A mips64el `SHT_REL` `R_MIPS_26` is applied like any other: the transposed
/// `r_info` is undone before the type is looked up, not by giving up on it.
#[test]
fn a_mips64el_rel_jump_is_patched() {
    let fx = common::elf_fixture::build_rel_elf_placed(
        common::elf_fixture::RelOpts {
            endian: object::Endianness::Little,
            is_64: true,
            e_machine: object::elf::EM_MIPS,
            r_type: object::elf::R_MIPS_26,
            defined_symbol: true,
            slot_init: vec![0u8; 4],
        },
        // In the fetch image, where an uncomputed field would be a hole.
        common::elf_fixture::RelPlacement {
            slot_exec: true,
            ..Default::default()
        },
    );
    let table =
        strider_reader::MemRegionsLookupTable::new(common::load_with_relocations(&fx.bytes));
    let mut word = [0u8; 4];
    table
        .read_exact(fx.slot_addr, &mut word)
        .expect("the jump is patched, not a hole");
    assert_eq!(
        u32::from_le_bytes(word),
        (fx.sym_addr >> 2) as u32,
        "`j func` encodes `func >> 2`"
    );
}

/// A mips64el `SHT_REL` composite this loader does not compute stays a hole
/// naming the whole untransposed type word: only a single type, a slot or an
/// `R_MIPS_REL32` applies.
#[test]
fn a_mips64el_rel_composite_is_a_hole_naming_its_type_word() {
    let r_type = (object::elf::R_MIPS_64 << 8) | object::elf::R_MIPS_26;
    let fx = common::elf_fixture::build_rel_elf_placed(
        common::elf_fixture::RelOpts {
            endian: object::Endianness::Little,
            is_64: true,
            e_machine: object::elf::EM_MIPS,
            r_type,
            defined_symbol: true,
            slot_init: vec![0u8; 4],
        },
        common::elf_fixture::RelPlacement {
            slot_exec: true,
            ..Default::default()
        },
    );
    let table =
        strider_reader::MemRegionsLookupTable::new(common::load_with_relocations(&fx.bytes));
    assert_eq!(table.unmodelled_relocation_at(fx.slot_addr), Some(r_type));
}

/// `object` reads an `SHT_CREL` entry's symbol and type as written, mips64el
/// included, and only an `SHT_REL` one transposed: a CREL `R_MIPS_26` with
/// its addend in the field is applied.
#[test]
fn a_mips64el_crel_jump_is_patched() {
    let fx = common::elf_fixture::build_rel_elf_placed(
        common::elf_fixture::RelOpts {
            endian: object::Endianness::Little,
            is_64: true,
            e_machine: object::elf::EM_MIPS,
            r_type: object::elf::R_MIPS_26,
            defined_symbol: true,
            slot_init: vec![0u8; 4],
        },
        common::elf_fixture::RelPlacement {
            slot_exec: true,
            table: common::elf_fixture::RelTable::Crel(None),
            ..Default::default()
        },
    );
    let table =
        strider_reader::MemRegionsLookupTable::new(common::load_with_relocations(&fx.bytes));
    let mut word = [0u8; 4];
    table
        .read_exact(fx.slot_addr, &mut word)
        .expect("the jump is patched, not a hole");
    assert_eq!(u32::from_le_bytes(word), (fx.sym_addr >> 2) as u32);
}

/// An image holding an `SHT_CREL` table reads its mips64el `SHT_REL`
/// entries as written, untransposed: the `R_MIPS_26` jump patched alone
/// reads as type 1, its `r_sym` word, against an out-of-range symbol, and is
/// a hole.
#[test]
fn a_mips64el_rel_entry_beside_a_crel_table_is_read_as_written() {
    let fx = common::elf_fixture::build_rel_elf_placed(
        common::elf_fixture::RelOpts {
            endian: object::Endianness::Little,
            is_64: true,
            e_machine: object::elf::EM_MIPS,
            r_type: object::elf::R_MIPS_26,
            defined_symbol: true,
            slot_init: vec![0u8; 4],
        },
        common::elf_fixture::RelPlacement {
            slot_exec: true,
            empty_crel: true,
            ..Default::default()
        },
    );
    let table =
        strider_reader::MemRegionsLookupTable::new(common::load_with_relocations(&fx.bytes));
    assert_eq!(
        table.unmodelled_relocation_at(fx.slot_addr),
        Some(object::elf::R_MIPS_16)
    );
}

/// `S` exists only for a symbol: a relocation whose value reads it is a hole
/// against STN_UNDEF, whichever row computes it. Each slot is read-only, so
/// an uncomputed field is a hole rather than skipped.
#[test]
fn a_value_reading_s_against_no_symbol_is_a_hole() {
    use common::elf_fixture::{RelOpts, RelPlacement, RelTable, build_rel_elf_placed};
    use object::Endianness::{Big, Little};
    use object::elf;
    for (endian, e_machine, r_type, slot_init, table) in [
        (
            Big,
            elf::EM_PPC64,
            elf::R_PPC64_REL32,
            vec![0u8; 4],
            RelTable::Rel,
        ),
        (
            Little,
            elf::EM_X86_64,
            elf::R_X86_64_GLOB_DAT,
            vec![0u8; 8],
            RelTable::Rel,
        ),
        // RELA: this loader reads no `SHT_REL` addend out of a branch.
        (
            Little,
            elf::EM_AARCH64,
            elf::R_AARCH64_CALL26,
            0x9400_0000u32.to_le_bytes().to_vec(),
            RelTable::Crel(Some(0)),
        ),
    ] {
        let fx = build_rel_elf_placed(
            RelOpts {
                endian,
                is_64: true,
                e_machine,
                r_type,
                defined_symbol: false,
                slot_init,
            },
            RelPlacement {
                slot_exec: true,
                table,
                ..Default::default()
            },
        );
        let regions = common::load_with_relocations(&fx.bytes);
        let table = strider_reader::MemRegionsLookupTable::new(regions);
        assert_eq!(
            table.unmodelled_relocation_at(fx.slot_addr),
            Some(r_type),
            "r_type {r_type}"
        );
    }
}

/// `B + A` reads no symbol, so an `R_*_RELATIVE` naming one computes its
/// addend whether that symbol resolves or not: here a weak undefined one,
/// valued 0 (unresolved) and not.
#[test]
fn an_image_relative_relocation_ignores_its_symbol() {
    let addend = 0x1234;
    for value in [0, 0x1000] {
        let fx = common::elf_fixture::build_rel_elf_placed(
            common::elf_fixture::RelOpts {
                endian: object::Endianness::Little,
                is_64: true,
                e_machine: object::elf::EM_X86_64,
                r_type: object::elf::R_X86_64_RELATIVE,
                defined_symbol: true,
                slot_init: vec![0u8; 8],
            },
            // Read-only, so an uncomputed field would be a hole.
            common::elf_fixture::RelPlacement {
                slot_exec: true,
                table: common::elf_fixture::RelTable::Crel(Some(addend)),
                undefined_weak: Some(value),
                ..Default::default()
            },
        );
        let regions = common::load_with_relocations(&fx.bytes);
        assert_eq!(
            read_u64_le(&regions, fx.slot_addr),
            Some(addend as u64),
            "st_value {value:#x}"
        );
    }
}

/// An `R_X86_64_RELATIVE` whose eight-byte field runs past its four-byte
/// mapping cannot be patched, and the four bytes mapped are a hole rather
/// than the file's.
#[test]
fn an_image_relative_word_past_its_mapping_is_a_hole() {
    let slot_init = vec![0x11, 0x22, 0x33, 0x44];
    let fx = common::elf_fixture::build_rel_elf_placed(
        common::elf_fixture::RelOpts {
            endian: object::Endianness::Little,
            is_64: true,
            e_machine: object::elf::EM_X86_64,
            r_type: object::elf::R_X86_64_RELATIVE,
            defined_symbol: false,
            slot_init: slot_init.clone(),
        },
        common::elf_fixture::RelPlacement {
            slot_exec: true,
            table: common::elf_fixture::RelTable::Crel(Some(0x1234)),
            ..Default::default()
        },
    );
    let table =
        strider_reader::MemRegionsLookupTable::new(common::load_with_relocations(&fx.bytes));
    assert!(table.read_exact(fx.slot_addr, &mut [0u8; 4]).is_err());
    assert_eq!(
        table.unmodelled_relocation_at(fx.slot_addr),
        Some(object::elf::R_X86_64_RELATIVE)
    );
}

/// An `SHT_CREL` `.rel.dyn` relinked to 0 in an image with a `.dynsym`,
/// over an executable slot, where a hole shows: `object`'s dynamic walk
/// skips it, and the walk of tables off `.dynsym` reads it as it reads a
/// REL or RELA one. A RELATIVE is B + A; an entry naming a symbol a hole.
#[test]
fn a_crel_table_off_the_dynsym_is_walked() {
    let build = |r_type: u32, defined_symbol: bool| {
        let mut fx = common::elf_fixture::build_rel_elf_placed(
            common::elf_fixture::RelOpts {
                endian: object::Endianness::Little,
                is_64: true,
                e_machine: object::elf::EM_X86_64,
                r_type,
                defined_symbol,
                slot_init: vec![0x55; 8],
            },
            common::elf_fixture::RelPlacement {
                slot_exec: true,
                table: common::elf_fixture::RelTable::Crel(Some(0x40)),
                ..Default::default()
            },
        );
        // Elf64_Ehdr.e_shoff, then Elf64_Shdr.sh_link of `.rel.dyn`
        // (section 5).
        let shoff = u64::from_le_bytes(fx.bytes[0x28..0x30].try_into().unwrap()) as usize;
        let at = shoff + 5 * 64 + 0x28;
        fx.bytes[at..at + 4].copy_from_slice(&0u32.to_le_bytes());
        let regions = common::relocated(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);
        let region = regions
            .iter()
            .find(|r| r.start_addr() <= fx.slot_addr && fx.slot_addr < r.end_addr())
            .expect("slot region")
            .clone();
        (region, fx.slot_addr)
    };
    let (region, slot) = build(object::elf::R_X86_64_RELATIVE, false);
    let mut got = [0u8; 8];
    assert_eq!(region.read(slot, &mut got), Some(8));
    assert_eq!(u64::from_le_bytes(got), 0x40, "RELATIVE");
    let (region, slot) = build(object::elf::R_X86_64_64, true);
    assert_eq!(
        region.read(slot, &mut got),
        None,
        "a symbol entry is a hole"
    );
}

/// The CREL image with its empty `.crel.empty` header (section 6) turned
/// into a decoy linked to `.dynsym`, starting at `.dynstr` and running over
/// the real `.rel.dyn` table: the decoy's own stream decodes no entry, and
/// the real table is still decoded, so the RELATIVE slot reads B + A.
#[test]
fn a_crel_header_over_another_crel_table_does_not_hide_it() {
    let mut fx = common::elf_fixture::build_rel_elf_placed(
        common::elf_fixture::RelOpts {
            endian: object::Endianness::Little,
            is_64: true,
            e_machine: object::elf::EM_X86_64,
            r_type: object::elf::R_X86_64_RELATIVE,
            defined_symbol: false,
            slot_init: vec![0x55; 8],
        },
        common::elf_fixture::RelPlacement {
            table: common::elf_fixture::RelTable::Crel(Some(0x40)),
            empty_crel: true,
            ..Default::default()
        },
    );
    let (dynstr, reldyn_end) = {
        let obj = object::File::parse(&*fx.bytes).unwrap();
        let range = |name: &str| {
            use object::ObjectSection as _;
            obj.section_by_name(name).unwrap().file_range().unwrap()
        };
        let (rel, rel_size) = range(".rel.dyn");
        (range(".dynstr").0, rel + rel_size)
    };
    // Elf64_Ehdr.e_shoff; the decoy's Elf64_Shdr sh_offset (0x18), sh_size
    // (0x20) and sh_link (0x28, `.dynsym` is section 3).
    let shoff = u64::from_le_bytes(fx.bytes[0x28..0x30].try_into().unwrap()) as usize;
    let decoy = shoff + 6 * 64;
    fx.bytes[decoy + 0x18..decoy + 0x20].copy_from_slice(&dynstr.to_le_bytes());
    fx.bytes[decoy + 0x20..decoy + 0x28].copy_from_slice(&(reldyn_end - dynstr).to_le_bytes());
    fx.bytes[decoy + 0x28..decoy + 0x2c].copy_from_slice(&3u32.to_le_bytes());

    let regions = common::relocated(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);
    let region = regions
        .iter()
        .find(|r| r.start_addr() <= fx.slot_addr && fx.slot_addr < r.end_addr())
        .expect("slot region");
    let mut got = [0u8; 8];
    assert_eq!(region.read(fx.slot_addr, &mut got), Some(8));
    assert_eq!(u64::from_le_bytes(got), 0x40);
}

/// A big-endian MIPS32 `R_MIPS_COPY` in a `.rel.dyn` linked to no symbol
/// table, over a 16-byte executable slot: which table its index means is
/// in doubt, and with it the object's size, so the hole runs to the end of
/// the mapping rather than over the one word at `r_offset`.
#[test]
fn a_mips32_copy_off_the_dynsym_is_a_hole_to_its_mappings_end() {
    let mut fx = common::elf_fixture::build_rel_elf_placed(
        common::elf_fixture::RelOpts {
            endian: object::Endianness::Big,
            is_64: false,
            e_machine: object::elf::EM_MIPS,
            r_type: object::elf::R_MIPS_COPY,
            defined_symbol: true,
            slot_init: vec![0x11; 16],
        },
        common::elf_fixture::RelPlacement {
            slot_exec: true,
            ..Default::default()
        },
    );
    // Elf32_Ehdr.e_shoff, then Elf32_Shdr.sh_link of `.rel.dyn`.
    let at = {
        use object::{Object as _, ObjectSection as _};
        let obj = object::File::parse(&fx.bytes[..]).unwrap();
        let index = obj.section_by_name(".rel.dyn").unwrap().index().0;
        u32::from_be_bytes(fx.bytes[0x20..0x24].try_into().unwrap()) as usize + 40 * index + 0x18
    };
    fx.bytes[at..at + 4].copy_from_slice(&0u32.to_be_bytes());
    let table =
        strider_reader::MemRegionsLookupTable::new(common::load_with_relocations(&fx.bytes));
    for off in [0, 4, 8, 12] {
        assert_eq!(
            table.unmodelled_relocation_at(fx.slot_addr + off),
            Some(object::elf::R_MIPS_COPY),
            "slot+{off}"
        );
    }
}

/// GCC's mips64 `.gpdword` jump-table entry is `R_MIPS_GPREL32 | R_MIPS_64`,
/// an 8-byte field this loader does not compute: neither half is served.
#[test]
fn an_uncomputed_8_byte_mips64_composite_serves_no_byte() {
    use strider_reader::ReadOnlyMemory as _;
    let path = common::fixtures_out().join("mips64be/switch_masked_loop.o");
    let elf = strider_reader::OwnedElf::open(&path).expect("open");
    let obj = elf.checked_file().expect("file");
    let rodata = obj.section_by_name(".rodata").expect(".rodata");
    let base = strider_reader::elf::ElfSectionLayout::new(&obj).section_base(&rodata);
    let reader = strider_reader::ElfFileMemReader::new(
        &elf,
        strider_reader::elf::RelocationMode::Applied { got_is_rom: false },
    )
    .expect("reader");
    let mut half = [0u8; 4];
    assert!(!reader.try_read(base, &mut half), "high half");
    assert!(
        !reader.try_read(base + 4, &mut half),
        "low half served as {half:02x?}"
    );
}
