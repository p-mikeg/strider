#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for the section-walker behind
//! `elf_get_code_and_readonly_sections_as_mem_regions` and
//! `elf_get_allocatable_file_backed_sections_as_mem_regions`.
//!
//! These two presets are the only ELF → [`MemRegion`] collectors exposed
//! by `strider_reader::elf`; both go through the same private
//! `collect_sections_as_mem_regions` walker, so the propagation /
//! empty-data / overflow contracts are pinned through whichever preset's
//! filter happens to accept the synthetic section.

#[path = "common/mod.rs"]
mod common;

use common::elf_fixture::{SectionSpec, build_elf_with_sections};
use strider_reader::elf::elf_get_code_and_readonly_sections_as_mem_regions;

/// Parses the bytes as an ELF; panics with a clear message if parse fails.
fn parse(bytes: &[u8]) -> object::File<'_> {
    object::File::parse(bytes).expect("parse synthetic ELF")
}

// ── elf_get_code_and_readonly_sections_as_mem_regions ─────────────────────

#[test]
fn elf_code_and_readonly_sections_include_text_and_rodata_exclude_data_and_bss() {
    let bytes = build_elf_with_sections(&[
        SectionSpec::text(0x1000, vec![1, 2]),    // exec     → include
        SectionSpec::rodata(0x2000, vec![3, 4]),  // ro data  → include
        SectionSpec::data(0x3000, vec![5, 6]),    // writable → exclude
        SectionSpec::bss(0x4000, 16),             // NOBITS   → exclude (empty data)
    ]);
    let obj = parse(&bytes);
    let regions = elf_get_code_and_readonly_sections_as_mem_regions(&obj).unwrap();

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
    let regions = elf_get_code_and_readonly_sections_as_mem_regions(&obj).unwrap();

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
    use object::Endianness;
    use object::elf;
    use object::write::elf::{FileHeader, SectionHeader, Writer};

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
            e_type: elf::ET_EXEC,
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
    let err = elf_get_code_and_readonly_sections_as_mem_regions(&obj)
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
    use object::Endianness;
    use object::elf;
    use object::write::elf::{FileHeader, SectionHeader, Writer};

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
            e_type: elf::ET_EXEC,
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
    let err = elf_get_code_and_readonly_sections_as_mem_regions(&obj)
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
    use object::Endianness;
    use object::elf;
    use object::write::elf::{FileHeader, SectionHeader, Writer};

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
            e_type: elf::ET_EXEC,
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
    let regions = elf_get_code_and_readonly_sections_as_mem_regions(&obj)
        .expect("filter-rejected malformed section must not surface an error");
    assert!(regions.is_empty(), "nothing was accepted");
}

// ── same start_addr → last wins via lookup table ────────────────────────

/// When two sections share a start_addr, the walker preserves both
/// entries in iteration order; `MemRegionsLookupTable` collapses them by
/// its own "last insert wins" rule.  Read through the table to exercise
/// the real, user-visible behavior.
///
/// Both sections here are non-writable PROGBITS (`.rodata`-like), so the
/// code+rodata preset accepts both.
#[test]
fn code_and_readonly_sections_same_start_last_wins_via_lookup_table() {
    let bytes = build_elf_with_sections(&[
        SectionSpec { name: b".first",  addr: 0x1000, data: vec![0xaa], exec: true,  writable: false, nobits: false },
        SectionSpec { name: b".second", addr: 0x1000, data: vec![0xbb], exec: false, writable: false, nobits: false },
    ]);
    let obj = parse(&bytes);
    let regions = elf_get_code_and_readonly_sections_as_mem_regions(&obj).unwrap();
    let table = strider_reader::MemRegionsLookupTable::new(regions);

    let mut buf = [0u8; 1];
    assert_eq!(table.read(0x1000, &mut buf), Some(1));
    assert_eq!(buf[0], 0xbb, "later section wins on duplicate start_addr");
}
