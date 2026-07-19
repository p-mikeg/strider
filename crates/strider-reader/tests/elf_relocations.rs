//! `apply_elf_relocations` against `fixtures/out/<arch>/elf_relocs.elf`, a
//! `-shared -fPIC` fixture whose `dispatch_table` has one relocation per slot
//! pointing at `helper_a..helper_d`. Unapplied the slots read zero; applied
//! they read the helper addresses.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use object::{Object, ObjectSymbol};
use std::path::PathBuf;

#[path = "common/mod.rs"]
mod common;

fn read_u32_be(regions: &[strider_reader::MemRegion], addr: u64) -> Option<u32> {
    for r in regions {
        if r.contains(addr) && addr + 4 <= r.end_addr() {
            let off = (addr - r.start_addr()) as usize;
            let bytes = &r.data()[off..off + 4];
            return Some(u32::from_be_bytes(bytes.try_into().unwrap()));
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
    let mut regions =
        strider_reader::elf::elf_get_loadable_regions_including_writable(&obj).expect("regions");
    strider_reader::elf::apply_elf_relocations(&mut regions, &obj).expect("apply");

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

    let mut regions =
        strider_reader::elf::elf_get_loadable_regions_including_writable(&obj).expect("regions");
    strider_reader::elf::apply_elf_relocations(&mut regions, &obj).expect("apply");

    assert_eq!(
        read_u32_be(&regions, fx.slot_addr),
        Some(0),
        "undefined-symbol REL32 stays addend-only (0)"
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

    let mut regions =
        strider_reader::elf::elf_get_loadable_regions_including_writable(&obj).expect("regions");
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

    strider_reader::elf::apply_elf_relocations(&mut regions, &obj).expect("apply");
    assert_eq!(
        read_u32_be(&regions, fx.slot_addr),
        Some(fx.sym_addr as u32),
        "patch touching the region's final bytes must apply cleanly",
    );
}

#[test]
fn apply_elf_relocations_autoload_field_straddling_section_end_is_not_patched() {
    // `.data.rel.ro` is 6 bytes with the 4-byte PC32 site at offset 4, so the
    // field `[4, 8)` runs past its file-backed bytes. Loading code-only forces
    // autoload to stage the section; the site's first byte lands inside the
    // staged region but the full field straddles its end, so the patch cannot
    // land and the slot stays zeroed.
    let fx =
        common::elf_fixture::build_x86_64_pc32_rela_elf(/* slot_len */ 6, /* off */ 4, 0);
    let obj = object::File::parse(&fx.bytes[..]).expect("parse fixture");

    let mut regions = strider_reader::elf::elf_get_loadable_regions(&obj).expect("regions");

    strider_reader::elf::apply_elf_relocations_autoload(&mut regions, &obj).expect("autoload");

    let staged = regions
        .iter()
        .find(|r| r.contains(fx.site_addr))
        .expect("autoload must stage the section covering the site's first byte");
    let off = (fx.site_addr - staged.start_addr()) as usize;
    assert_eq!(
        staged.data()[off],
        0,
        "a field straddling the staged region's end must NOT be patched"
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

    let mut regions =
        strider_reader::elf::elf_get_loadable_regions_including_writable(&obj).expect("regions");
    strider_reader::elf::apply_elf_relocations(&mut regions, &obj).expect("apply");

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

fn read_u32_le_at(regions: &[strider_reader::MemRegion], addr: u64) -> Option<u32> {
    for r in regions {
        if r.contains(addr) && addr + 4 <= r.end_addr() {
            let off = (addr - r.start_addr()) as usize;
            let bytes = &r.data()[off..off + 4];
            return Some(u32::from_le_bytes(bytes.try_into().unwrap()));
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
        if r.contains(addr) && addr + 8 <= r.end_addr() {
            let off = (addr - r.start_addr()) as usize;
            let bytes = &r.data()[off..off + 8];
            return Some(u64::from_le_bytes(bytes.try_into().unwrap()));
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
    let obj = strider_reader::load_elf(&path).expect("load_elf");
    let obj = obj.file();
    // `dispatch_table` lives in `.data.rel.ro`, which the default
    // code-and-readonly loader skips as writable; the wider loader picks it up
    // so the applier has somewhere to patch.
    let regions = strider_reader::elf::elf_load_with_relocations(&obj).expect("load+apply");

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
        return;
    }
    let obj = strider_reader::load_elf(&path).expect("load_elf");
    let obj = obj.file();
    let regions = strider_reader::elf::elf_get_loadable_regions(&obj).unwrap();
    let table_addr = sym_addr(&obj, "dispatch_table");
    assert!(
        read_u64_le(&regions, table_addr).is_none(),
        "default loader must NOT cover `.data.rel.ro` — apply_relocations=True is required"
    );
}

#[test]
fn apply_elf_relocations_no_op_on_pre_resolved_binary() {
    // `control.elf` is ET_EXEC, so `dynamic_relocations()` is empty and the
    // applier no-ops. Justifies relocation processing being opt-in: a normal
    // userland binary does not need it.
    let path = fixture_path("x86", "control");
    if !path.exists() {
        return; // skip if fixture not built
    }
    let obj = strider_reader::load_elf(&path).expect("load_elf");
    let obj = obj.file();
    let mut regions = strider_reader::elf::elf_get_loadable_regions(&obj).expect("regions");
    // Any GLOB_DAT / JUMP_SLOT entries target undefined externs and are
    // deliberately skipped, so the regions stay byte-for-byte identical.
    let before: Vec<Vec<u8>> = regions.iter().map(|r| r.data().to_vec()).collect();
    strider_reader::elf::apply_elf_relocations(&mut regions, &obj).expect("apply");
    let after: Vec<Vec<u8>> = regions.iter().map(|r| r.data().to_vec()).collect();
    assert_eq!(
        before, after,
        "ET_EXEC pre-link-resolved binary must have nothing to apply"
    );
}

#[test]
fn apply_elf_relocations_autoload_pulls_in_missing_site_sections() {
    // The i386 kernel scenario: load code-and-readonly only, excluding
    // `.data.rel.ro`, and autoload must pull the missing section in so every
    // relocation still lands.
    let path = fixture_path("x64", "elf_relocs");
    if !path.exists() {
        return;
    }
    let obj = strider_reader::load_elf(&path).expect("load_elf");
    let obj = obj.file();
    let mut regions = strider_reader::elf::elf_get_loadable_regions(&obj).unwrap();
    let regions_before = regions.len();

    strider_reader::elf::apply_elf_relocations_autoload(&mut regions, &obj)
        .expect("autoload apply");

    assert!(
        regions.len() > regions_before,
        "autoload must have added at least one region"
    );

    let table_addr = sym_addr(&obj, "dispatch_table");
    let helper_a = sym_addr(&obj, "helper_a");
    assert_eq!(read_u64_le(&regions, table_addr), Some(helper_a));
}

#[test]
fn apply_elf_relocations_idempotent() {
    // Each relocation is a deterministic write, so re-applying overwrites with
    // the same value.
    let path = fixture_path("x64", "elf_relocs");
    if !path.exists() {
        return;
    }
    let obj = strider_reader::load_elf(&path).expect("load_elf");
    let obj = obj.file();
    let mut regions =
        strider_reader::elf::elf_get_loadable_regions_including_writable(&obj).unwrap();
    strider_reader::elf::apply_elf_relocations(&mut regions, &obj).unwrap();
    let snapshot: Vec<Vec<u8>> = regions.iter().map(|r| r.data().to_vec()).collect();
    strider_reader::elf::apply_elf_relocations(&mut regions, &obj).unwrap();
    let after: Vec<Vec<u8>> = regions.iter().map(|r| r.data().to_vec()).collect();
    assert_eq!(snapshot, after, "apply_elf_relocations is not idempotent");
}

#[test]
fn apply_elf_relocations_autoload_is_idempotent() {
    // The second call sees the previously-staged sections and skips re-staging.
    let path = fixture_path("x64", "elf_relocs");
    if !path.exists() {
        return;
    }
    let obj = strider_reader::load_elf(&path).expect("load_elf");
    let obj = obj.file();
    let mut regions = strider_reader::elf::elf_get_loadable_regions(&obj).unwrap();

    strider_reader::elf::apply_elf_relocations_autoload(&mut regions, &obj).unwrap();
    let snapshot: Vec<(u64, Vec<u8>)> = regions
        .iter()
        .map(|r| (r.start_addr(), r.data().to_vec()))
        .collect();

    strider_reader::elf::apply_elf_relocations_autoload(&mut regions, &obj).unwrap();
    let after: Vec<(u64, Vec<u8>)> = regions
        .iter()
        .map(|r| (r.start_addr(), r.data().to_vec()))
        .collect();

    assert_eq!(snapshot, after, "autoload must be idempotent");
}

#[test]
fn apply_elf_relocations_autoload_does_not_fabricate_values_for_undefined_externs() {
    // `control`'s GLOB_DAT / JMP_SLOT entries target undefined libc externs.
    // Autoload pulls the .got / .got.plt sections in regardless, but the
    // applier must still refuse to write: every staged region keeps the
    // section's file-initial bytes.
    use object::ObjectSection as _;

    let path = fixture_path("x86", "control");
    if !path.exists() {
        return;
    }
    let obj = strider_reader::load_elf(&path).expect("load_elf");
    let obj = obj.file();
    let mut regions = strider_reader::elf::elf_get_loadable_regions(&obj).unwrap();
    let regions_before = regions.len();

    strider_reader::elf::apply_elf_relocations_autoload(&mut regions, &obj).unwrap();

    for r in &regions[regions_before..] {
        let sec = obj
            .sections()
            .find(|s| s.address() == r.start_addr())
            .expect("staged region must correspond to an ELF section");
        let file_bytes = sec.data().expect("section data");
        assert_eq!(
            r.data(),
            &file_bytes[..r.data().len()],
            "undefined-extern slots must keep their file-initial bytes (no fabricated value)"
        );
    }
}

#[test]
fn apply_elf_relocations_autoload_preserves_preloaded_bytes_on_pre_resolved_binary() {
    // `arithmetic` is a dynamically-linked ET_EXEC whose only dynamic relocs
    // target undefined externs and are skipped. Autoload may still stage an
    // uncovered GOT section, so the region set can grow, but with nothing to
    // apply it must not touch the originally-loaded bytes.
    let path = fixture_path("x86", "arithmetic");
    if !path.exists() {
        return;
    }
    let obj = strider_reader::load_elf(&path).expect("load_elf");
    let obj = obj.file();
    let mut regions = strider_reader::elf::elf_get_loadable_regions(&obj).unwrap();
    let before: Vec<(u64, Vec<u8>)> = regions
        .iter()
        .map(|r| (r.start_addr(), r.data().to_vec()))
        .collect();

    strider_reader::elf::apply_elf_relocations_autoload(&mut regions, &obj).unwrap();

    // Autoload only appends, so every originally-loaded region survives.
    for (start, bytes) in &before {
        let after = regions
            .iter()
            .find(|r| r.start_addr() == *start)
            .expect("originally-loaded region must survive autoload");
        assert_eq!(
            after.data(),
            bytes.as_slice(),
            "autoload must not patch preloaded bytes of region @ {start:#x}"
        );
    }
    assert!(
        regions.len() >= before.len(),
        "autoload never drops a preloaded region"
    );
}
