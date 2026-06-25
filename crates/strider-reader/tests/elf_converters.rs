#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for the section-walker behind
//! `elf_get_loadable_regions` and
//! `elf_get_loadable_regions_including_writable`.
//!
//! These two presets are the only ELF → [`MemRegion`] collectors exposed
//! by `strider_reader::elf`; both go through the same private
//! `collect_sections_as_mem_regions` walker, so the propagation /
//! empty-data / overflow contracts are pinned through whichever preset's
//! filter happens to accept the synthetic section.

#[path = "common/mod.rs"]
mod common;

use common::elf_fixture::{SectionSpec, build_elf_with_sections};
use strider_reader::elf::{elf_get_loadable_regions, elf_get_loadable_regions_including_writable};

/// Parses the bytes as an ELF; panics with a clear message if parse fails.
fn parse(bytes: &[u8]) -> object::File<'_> {
    object::File::parse(bytes).expect("parse synthetic ELF")
}

// ── elf_get_loadable_regions ─────────────────────

#[test]
fn elf_code_and_readonly_sections_include_text_and_rodata_exclude_data_and_bss() {
    let bytes = build_elf_with_sections(&[
        SectionSpec::text(0x1000, vec![1, 2]),   // exec     → include
        SectionSpec::rodata(0x2000, vec![3, 4]), // ro data  → include
        SectionSpec::data(0x3000, vec![5, 6]),   // writable → exclude
        SectionSpec::bss(0x4000, 16),            // NOBITS   → exclude (empty data)
    ]);
    let obj = parse(&bytes);
    let regions = elf_get_loadable_regions(&obj).unwrap();

    let addrs: Vec<u64> = regions.iter().map(|r| r.start_addr()).collect();
    assert!(addrs.contains(&0x1000), ".text must be included");
    assert!(addrs.contains(&0x2000), ".rodata must be included");
    assert!(!addrs.contains(&0x3000), ".data must be excluded");
    assert!(!addrs.contains(&0x4000), ".bss must be excluded");
    assert_eq!(regions.len(), 2);
}

// ── NOBITS sections are skipped ───────────────────────────────────────────

/// `.bss` is `SHT_NOBITS` — `section.data()` returns empty bytes. The
/// section walker treats empty-data sections as skippable regardless of
/// the preset's filter verdict.
#[test]
fn code_and_readonly_preset_skips_nobits() {
    let bytes = build_elf_with_sections(&[
        SectionSpec::text(0x1000, vec![1, 2, 3]),
        SectionSpec::bss(0x2000, 64),
    ]);
    let obj = parse(&bytes);
    let regions = elf_get_loadable_regions(&obj).unwrap();

    let addrs: Vec<u64> = regions.iter().map(|r| r.start_addr()).collect();
    assert!(addrs.contains(&0x1000), ".text must be present");
    assert!(!addrs.contains(&0x2000), ".bss (NOBITS) must be skipped");
}

// ── malformed accepted section surfaces as an error, not a silent skip ───

/// Pinned contract: when an accepted section's `section.data()` fails,
/// the section walker propagates the `object::Error` rather than silently
/// skipping the offending section.  NOBITS sections (where `data()`
/// returns `Ok(&[])`) are the *only* legitimate skip path; a real `Err`
/// means the ELF is malformed and silently dropping it would hand the
/// caller a partially-loaded reader.
///
/// We synthesize the failure by pointing a PROGBITS section (non-writable
/// so the code+rodata preset accepts it) at a file offset past the end of
/// the buffer, which makes `section.data()` return `Err`.
#[test]
fn code_and_readonly_preset_propagates_data_error() {
    use object::write::elf::{FileHeader, SectionHeader, Writer};
    use object::{Endianness, elf};

    let mut buf = Vec::new();
    {
        let mut w = Writer::new(Endianness::Little, true, &mut buf);
        let _null = w.reserve_null_section_index();
        let name = w.add_section_name(b".broken");
        let _sec = w.reserve_section_index();
        let _shstr = w.reserve_shstrtab_section_index();

        w.reserve_file_header();
        w.reserve_shstrtab();
        w.reserve_section_headers();

        w.write_file_header(&FileHeader {
            os_abi: elf::ELFOSABI_SYSV,
            abi_version: 0,
            // ET_REL: a section-only fixture has no PT_LOAD segments,
            // so the kind-dispatched loader walks sections (the ET_REL
            // / fallback path).  Marking the ELF as ET_EXEC would
            // route through the segments path with an empty segment
            // list, which is not what these section-walker tests aim
            // to pin.
            e_type: elf::ET_REL,
            e_machine: elf::EM_X86_64,
            e_entry: 0,
            e_flags: 0,
        })
        .expect("write file header");
        w.write_shstrtab();
        w.write_null_section_header();
        w.write_section_header(&SectionHeader {
            name: Some(name),
            sh_type: elf::SHT_PROGBITS,
            // SHF_ALLOC, no SHF_WRITE → code+rodata preset accepts.
            sh_flags: u64::from(elf::SHF_ALLOC),
            sh_addr: 0x1000,
            sh_offset: 0xdead_beef, // past EOF → data() must fail
            sh_size: 4,
            sh_link: 0,
            sh_info: 0,
            sh_addralign: 1,
            sh_entsize: 0,
        });
        w.write_shstrtab_section_header();
    }
    let obj = parse(&buf);
    let err = elf_get_loadable_regions(&obj)
        .expect_err("malformed accepted section must surface an error");
    assert!(
        err.to_string().contains("failed to parse ELF"),
        "got: {err}",
    );
}

// ── Pinned contract: RegionOverflow from MemRegion::new propagates ───────

/// Pinned contract: when an accepted section's `sh_addr + sh_size` would
/// overflow `u64`, `MemRegion::new` returns an overflow error, and the
/// section walker must propagate that — *not* silently drop it and not
/// rewrap it as an `object`-crate parse error.
///
/// Complements `code_and_readonly_preset_propagates_data_error`: that
/// test pins the `object::Error` arm of the walker's error set; this
/// test pins the overflow arm.  Together they enumerate every error path
/// the walker can take.
///
/// We synthesize the failure by building a section whose `sh_addr` is one
/// less than `u64::MAX` and whose `sh_size` is 4.  The data block fits in
/// the file (no `object::Error`), but `addr + len` overflows by 3 bytes,
/// so `MemRegion::new` must reject it.
#[test]
fn code_and_readonly_preset_propagates_region_overflow() {
    use object::write::elf::{FileHeader, SectionHeader, Writer};
    use object::{Endianness, elf};

    let payload = [0u8, 0, 0, 0]; // 4 bytes of data on disk

    let mut buf = Vec::new();
    {
        let mut w = Writer::new(Endianness::Little, true, &mut buf);
        let _null = w.reserve_null_section_index();
        let name = w.add_section_name(b".overflow");
        let _sec = w.reserve_section_index();
        let _shstr = w.reserve_shstrtab_section_index();

        w.reserve_file_header();
        let data_off = w.reserve(payload.len(), 1);
        w.reserve_shstrtab();
        w.reserve_section_headers();

        w.write_file_header(&FileHeader {
            os_abi: elf::ELFOSABI_SYSV,
            abi_version: 0,
            // ET_REL: a section-only fixture has no PT_LOAD segments,
            // so the kind-dispatched loader walks sections (the ET_REL
            // / fallback path).  Marking the ELF as ET_EXEC would
            // route through the segments path with an empty segment
            // list, which is not what these section-walker tests aim
            // to pin.
            e_type: elf::ET_REL,
            e_machine: elf::EM_X86_64,
            e_entry: 0,
            e_flags: 0,
        })
        .expect("write file header");
        w.write(&payload);
        w.write_shstrtab();
        w.write_null_section_header();
        w.write_section_header(&SectionHeader {
            name: Some(name),
            sh_type: elf::SHT_PROGBITS,
            // SHF_ALLOC, no SHF_WRITE → code+rodata preset accepts.
            sh_flags: u64::from(elf::SHF_ALLOC),
            // sh_addr near top of address space; sh_addr + sh_size > u64::MAX
            sh_addr: u64::MAX - 1,
            sh_offset: data_off as u64,
            sh_size: payload.len() as u64,
            sh_link: 0,
            sh_info: 0,
            sh_addralign: 1,
            sh_entsize: 0,
        });
        w.write_shstrtab_section_header();
    }
    let obj = parse(&buf);
    let err = elf_get_loadable_regions(&obj)
        .expect_err("addr+len overflow must surface as RegionOverflow");
    let msg = err.to_string();
    let expected_addr = format!("{:#x}", u64::MAX - 1);
    assert!(
        msg.contains("would overflow u64")
            && msg.contains(&expected_addr)
            && msg.contains("length 4"),
        "got: {err}",
    );
}

// ── filter-rejected malformed section is silent, not an error ────────────

/// Pinned contract: when the preset's filter rejects a section, the
/// walker must NOT call `section.data()` on it — so a malformed rejected
/// section cannot spuriously surface as a parse error from the `object`
/// crate.
///
/// Complement of `code_and_readonly_preset_propagates_data_error`:
/// that test pins "accepted-and-malformed ⇒ error"; this one pins
/// "rejected-and-malformed ⇒ empty Ok".  Together they lock in
/// filter-before-data semantics.
///
/// We use a writable PROGBITS section (rejected by the code+rodata
/// preset) at a bogus offset; the walker must skip it without reading.
#[test]
fn code_and_readonly_preset_skips_rejected_malformed_section() {
    use object::write::elf::{FileHeader, SectionHeader, Writer};
    use object::{Endianness, elf};

    let mut buf = Vec::new();
    {
        let mut w = Writer::new(Endianness::Little, true, &mut buf);
        let _null = w.reserve_null_section_index();
        let name = w.add_section_name(b".broken");
        let _sec = w.reserve_section_index();
        let _shstr = w.reserve_shstrtab_section_index();

        w.reserve_file_header();
        w.reserve_shstrtab();
        w.reserve_section_headers();

        w.write_file_header(&FileHeader {
            os_abi: elf::ELFOSABI_SYSV,
            abi_version: 0,
            // ET_REL: a section-only fixture has no PT_LOAD segments,
            // so the kind-dispatched loader walks sections (the ET_REL
            // / fallback path).  Marking the ELF as ET_EXEC would
            // route through the segments path with an empty segment
            // list, which is not what these section-walker tests aim
            // to pin.
            e_type: elf::ET_REL,
            e_machine: elf::EM_X86_64,
            e_entry: 0,
            e_flags: 0,
        })
        .expect("write file header");
        w.write_shstrtab();
        w.write_null_section_header();
        w.write_section_header(&SectionHeader {
            name: Some(name),
            sh_type: elf::SHT_PROGBITS,
            // SHF_ALLOC + SHF_WRITE → code+rodata preset REJECTS.
            sh_flags: u64::from(elf::SHF_ALLOC) | u64::from(elf::SHF_WRITE),
            sh_addr: 0x1000,
            sh_offset: 0xdead_beef, // past EOF → data() would fail if read
            sh_size: 4,
            sh_link: 0,
            sh_info: 0,
            sh_addralign: 1,
            sh_entsize: 0,
        });
        w.write_shstrtab_section_header();
    }
    let obj = parse(&buf);

    // The malformed section is writable, so the code+rodata preset
    // rejects it.  The walker must NOT call `section.data()` on a
    // rejected section, so no `object::Error` surfaces.
    let regions = elf_get_loadable_regions(&obj)
        .expect("filter-rejected malformed section must not surface an error");
    assert!(regions.is_empty(), "nothing was accepted");
}

// ── same start_addr (ET_REL) → first wins ────────────────────────────────

/// When two sections share a `sh_addr`, ET_REL's section-walker
/// applies **first-wins** VMA dedup.  Bytes for the first section
/// encountered occupy the slot; the second section's bytes are
/// dropped.
///
/// This pins the ET_REL semantics that motivate the dedup: a `.o`
/// commonly has `.text`, `.text.startup`, `.text.foo`, … all sitting
/// at VMA 0 pre-link.  Last-wins would non-deterministically swap
/// which section's bytes land at VMA 0 depending on iteration order;
/// first-wins gives a stable result.
///
/// Both sections here are non-writable PROGBITS (`.rodata`-like), so
/// the code+rodata preset accepts both.
#[test]
fn et_rel_sections_same_start_first_wins() {
    let bytes = build_elf_with_sections(&[
        SectionSpec {
            name: b".first",
            addr: 0x1000,
            data: vec![0xaa],
            exec: true,
            writable: false,
            nobits: false,
        },
        SectionSpec {
            name: b".second",
            addr: 0x1000,
            data: vec![0xbb],
            exec: false,
            writable: false,
            nobits: false,
        },
    ]);
    let obj = parse(&bytes);
    let regions = elf_get_loadable_regions(&obj).unwrap();
    // Only one region for the shared VMA — the dedup happens at the
    // collector, not at the lookup table.
    assert_eq!(regions.len(), 1);
    let table = strider_reader::MemRegionsLookupTable::new(regions);

    let mut buf = [0u8; 1];
    assert_eq!(table.read(0x1000, &mut buf), Some(1));
    assert_eq!(
        buf[0], 0xaa,
        "first section wins on duplicate start_addr (ET_REL)"
    );
}

// ── elf_get_loadable_regions_including_writable ───────────────
//
// The "allocatable" preset is the broader filter used by
// `apply_elf_relocations_autoload`: it accepts every section whose
// `sh_flags & SHF_ALLOC` is set, so it picks up writable allocatable
// sections (`.data`, `.got.plt`, `.data.rel.ro`) on top of the
// code+rodata preset's footprint.  These tests pin the shared
// section-walker contracts (NOBITS skip, data-error propagation,
// overflow propagation) via the allocatable preset's filter so a
// future filter-vs-walker refactor cannot drop one preset without the
// other catching it.

#[test]
fn allocatable_sections_include_text_rodata_data_and_exclude_bss() {
    // The allocatable preset accepts `.text`, `.rodata`, and `.data`
    // because all three carry SHF_ALLOC.  `.bss` is NOBITS so the
    // walker skips it regardless of preset.
    let bytes = build_elf_with_sections(&[
        SectionSpec::text(0x1000, vec![1, 2]),
        SectionSpec::rodata(0x2000, vec![3, 4]),
        SectionSpec::data(0x3000, vec![5, 6]),
        SectionSpec::bss(0x4000, 16),
    ]);
    let obj = parse(&bytes);
    let regions = elf_get_loadable_regions_including_writable(&obj).unwrap();

    let addrs: Vec<u64> = regions.iter().map(|r| r.start_addr()).collect();
    assert!(addrs.contains(&0x1000), ".text must be included");
    assert!(addrs.contains(&0x2000), ".rodata must be included");
    assert!(addrs.contains(&0x3000), ".data must be included");
    assert!(!addrs.contains(&0x4000), ".bss (NOBITS) must be excluded");
    assert_eq!(regions.len(), 3);
}

#[test]
fn allocatable_preset_skips_nobits() {
    // Sanity check independent of the multi-section fixture above:
    // a `.bss`-style NOBITS section produces empty `data()` and the
    // walker skips it regardless of preset.
    let bytes = build_elf_with_sections(&[
        SectionSpec::text(0x1000, vec![1, 2, 3]),
        SectionSpec::bss(0x2000, 64),
    ]);
    let obj = parse(&bytes);
    let regions = elf_get_loadable_regions_including_writable(&obj).unwrap();

    let addrs: Vec<u64> = regions.iter().map(|r| r.start_addr()).collect();
    assert!(addrs.contains(&0x1000), ".text must be present");
    assert!(!addrs.contains(&0x2000), ".bss (NOBITS) must be skipped");
}

/// Pinned contract: when an accepted section's `section.data()` fails,
/// the allocatable preset's walker propagates the `object::Error`
/// rather than silently skipping the offending section.  NOBITS
/// sections (where `data()` returns `Ok(&[])`) are the only legitimate
/// skip path; a real `Err` means the ELF is malformed and silently
/// dropping it would hand the caller a partially-loaded reader.
///
/// We synthesize the failure by pointing a writable PROGBITS section
/// (accepted by the allocatable preset, rejected by code+rodata) at a
/// file offset past the end of the buffer, which makes `section.data()`
/// return `Err`.
#[test]
fn allocatable_preset_propagates_data_error() {
    use object::write::elf::{FileHeader, SectionHeader, Writer};
    use object::{Endianness, elf};

    let mut buf = Vec::new();
    {
        let mut w = Writer::new(Endianness::Little, true, &mut buf);
        let _null = w.reserve_null_section_index();
        let name = w.add_section_name(b".broken_writable");
        let _sec = w.reserve_section_index();
        let _shstr = w.reserve_shstrtab_section_index();

        w.reserve_file_header();
        w.reserve_shstrtab();
        w.reserve_section_headers();

        w.write_file_header(&FileHeader {
            os_abi: elf::ELFOSABI_SYSV,
            abi_version: 0,
            // ET_REL: a section-only fixture has no PT_LOAD segments,
            // so the kind-dispatched loader walks sections (the ET_REL
            // / fallback path).  Marking the ELF as ET_EXEC would
            // route through the segments path with an empty segment
            // list, which is not what these section-walker tests aim
            // to pin.
            e_type: elf::ET_REL,
            e_machine: elf::EM_X86_64,
            e_entry: 0,
            e_flags: 0,
        })
        .expect("write file header");
        w.write_shstrtab();
        w.write_null_section_header();
        w.write_section_header(&SectionHeader {
            name: Some(name),
            sh_type: elf::SHT_PROGBITS,
            // SHF_ALLOC + SHF_WRITE → the allocatable preset accepts
            // (the code+rodata preset would reject).
            sh_flags: u64::from(elf::SHF_ALLOC) | u64::from(elf::SHF_WRITE),
            sh_addr: 0x1000,
            sh_offset: 0xdead_beef, // past EOF → data() must fail
            sh_size: 4,
            sh_link: 0,
            sh_info: 0,
            sh_addralign: 1,
            sh_entsize: 0,
        });
        w.write_shstrtab_section_header();
    }
    let obj = parse(&buf);
    let err = elf_get_loadable_regions_including_writable(&obj)
        .expect_err("malformed accepted section must surface an error");
    assert!(
        err.to_string().contains("failed to parse ELF"),
        "got: {err}"
    );
}

/// Pinned contract: when an accepted section's `sh_addr + sh_size`
/// would overflow `u64`, `MemRegion::new` returns an overflow error,
/// and the section walker must propagate that — *not* silently drop
/// it and not rewrap it as an `object`-crate parse error.
///
/// Complements `allocatable_preset_propagates_data_error`: that test
/// pins the `object::Error` arm of the walker's error set; this test
/// pins the overflow arm via the allocatable preset's filter.
#[test]
fn allocatable_preset_propagates_region_overflow() {
    use object::write::elf::{FileHeader, SectionHeader, Writer};
    use object::{Endianness, elf};

    let payload = [0u8, 0, 0, 0]; // 4 bytes of data on disk

    let mut buf = Vec::new();
    {
        let mut w = Writer::new(Endianness::Little, true, &mut buf);
        let _null = w.reserve_null_section_index();
        let name = w.add_section_name(b".overflow_writable");
        let _sec = w.reserve_section_index();
        let _shstr = w.reserve_shstrtab_section_index();

        w.reserve_file_header();
        let data_off = w.reserve(payload.len(), 1);
        w.reserve_shstrtab();
        w.reserve_section_headers();

        w.write_file_header(&FileHeader {
            os_abi: elf::ELFOSABI_SYSV,
            abi_version: 0,
            // ET_REL: a section-only fixture has no PT_LOAD segments,
            // so the kind-dispatched loader walks sections (the ET_REL
            // / fallback path).  Marking the ELF as ET_EXEC would
            // route through the segments path with an empty segment
            // list, which is not what these section-walker tests aim
            // to pin.
            e_type: elf::ET_REL,
            e_machine: elf::EM_X86_64,
            e_entry: 0,
            e_flags: 0,
        })
        .expect("write file header");
        w.write(&payload);
        w.write_shstrtab();
        w.write_null_section_header();
        w.write_section_header(&SectionHeader {
            name: Some(name),
            sh_type: elf::SHT_PROGBITS,
            // SHF_ALLOC + SHF_WRITE → allocatable preset accepts.
            sh_flags: u64::from(elf::SHF_ALLOC) | u64::from(elf::SHF_WRITE),
            // sh_addr near top of address space; sh_addr + sh_size > u64::MAX
            sh_addr: u64::MAX - 1,
            sh_offset: data_off as u64,
            sh_size: payload.len() as u64,
            sh_link: 0,
            sh_info: 0,
            sh_addralign: 1,
            sh_entsize: 0,
        });
        w.write_shstrtab_section_header();
    }
    let obj = parse(&buf);
    let err = elf_get_loadable_regions_including_writable(&obj)
        .expect_err("addr+len overflow must surface as RegionOverflow");
    let msg = err.to_string();
    let expected_addr = format!("{:#x}", u64::MAX - 1);
    assert!(
        msg.contains("would overflow u64")
            && msg.contains(&expected_addr)
            && msg.contains("length 4"),
        "got: {err}"
    );
}
