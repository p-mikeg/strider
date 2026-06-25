//! Integration tests for `strider_reader::elf::apply_elf_relocations`.
//!
//! Uses the `fixtures/out/<arch>/elf_relocs.elf` shared-library
//! fixture (built by `fixtures/Makefile` with `-shared -fPIC`).  The
//! fixture's `dispatch_table` array has one relocation per slot
//! pointing at one of `helper_a..helper_d`; without the applier the
//! slots read zero, with the applier they read the helper addresses.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use object::{Object, ObjectSymbol};
use std::path::PathBuf;

#[path = "common/mod.rs"]
mod common;

/// Read 4 bytes (big-endian) from `regions` at virtual address `addr`.
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
    // A defined-symbol `R_MIPS_REL32` has `S + A` semantics (symbol
    // value plus addend), not addend-only — the addend-only reduction
    // is correct *only* for the undefined / index-0 (STN_UNDEF) case.
    // The `.data.rel.ro` slot here points at the defined `func` symbol
    // (st_value = sym_addr); after relocation the slot must read
    // `sym_addr`, not 0.  The fixture is a `REL` (implicit-addend)
    // section so A = 0, isolating the symbol-value contribution.
    let fx = common::elf_fixture::build_mips32be_rel32_elf();
    let obj = object::File::parse(&fx.bytes[..]).expect("parse fixture");

    // Load every allocatable file-backed section so `.data.rel.ro`
    // (the relocation site) has a region to patch.
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
    // The undefined / index-0 (STN_UNDEF) REL32 case must remain
    // addend-only: `S = 0`, the `REL` section's addend is 0, so the
    // slot reads 0 after relocation.  `object` reports the index-0
    // reloc as `RelocationTarget::Absolute`, which routes through
    // `image_relative_reloc` (addend-only) — this pins that the
    // defined-symbol fix did not perturb the undefined path.
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
    // The synthetic MIPS fixture's slot segment is exactly the 4-byte
    // relocation site, so the patch's last byte IS the region's last
    // byte — the boundary case where an off-by-one in the applier's
    // range check would reject or overrun.  Assert the geometry first
    // so a future fixture reshuffle that pads the segment doesn't
    // silently demote this test to an interior-patch case.
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
    // Synthesised ELF: the `.data.rel.ro` section is 6 bytes, but the
    // 4-byte `R_X86_64_PC32` relocation site sits at offset 4 — so the
    // field `[off 4, off 8)` runs past the section's 6 file-backed bytes.
    //
    // Load code-only so autoload must stage `.data.rel.ro`.  The site's
    // FIRST byte is covered by the staged region, but the full field
    // straddles the region's end, so the patch can't land — the slot must
    // remain at its zeroed initial value.
    let fx =
        common::elf_fixture::build_x86_64_pc32_rela_elf(/* slot_len */ 6, /* off */ 4, 0);
    let obj = object::File::parse(&fx.bytes[..]).expect("parse fixture");

    // Code-and-readonly only: excludes the writable `.data.rel.ro`.
    let mut regions = strider_reader::elf::elf_get_loadable_regions(&obj).expect("regions");

    strider_reader::elf::apply_elf_relocations_autoload(&mut regions, &obj).expect("autoload");

    // The staged region covers the site's first byte but not the full
    // 4-byte field, so the relocation is dropped: the slot's first byte
    // stays zeroed.
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
    // A PC-relative `R_X86_64_PC32` (object's `RelocationKind::Relative`,
    // `S + A - P`) with a *negative* addend.  The applier casts the i64
    // addend to u64 (2's-complement bit pattern) and `wrapping_add`s it,
    // then truncates to the 4-byte field — the correct modular result.
    // This guards against a future "fix" to a checked/saturating add that
    // would silently break negative-addend relocations.
    let addend: i64 = -0x40;
    let fx = common::elf_fixture::build_x86_64_pc32_rela_elf(
        /* slot_len */ 4, /* off */ 0, addend,
    );
    let obj = object::File::parse(&fx.bytes[..]).expect("parse fixture");

    let mut regions =
        strider_reader::elf::elf_get_loadable_regions_including_writable(&obj).expect("regions");
    strider_reader::elf::apply_elf_relocations(&mut regions, &obj).expect("apply");

    // Expected: (S + A - P) truncated to 32 bits, written little-endian.
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

/// Read 4 bytes (LE) from `regions` at virtual address `addr`.
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

/// Read 8 bytes (LE) from `regions` at virtual address `addr`.
/// Returns `None` if no region covers the range.
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

/// Find the symbol's address; panic with a clear message when missing.
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
    // dispatch_table lives in `.data.rel.ro` (writable section that
    // gets relro-protected at runtime).  The default
    // code-and-readonly loader skips it; the wider loader picks it
    // up so the applier has somewhere to patch.
    let regions = strider_reader::elf::elf_load_with_relocations(&obj).expect("load+apply");

    let table_addr = sym_addr(&obj, "dispatch_table");
    let helper_a = sym_addr(&obj, "helper_a");
    let helper_b = sym_addr(&obj, "helper_b");
    let helper_c = sym_addr(&obj, "helper_c");
    let helper_d = sym_addr(&obj, "helper_d");

    // Post-condition: every dispatch_table slot now reads its helper.
    // A well-formed ELF resolves cleanly (no malformed / unresolved
    // targets), so all four slots are patched.
    assert_eq!(read_u64_le(&regions, table_addr), Some(helper_a));
    assert_eq!(read_u64_le(&regions, table_addr + 8), Some(helper_b));
    assert_eq!(read_u64_le(&regions, table_addr + 16), Some(helper_c));
    assert_eq!(read_u64_le(&regions, table_addr + 24), Some(helper_d));
}

#[test]
fn default_loader_omits_data_rel_ro() {
    // Sanity: without `apply_relocations`, the
    // `elf_get_loadable_regions` filter still excludes
    // `.data.rel.ro` (it's writable).  This is the
    // existing back-compat behaviour the Python `MemoryMap`
    // default mirrors.
    let path = fixture_path("x64", "elf_relocs");
    if !path.exists() {
        return;
    }
    let obj = strider_reader::load_elf(&path).expect("load_elf");
    let regions = strider_reader::elf::elf_get_loadable_regions(&obj).unwrap();
    let table_addr = sym_addr(&obj, "dispatch_table");
    assert!(
        read_u64_le(&regions, table_addr).is_none(),
        "default loader must NOT cover `.data.rel.ro` — apply_relocations=True is required"
    );
}

#[test]
fn apply_elf_relocations_no_op_on_pre_resolved_binary() {
    // Strider's existing fixtures (e.g. control.elf) are statically
    // linked executables (ET_EXEC) — `dynamic_relocations()` returns
    // an empty iterator and the applier is a no-op.  This guards
    // the "false default" of the strider-py opt-in flag: a normal
    // userland binary doesn't need relocation processing.
    let path = fixture_path("x86", "control");
    if !path.exists() {
        return; // skip if fixture not built
    }
    let obj = strider_reader::load_elf(&path).expect("load_elf");
    let mut regions = strider_reader::elf::elf_get_loadable_regions(&obj).expect("regions");
    // ET_EXEC with no dynamic relocations: the applier is a no-op (any
    // GLOB_DAT / JUMP_SLOT entries target undefined externs and are
    // deliberately skipped), so the regions are byte-for-byte unchanged.
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
    // Reproduces the i386 kernel scenario at the Rust level: load only
    // code-and-readonly (so `.data.rel.ro` is excluded), then call the
    // autoload variant.  It must lazily pull the missing section so
    // every relocation lands (verified below by the patched table slot).
    let path = fixture_path("x64", "elf_relocs");
    if !path.exists() {
        return;
    }
    let obj = strider_reader::load_elf(&path).expect("load_elf");
    let mut regions = strider_reader::elf::elf_get_loadable_regions(&obj).unwrap();
    let regions_before = regions.len();

    strider_reader::elf::apply_elf_relocations_autoload(&mut regions, &obj)
        .expect("autoload apply");

    assert!(
        regions.len() > regions_before,
        "autoload must have added at least one region"
    );

    // The autoload staged the `.data.rel.ro` section, so the dispatch
    // table slot is now covered and patched to its helper's address.
    let table_addr = sym_addr(&obj, "dispatch_table");
    let helper_a = sym_addr(&obj, "helper_a");
    assert_eq!(read_u64_le(&regions, table_addr), Some(helper_a));
}

#[test]
fn apply_elf_relocations_idempotent() {
    // Running the applier twice produces the same regions as running
    // it once.  Each relocation is a deterministic write; re-applying
    // overwrites with the same value.
    let path = fixture_path("x64", "elf_relocs");
    if !path.exists() {
        return;
    }
    let obj = strider_reader::load_elf(&path).expect("load_elf");
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
    // Running autoload twice must produce the same regions as
    // running it once: the second call sees the previously-staged
    // sections in `regions` and skips re-staging.
    let path = fixture_path("x64", "elf_relocs");
    if !path.exists() {
        return;
    }
    let obj = strider_reader::load_elf(&path).expect("load_elf");
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
    // ET_EXEC userland fixture (`control`) has GLOB_DAT / JMP_SLOT
    // entries that target undefined externs (libc).  Autoload happily
    // pulls the .got / .got.plt sections in, but the inner applier still
    // refuses to write a value because the symbol is undefined.
    //
    // Pins the property that autoload doesn't tempt the applier into
    // making up values for undefined externs: every staged region's
    // bytes must equal the ELF section's file-initial bytes (no patch
    // landed).
    use object::ObjectSection as _;

    let path = fixture_path("x86", "control");
    if !path.exists() {
        return;
    }
    let obj = strider_reader::load_elf(&path).expect("load_elf");
    let mut regions = strider_reader::elf::elf_get_loadable_regions(&obj).unwrap();
    let regions_before = regions.len();

    strider_reader::elf::apply_elf_relocations_autoload(&mut regions, &obj).unwrap();

    // Anything autoload staged beyond the original code+rodata set is a
    // GOT/GOT.PLT section pulled in to back an undefined-extern reloc; its
    // bytes must be byte-for-byte the file-initial section data.
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
fn apply_elf_relocations_autoload_no_op_when_no_dynamic_table() {
    // A statically-linked fixture (no dynamic_relocations()) ⇒ autoload
    // short-circuits with zero region mutation.  Uses `arithmetic`
    // because it's the simplest statically-linked fixture in the suite.
    let path = fixture_path("x86", "arithmetic");
    if !path.exists() {
        return;
    }
    let obj = strider_reader::load_elf(&path).expect("load_elf");
    let mut regions = strider_reader::elf::elf_get_loadable_regions(&obj).unwrap();
    let before: Vec<(u64, Vec<u8>)> = regions
        .iter()
        .map(|r| (r.start_addr(), r.data().to_vec()))
        .collect();

    strider_reader::elf::apply_elf_relocations_autoload(&mut regions, &obj).unwrap();

    // A statically-linked binary with no autoloadable dynamic relocs
    // leaves the region set byte-for-byte unchanged (no sections staged,
    // no patches applied).  When the linker did emit a small dynamic
    // table the previous tests cover the patching path; here we only pin
    // that nothing is corrupted in place.
    let after: Vec<(u64, Vec<u8>)> = regions
        .iter()
        .map(|r| (r.start_addr(), r.data().to_vec()))
        .collect();
    assert_eq!(
        before, after,
        "no autoloadable dynamic relocs ⇒ no in-place region mutation"
    );
}
