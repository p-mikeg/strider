//! `apply_elf_relocations` against `fixtures/out/<arch>/elf_relocs.elf`, a
//! `-shared -fPIC` fixture whose `dispatch_table` has one relocation per slot
//! pointing at `helper_a..helper_d`. Unapplied the slots read zero; applied
//! they read the helper addresses.

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
fn apply_elf_relocations_defined_mips_rel32_writes_symbol_value() {
    // A defined-symbol REL32 is `S + A`, not addend-only; the addend-only
    // reduction holds only for the undefined / index-0 (STN_UNDEF) case. The
    // fixture uses a `REL` (implicit-addend) section so A = 0, isolating the
    // symbol-value contribution.
    let fx = common::elf_fixture::build_mips32be_rel32_elf();
    let obj = object::File::parse(&fx.bytes[..]).expect("parse fixture");

    // Include writable sections so `.data.rel.ro` has a region to patch.
    let mut regions = common::regions(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);
    strider_reader::elf::apply_elf_relocations(
        &mut regions,
        &obj,
        strider_reader::elf::LoadFilter::AllAllocatable,
    )
    .expect("apply");

    assert_eq!(
        read_u32_be(&regions, fx.slot_addr),
        Some(fx.sym_addr as u32),
        "defined-symbol REL32 must write S + A (= sym_addr), not addend-only (0)"
    );
}

#[test]
fn apply_elf_relocations_undefined_mips_rel32_stays_addend_only() {
    // The STN_UNDEF case stays addend-only: `S = 0` and the `REL` addend is 0,
    // so the slot reads 0. `object` reports it as `RelocationTarget::Absolute`,
    // routing through `image_relative_reloc`.
    let fx = common::elf_fixture::build_mips32be_rel32_elf_with(false);
    let obj = object::File::parse(&fx.bytes[..]).expect("parse fixture");

    let mut regions = common::regions(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);
    strider_reader::elf::apply_elf_relocations(
        &mut regions,
        &obj,
        strider_reader::elf::LoadFilter::AllAllocatable,
    )
    .expect("apply");

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
    let obj = object::File::parse(&fx.bytes[..]).expect("parse fixture");

    let mut regions = common::regions(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);
    strider_reader::elf::apply_elf_relocations(
        &mut regions,
        &obj,
        strider_reader::elf::LoadFilter::AllAllocatable,
    )
    .expect("apply");

    assert_eq!(
        read_u32_le_at(&regions, fx.slot_addr),
        Some(fx.sym_addr as u32 + addend),
        "R_386_32 in a REL table must write S + A, A being the field's own bytes"
    );
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
    let obj = object::File::parse(&fx.bytes[..]).expect("parse fixture");

    let mut regions = common::regions(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);
    strider_reader::elf::apply_elf_relocations(
        &mut regions,
        &obj,
        strider_reader::elf::LoadFilter::AllAllocatable,
    )
    .expect("apply");

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
    let obj = object::File::parse(&fx.bytes[..]).expect("parse fixture");

    let mut regions = common::regions(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);
    strider_reader::elf::apply_elf_relocations(
        &mut regions,
        &obj,
        strider_reader::elf::LoadFilter::AllAllocatable,
    )
    .expect("apply");

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
        let obj = object::File::parse(&fx.bytes[..]).expect("parse fixture");
        let mut regions =
            common::regions(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);
        strider_reader::elf::apply_elf_relocations(
            &mut regions,
            &obj,
            strider_reader::elf::LoadFilter::AllAllocatable,
        )
        .expect("apply");

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
    let obj = object::File::parse(&fx.bytes[..]).expect("parse fixture");

    let mut regions = common::regions(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);
    strider_reader::elf::apply_elf_relocations(
        &mut regions,
        &obj,
        strider_reader::elf::LoadFilter::AllAllocatable,
    )
    .expect("apply");

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
    let obj = object::File::parse(&fx.bytes[..]).expect("parse fixture");

    let mut regions = common::regions(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);
    strider_reader::elf::apply_elf_relocations(
        &mut regions,
        &obj,
        strider_reader::elf::LoadFilter::AllAllocatable,
    )
    .expect("apply");

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
    let obj = object::File::parse(&fx.bytes[..]).expect("parse fixture");

    let mut regions = strider_reader::elf::elf_get_loadable_regions(&obj).expect("regions");
    assert_eq!(
        regions.iter().map(common::region_bytes).collect::<Vec<_>>(),
        vec![fx.text_bytes.clone()],
        "fixture geometry: the code-and-readonly load materialises `.text.f`",
    );

    strider_reader::elf::apply_elf_relocations(
        &mut regions,
        &obj,
        strider_reader::elf::LoadFilter::CodeAndReadOnly,
    )
    .expect("apply");

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
    let obj = object::File::parse(&fx.bytes[..]).expect("parse fixture");

    let mut regions = common::regions(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);
    assert_eq!(
        regions
            .iter()
            .map(|r| (r.start_addr(), common::region_bytes(r)))
            .collect::<Vec<_>>(),
        vec![
            (0, fx.data_bytes.clone()),
            (fx.data_bytes.len() as u64, fx.text_bytes.clone()),
        ],
        "fixture geometry: `.data` (index 1) keeps VMA 0, `.text.f` follows it",
    );

    strider_reader::elf::apply_elf_relocations(
        &mut regions,
        &obj,
        strider_reader::elf::LoadFilter::AllAllocatable,
    )
    .expect("apply");

    assert_eq!(read_u64_le(&regions, 0), Some(fx.sym_value));
    assert_eq!(
        common::region_bytes(&regions[1]),
        fx.text_bytes,
        "`.rela.data` must not reach `.text.f`"
    );
}

#[test]
fn apply_elf_relocations_patches_slot_at_very_end_of_region() {
    // The slot segment is exactly the 4-byte site, so the patch's last byte is
    // the region's last byte, where an off-by-one would reject or overrun. The
    // geometry is asserted first so a future fixture reshuffle that pads the
    // segment cannot silently demote this to an interior patch.
    let fx = common::elf_fixture::build_mips32be_rel32_elf();
    let obj = object::File::parse(&fx.bytes[..]).expect("parse fixture");

    let mut regions = common::regions(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);
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

    strider_reader::elf::apply_elf_relocations(
        &mut regions,
        &obj,
        strider_reader::elf::LoadFilter::AllAllocatable,
    )
    .expect("apply");
    assert_eq!(
        read_u32_be(&regions, fx.slot_addr),
        Some(fx.sym_addr as u32),
        "patch touching the region's final bytes must apply cleanly",
    );
}

#[test]
fn apply_elf_relocations_field_straddling_section_end_is_not_patched() {
    // `.data.rel.ro` is 6 bytes with the 4-byte PC32 site at offset 4, so the
    // field `[4, 8)` runs past its file-backed bytes. The site's first byte
    // lands inside the region but the full field straddles its end, so the
    // patch cannot land and the slot stays zeroed.
    let fx =
        common::elf_fixture::build_x86_64_pc32_rela_elf(/* slot_len */ 6, /* off */ 4, 0);
    let obj = object::File::parse(&fx.bytes[..]).expect("parse fixture");

    let mut regions = common::regions(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);

    strider_reader::elf::apply_elf_relocations(
        &mut regions,
        &obj,
        strider_reader::elf::LoadFilter::AllAllocatable,
    )
    .expect("apply");

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
fn apply_elf_relocations_negative_addend_pc_relative() {
    // A PC32 (`S + A - P`) with a negative addend. The applier bitcasts the
    // i64 to u64, wrapping-adds, and truncates to the 4-byte field, giving the
    // correct modular result. Guards against a future "fix" to a checked or
    // saturating add.
    let addend: i64 = -0x40;
    let fx = common::elf_fixture::build_x86_64_pc32_rela_elf(
        /* slot_len */ 4, /* off */ 0, addend,
    );
    let obj = object::File::parse(&fx.bytes[..]).expect("parse fixture");

    let mut regions = common::regions(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);
    strider_reader::elf::apply_elf_relocations(
        &mut regions,
        &obj,
        strider_reader::elf::LoadFilter::AllAllocatable,
    )
    .expect("apply");

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
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out")
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
fn apply_elf_relocations_patches_dispatch_table_x86_64() {
    let path = fixture_path("x64", "elf_relocs");
    if !path.exists() {
        panic!("missing {path:?}; run `make -C fixtures CASE=elf_relocs ARCH=x64`");
    }
    let owned = strider_reader::load_elf(&path).expect("load_elf");
    let obj = owned.file();
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
    if !path.exists() {
        // A missing fixture must be VISIBLE: a silent return reports as a pass
        // and this file is the only coverage the ET_REL loader has.
        eprintln!(
            "SKIP {}: {} is not built; run `make -C fixtures`",
            module_path!(),
            path.display()
        );
        return;
    }
    let obj = strider_reader::load_elf(&path).expect("load_elf");
    let obj = obj.file();
    let regions = strider_reader::elf::elf_get_loadable_regions(&obj).unwrap();
    let table_addr = sym_addr(&obj, "dispatch_table");
    assert!(
        read_u64_le(&regions, table_addr).is_none(),
        "default loader must NOT cover `.data.rel.ro`; apply_relocations=True is required"
    );
}

#[test]
fn apply_elf_relocations_no_op_on_pre_resolved_binary() {
    // `control.elf` is ET_EXEC, so `dynamic_relocations()` is empty and the
    // applier no-ops. Justifies relocation processing being opt-in: a normal
    // userland binary does not need it.
    let path = fixture_path("x86", "control");
    if !path.exists() {
        // A missing fixture must be VISIBLE: a silent return reports as a pass
        // and this file is the only coverage the ET_REL loader has.
        eprintln!(
            "SKIP {}: {} is not built; run `make -C fixtures`",
            module_path!(),
            path.display()
        );
        return; // skip if fixture not built
    }
    let obj = strider_reader::load_elf(&path).expect("load_elf");
    let obj = obj.file();
    let mut regions = strider_reader::elf::elf_get_loadable_regions(&obj).expect("regions");
    // Any GLOB_DAT / JUMP_SLOT entries target undefined externs and are
    // deliberately skipped, so the regions stay byte-for-byte identical.
    let before: Vec<Vec<u8>> = regions.iter().map(common::region_bytes).collect();
    strider_reader::elf::apply_elf_relocations(
        &mut regions,
        &obj,
        strider_reader::elf::LoadFilter::CodeAndReadOnly,
    )
    .expect("apply");
    let after: Vec<Vec<u8>> = regions.iter().map(common::region_bytes).collect();
    assert_eq!(
        before, after,
        "ET_EXEC pre-link-resolved binary must have nothing to apply"
    );
}
#[test]
fn apply_elf_relocations_idempotent() {
    // Each relocation is a deterministic write, so re-applying overwrites with
    // the same value.
    let path = fixture_path("x64", "elf_relocs");
    if !path.exists() {
        // A missing fixture must be VISIBLE: a silent return reports as a pass
        // and this file is the only coverage the ET_REL loader has.
        eprintln!(
            "SKIP {}: {} is not built; run `make -C fixtures`",
            module_path!(),
            path.display()
        );
        return;
    }
    let owned = strider_reader::load_elf(&path).expect("load_elf");
    let obj = owned.file();
    let mut regions = owned
        .regions(
            strider_reader::elf::RegionSource::Auto,
            strider_reader::elf::LoadFilter::AllAllocatable,
            false,
        )
        .unwrap();
    strider_reader::elf::apply_elf_relocations(
        &mut regions,
        &obj,
        strider_reader::elf::LoadFilter::AllAllocatable,
    )
    .unwrap();
    let snapshot: Vec<Vec<u8>> = regions.iter().map(common::region_bytes).collect();
    strider_reader::elf::apply_elf_relocations(
        &mut regions,
        &obj,
        strider_reader::elf::LoadFilter::AllAllocatable,
    )
    .unwrap();
    let after: Vec<Vec<u8>> = regions.iter().map(common::region_bytes).collect();
    assert_eq!(snapshot, after, "apply_elf_relocations is not idempotent");
}
/// The colliding sections hold the SAME bytes, which a zero-initialised or
/// same-length pair does routinely. Nothing about the region's contents can
/// then say which section the load accepted, so ownership has to come from the
/// load itself.
#[test]
fn et_rel_relocations_apply_when_the_colliding_sections_are_byte_identical() {
    let fx = common::elf_fixture::build_et_rel_vma_collision_elf_with(vec![0u8; 8]);
    let obj = object::File::parse(&fx.bytes[..]).expect("parse fixture");
    assert_eq!(fx.text_bytes, fx.data_bytes, "fixture geometry: bytes tie");

    let mut regions = common::regions(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable);
    strider_reader::elf::apply_elf_relocations(
        &mut regions,
        &obj,
        strider_reader::elf::LoadFilter::AllAllocatable,
    )
    .expect("apply");

    assert_eq!(
        read_u64_le(&regions, 0),
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
    let obj = object::File::parse(&fx.bytes[..]).expect("parse fixture");

    let mut regions = strider_reader::elf::elf_get_loadable_regions(&obj).expect("regions");
    strider_reader::elf::apply_elf_relocations(
        &mut regions,
        &obj,
        strider_reader::elf::LoadFilter::CodeAndReadOnly,
    )
    .expect("apply");

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
    let obj = object::File::parse(&fx.bytes[..]).expect("parse fixture");
    let data_len = fx.data_bytes.len() as u64;

    // `.data` is section index 1 and `.text.f` index 2, so `.data` keeps VMA 0
    // and `.text.f` is placed just past it.
    for (name, regions, expected) in [
        (
            "code-and-readonly",
            strider_reader::elf::elf_get_loadable_regions(&obj).expect("regions"),
            vec![(data_len, fx.text_bytes.clone())],
        ),
        (
            "all-allocatable",
            common::regions(&fx.bytes, strider_reader::elf::LoadFilter::AllAllocatable),
            vec![
                (0, fx.data_bytes.clone()),
                (data_len, fx.text_bytes.clone()),
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
/// exists once the link allocates it in `.bss`. Applying a relocation against
/// that value patches a site with a fabricated target, so an unallocated
/// common must be skipped exactly as an undefined extern is.
#[test]
fn a_common_symbol_relocation_is_skipped_not_applied() {
    let fx = common::elf_fixture::build_et_rel_vma_collision_elf_full(
        vec![0u8; 8],
        object::elf::SHN_COMMON,
        4, // the alignment, not an address
    );
    let regions = common::load_with_relocations(&fx.bytes);
    let table = strider_reader::MemRegionsLookupTable::new(regions);
    let mut got = [0u8; 8];
    table
        .read_exact(0, &mut got)
        .expect("read the relocated site");
    assert_eq!(
        got, [0u8; 8],
        "a SHN_COMMON symbol's st_value is its alignment; the relocation must \
         be skipped, not applied with 4 as the address",
    );
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
        "R_MIPS_COPY is unhandled; the site must keep its file-initial bytes"
    );
}

/// Relocations are a patch list applied to whatever part of a read they cover,
/// so reads of different widths and alignments over one site must agree.
#[test]
fn a_read_straddling_a_relocated_site_serves_the_patched_bytes() {
    let path = fixture_path("x64", "elf_relocs");
    if !path.exists() {
        // A missing fixture must be VISIBLE: a silent return reports as a pass
        // and this file is the only coverage the ET_REL loader has.
        eprintln!(
            "SKIP {}: {} is not built; run `make -C fixtures`",
            module_path!(),
            path.display()
        );
        return;
    }
    let owned = strider_reader::load_elf(&path).expect("load_elf");
    let obj = owned.file();
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
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out/ppc32be/switch_masked_loop.o");
    let owned = strider_reader::load_elf(&path).expect("load_elf");
    let obj = owned.file();
    let mut regions = strider_reader::elf::elf_get_loadable_regions(&obj).expect("regions");
    strider_reader::elf::apply_elf_relocations(
        &mut regions,
        &obj,
        strider_reader::elf::LoadFilter::CodeAndReadOnly,
    )
    .expect("apply");

    // `S + A - P` for entry 0 is (.text + 0x90) - 0x1d0 = -0x140.
    let raw = read_u32_be(&regions, 0x1d0).expect("the rebased .rodata table");
    let first = raw as i32;
    assert_eq!(first, -0x140, "table[0] must be patched, not left at 0");
    assert_eq!(
        0x1d0i64 + i64::from(first),
        0x90,
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
/// `.data + 8` -- the rebased start of `.text.f`, whose body would take the
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
        .read_exact(fx.data_bytes.len() as u64, &mut got)
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
