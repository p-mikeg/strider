#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for the free `elf_*_to_mem_region(s)` functions and
//! the three filter helpers in `reader::elf`.

#[path = "common/mod.rs"]
mod common;

use common::elf_fixture::{SectionSpec, build_elf_with_sections};
use object::Object;
use object::read::ObjectSection;
use reader::elf::{
    elf_get_code_and_readonly_sections_as_mem_regions,
    elf_get_executable_sections_as_mem_regions,
    elf_section_to_mem_region,
    elf_sections_to_mem_regions,
};

/// Parses the bytes as an ELF; panics with a clear message if parse fails.
fn parse(bytes: &[u8]) -> object::File<'_> {
    object::File::parse(bytes).expect("parse synthetic ELF")
}

// ── elf_section_to_mem_region (single-section round-trip) ─────────────────

#[test]
fn elf_section_to_mem_region_preserves_addr_and_data() {
    let bytes = build_elf_with_sections(&[SectionSpec::text(0x1000, vec![1, 2, 3, 4])]);
    let obj = parse(&bytes);
    let sec = obj
        .section_by_name(".text")
        .expect("find .text in synthetic ELF");

    let region = elf_section_to_mem_region(&sec).expect("convert section");
    assert_eq!(region.start_addr, 0x1000);
    assert_eq!(region.data, vec![1, 2, 3, 4]);
}

// ── elf_sections_to_mem_regions: filter is honored ────────────────────────

#[test]
fn elf_sections_to_mem_regions_filter_rejects_all() {
    let bytes = build_elf_with_sections(&[
        SectionSpec::text(0x1000, vec![1]),
        SectionSpec::rodata(0x2000, vec![2]),
    ]);
    let obj = parse(&bytes);
    let regions = elf_sections_to_mem_regions(&obj, |_| false).unwrap();
    assert!(regions.is_empty(), "filter=false must reject all");
}

#[test]
fn elf_sections_to_mem_regions_filter_selects_subset() {
    let bytes = build_elf_with_sections(&[
        SectionSpec::text(0x1000, vec![1]),
        SectionSpec::rodata(0x2000, vec![2]),
    ]);
    let obj = parse(&bytes);
    let regions = elf_sections_to_mem_regions(&obj, |sec| {
        sec.name().map(|n| n == ".text").unwrap_or(false)
    })
    .unwrap();

    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].start_addr, 0x1000);
    assert_eq!(regions[0].data, vec![1]);
}

// ── elf_sections_to_mem_regions: NOBITS sections are skipped ──────────────

/// `.bss` is `SHT_NOBITS` — `section.data()` returns empty bytes. The
/// helper treats empty-data sections as skippable regardless of filter.
///
/// Asserted positively (not by count) because the low-level helper honors
/// its filter for every section the ELF contains, including the unavoidable
/// `.shstrtab` synthetic fixtures emit. The test's subject is the NOBITS
/// skip, not the total section count.
#[test]
fn elf_sections_to_mem_regions_skips_nobits() {
    let bytes = build_elf_with_sections(&[
        SectionSpec::text(0x1000, vec![1, 2, 3]),
        SectionSpec::bss(0x2000, 64),
    ]);
    let obj = parse(&bytes);
    let regions = elf_sections_to_mem_regions(&obj, |_| true).unwrap();

    let addrs: Vec<u64> = regions.iter().map(|r| r.start_addr).collect();
    assert!(addrs.contains(&0x1000), ".text must be present");
    assert!(!addrs.contains(&0x2000), ".bss (NOBITS) must be skipped");
}

// ── elf_sections_to_mem_regions: same start_addr → last wins (via table) ──

/// When two sections share a start_addr, the helpers preserve both entries
/// in iteration order; `MemRegionsLookupTable` collapses them by its own
/// "last insert wins" rule. Read through the table to exercise the real,
/// user-visible behavior.
#[test]
fn elf_sections_same_start_last_wins_via_lookup_table() {
    let bytes = build_elf_with_sections(&[
        SectionSpec { name: b".first",  addr: 0x1000, data: vec![0xaa], exec: true,  writable: false, nobits: false },
        SectionSpec { name: b".second", addr: 0x1000, data: vec![0xbb], exec: false, writable: false, nobits: false },
    ]);
    let obj = parse(&bytes);
    let regions = elf_sections_to_mem_regions(&obj, |_| true).unwrap();
    let table = reader::MemRegionsLookupTable::new(regions);

    let mut buf = [0u8; 1];
    assert_eq!(table.read(0x1000, &mut buf), Some(1));
    assert_eq!(buf[0], 0xbb, "later section wins on duplicate start_addr");
}

// ── elf_get_executable_sections_as_mem_regions ────────────────────────────

#[test]
fn elf_exec_sections_include_shf_execinstr_and_exclude_others() {
    let bytes = build_elf_with_sections(&[
        SectionSpec::text(0x1000, vec![1]),     // SHF_EXECINSTR
        SectionSpec::rodata(0x2000, vec![2]),   // not exec
        SectionSpec::data(0x3000, vec![3]),     // not exec
    ]);
    let obj = parse(&bytes);
    let regions = elf_get_executable_sections_as_mem_regions(&obj).unwrap();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].start_addr, 0x1000);
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

    let addrs: Vec<u64> = regions.iter().map(|r| r.start_addr).collect();
    assert!(addrs.contains(&0x1000), ".text must be included");
    assert!(addrs.contains(&0x2000), ".rodata must be included");
    assert!(!addrs.contains(&0x3000), ".data must be excluded");
    assert!(!addrs.contains(&0x4000), ".bss must be excluded");
    assert_eq!(regions.len(), 2);
}

use common::elf_fixture::{SegmentSpec, build_elf_with_segments};
use object::read::ObjectSegment;
use reader::elf::{
    elf_get_executable_segments_as_mem_regions,
    elf_segment_to_mem_region,
    elf_segments_to_mem_regions,
};

// ── elf_segment_to_mem_region ─────────────────────────────────────────────

#[test]
fn elf_segment_to_mem_region_preserves_addr_and_data() {
    let bytes = build_elf_with_segments(&[SegmentSpec {
        addr: 0x1000,
        data: vec![1, 2, 3, 4],
        exec: true,
    }]);
    let obj = parse(&bytes);
    let seg = obj.segments().next().expect("at least one segment");

    let region = elf_segment_to_mem_region(&seg).expect("convert segment");
    assert_eq!(region.start_addr, 0x1000);
    assert_eq!(region.data, vec![1, 2, 3, 4]);
}

// ── elf_segments_to_mem_regions: filter honored ───────────────────────────

#[test]
fn elf_segments_to_mem_regions_filter_rejects_all() {
    let bytes = build_elf_with_segments(&[
        SegmentSpec { addr: 0x1000, data: vec![1], exec: true },
        SegmentSpec { addr: 0x2000, data: vec![2], exec: false },
    ]);
    let obj = parse(&bytes);
    let regions = elf_segments_to_mem_regions(&obj, |_| false).unwrap();
    assert!(regions.is_empty());
}

#[test]
fn elf_segments_to_mem_regions_filter_selects_exec_only() {
    let bytes = build_elf_with_segments(&[
        SegmentSpec { addr: 0x1000, data: vec![1], exec: true },
        SegmentSpec { addr: 0x2000, data: vec![2], exec: false },
    ]);
    let obj = parse(&bytes);
    let regions = elf_segments_to_mem_regions(&obj, |seg| matches!(
        seg.flags(),
        object::read::SegmentFlags::Elf { p_flags }
            if p_flags & object::elf::PF_X != 0,
    ))
    .unwrap();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].start_addr, 0x1000);
}

// ── elf_get_executable_segments_as_mem_regions ────────────────────────────

#[test]
fn elf_exec_segments_include_pf_x_and_exclude_others() {
    let bytes = build_elf_with_segments(&[
        SegmentSpec { addr: 0x1000, data: vec![1], exec: true },   // PF_X
        SegmentSpec { addr: 0x2000, data: vec![2], exec: false },  // no PF_X
    ]);
    let obj = parse(&bytes);
    let regions = elf_get_executable_segments_as_mem_regions(&obj).unwrap();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].start_addr, 0x1000);
}
