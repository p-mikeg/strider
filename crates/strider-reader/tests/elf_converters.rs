#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Both loadable-region presets share one section walker, so the propagation /
//! empty-data / overflow contracts are pinned through whichever preset's filter
//! accepts the synthetic section under test.

#[path = "common/mod.rs"]
mod common;

use common::elf_fixture::{SectionSpec, build_elf_with_sections};
use strider_reader::elf::{elf_get_loadable_regions, elf_get_loadable_regions_including_writable};

fn parse(bytes: &[u8]) -> object::File<'_> {
    object::File::parse(bytes).expect("parse synthetic ELF")
}

#[test]
fn elf_code_and_readonly_sections_include_text_and_rodata_exclude_data_and_bss() {
    let bytes = build_elf_with_sections(&[
        SectionSpec::text(0x1000, vec![1, 2]),   // exec     -> include
        SectionSpec::rodata(0x2000, vec![3, 4]), // ro data  -> include
        SectionSpec::data(0x3000, vec![5, 6]),   // writable -> exclude
        SectionSpec::bss(0x4000, 16),            // NOBITS   -> exclude (empty data)
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

/// `SHT_NOBITS` yields empty `data()`, and the walker skips empty-data sections
/// whatever the filter says.
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

/// A failing `section.data()` on an accepted section must propagate, not skip.
/// NOBITS (`Ok(&[])`) is the only legitimate skip path; a real `Err` means a
/// malformed ELF, and dropping it silently would hand back a partially-loaded
/// reader.
///
/// The failure is synthesised by pointing a non-writable PROGBITS section past
/// the end of the buffer.
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
            // A section-only fixture has no PT_LOAD segments, so ET_REL is
            // what routes the loader down the section-walker path. ET_EXEC
            // would take the segments path with an empty segment list.
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
            sh_flags: u64::from(elf::SHF_ALLOC),
            sh_addr: 0x1000,
            sh_offset: 0xdead_beef, // past EOF -> data() must fail
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

/// An `sh_addr + sh_size` overflow must propagate as `MemRegion::new`'s
/// overflow error, neither dropped nor rewrapped as an `object` parse error.
/// With the sibling data-error test this enumerates every error path the walker
/// can take.
///
/// `sh_addr` is `u64::MAX - 1` with `sh_size` 4: the data fits in the file, so
/// no `object::Error`, but `addr + len` overflows by 3.
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
            // A section-only fixture has no PT_LOAD segments, so ET_REL is
            // what routes the loader down the section-walker path. ET_EXEC
            // would take the segments path with an empty segment list.
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
            sh_flags: u64::from(elf::SHF_ALLOC),
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

/// Filter-before-data: a rejected section must never have `data()` called on
/// it, so a malformed rejected section cannot surface as a spurious parse
/// error. The sibling test pins accepted-and-malformed as an error; this pins
/// rejected-and-malformed as an empty `Ok`.
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
            // A section-only fixture has no PT_LOAD segments, so ET_REL is
            // what routes the loader down the section-walker path. ET_EXEC
            // would take the segments path with an empty segment list.
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
            // Writable, so the code+rodata preset rejects it.
            sh_flags: u64::from(elf::SHF_ALLOC) | u64::from(elf::SHF_WRITE),
            sh_addr: 0x1000,
            sh_offset: 0xdead_beef, // past EOF -> data() would fail if read
            sh_size: 4,
            sh_link: 0,
            sh_info: 0,
            sh_addralign: 1,
            sh_entsize: 0,
        });
        w.write_shstrtab_section_header();
    }
    let obj = parse(&buf);

    // Writable, so the code+rodata preset rejects it before reading.
    let regions = elf_get_loadable_regions(&obj)
        .expect("filter-rejected malformed section must not surface an error");
    assert!(regions.is_empty(), "nothing was accepted");
}

/// Two sections sharing a `sh_addr` resolve first-wins.
///
/// A `.o` commonly has `.text`, `.text.startup`, `.text.foo` all at VMA 0
/// pre-link. Last-wins would pick between them by iteration order, which is
/// non-deterministic; first-wins is stable.
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
    // One region for the shared VMA: dedup happens in the collector, not the
    // lookup table.
    assert_eq!(regions.len(), 1);
    let table = strider_reader::MemRegionsLookupTable::new(regions);

    let mut buf = [0u8; 1];
    assert_eq!(table.read(0x1000, &mut buf), Some(1));
    assert_eq!(
        buf[0], 0xaa,
        "first section wins on duplicate start_addr (ET_REL)"
    );
}

// The allocatable preset accepts every `SHF_ALLOC` section, so it adds
// `.data` / `.got.plt` / `.data.rel.ro` to the code+rodata footprint. These
// re-pin the shared walker contracts through its filter, so a future
// filter-vs-walker refactor cannot drop one preset unnoticed.

#[test]
fn allocatable_sections_include_text_rodata_data_and_exclude_bss() {
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
    // Independent of the multi-section fixture above.
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

/// The data-error propagation contract, re-pinned through the allocatable
/// filter: the section here is writable, so only this preset accepts it.
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
            // A section-only fixture has no PT_LOAD segments, so ET_REL is
            // what routes the loader down the section-walker path. ET_EXEC
            // would take the segments path with an empty segment list.
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
            // Accepted by the allocatable preset, rejected by code+rodata.
            sh_flags: u64::from(elf::SHF_ALLOC) | u64::from(elf::SHF_WRITE),
            sh_addr: 0x1000,
            sh_offset: 0xdead_beef, // past EOF -> data() must fail
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

/// The overflow propagation contract, re-pinned through the allocatable filter.
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
            // A section-only fixture has no PT_LOAD segments, so ET_REL is
            // what routes the loader down the section-walker path. ET_EXEC
            // would take the segments path with an empty segment list.
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
            sh_flags: u64::from(elf::SHF_ALLOC) | u64::from(elf::SHF_WRITE),
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
