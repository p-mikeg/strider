// Included per test file via `#[path = "common/mod.rs"] mod common;`, so each
// test crate compiles its own copy and exercises only a subset. Hence the
// blanket `dead_code` allow.

#![allow(dead_code)]
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

pub(crate) mod elf_fixture;
pub(crate) mod reader_contract;

/// Everything a region maps, read through the public API so relocation patches
/// land.
pub(crate) fn region_bytes(r: &strider_reader::MemRegion) -> Vec<u8> {
    let mut out = vec![0u8; (r.end_addr() - r.start_addr()) as usize];
    if !out.is_empty() {
        assert_eq!(r.read(r.start_addr(), &mut out), Some(out.len()));
    }
    out
}

/// Every allocatable mapping of `obj`, relocated.
pub(crate) fn load_with_relocations(obj: &object::File<'_>) -> Vec<strider_reader::MemRegion> {
    relocated(
        strider_reader::elf::elf_get_loadable_regions_including_writable(obj).unwrap(),
        obj,
        strider_reader::elf::LoadFilter::AllAllocatable,
    )
}

/// The runtime-immutable image of `obj`, relocated.
pub(crate) fn load_readonly_with_relocations(
    obj: &object::File<'_>,
) -> Vec<strider_reader::MemRegion> {
    relocated(
        strider_reader::elf::elf_get_readonly_regions(obj).unwrap(),
        obj,
        strider_reader::elf::LoadFilter::ImmutableOnly,
    )
}

fn relocated(
    mut regions: Vec<strider_reader::MemRegion>,
    obj: &object::File<'_>,
    filter: strider_reader::elf::LoadFilter,
) -> Vec<strider_reader::MemRegion> {
    strider_reader::elf::apply_elf_relocations(&mut regions, obj, filter).unwrap();
    regions
}
