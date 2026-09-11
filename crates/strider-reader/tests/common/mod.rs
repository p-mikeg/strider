// Included per test file via `#[path = "common/mod.rs"] mod common;`, so each
// test crate compiles its own copy and exercises only a subset. Hence the
// blanket `dead_code` allow.

#![allow(dead_code)]

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

/// Every mapping `bytes` loads under `filter`, unrelocated.
pub(crate) fn regions(
    bytes: &[u8],
    filter: strider_reader::elf::LoadFilter,
) -> Vec<strider_reader::MemRegion> {
    try_regions(bytes, filter).unwrap()
}

/// [`regions`] with the loader's error left for the caller.
pub(crate) fn try_regions(
    bytes: &[u8],
    filter: strider_reader::elf::LoadFilter,
) -> strider_reader::Result<Vec<strider_reader::MemRegion>> {
    strider_reader::OwnedElf::parse(bytes.to_vec())?.regions(
        strider_reader::elf::RegionSource::Auto,
        filter,
        false,
    )
}

/// Every allocatable mapping of `bytes`, relocated.
pub(crate) fn load_with_relocations(bytes: &[u8]) -> Vec<strider_reader::MemRegion> {
    relocated(bytes, strider_reader::elf::LoadFilter::AllAllocatable)
}

/// The runtime-immutable image of `bytes`, relocated.
pub(crate) fn load_readonly_with_relocations(bytes: &[u8]) -> Vec<strider_reader::MemRegion> {
    relocated(bytes, strider_reader::elf::LoadFilter::ImmutableOnly)
}

fn relocated(
    bytes: &[u8],
    filter: strider_reader::elf::LoadFilter,
) -> Vec<strider_reader::MemRegion> {
    strider_reader::OwnedElf::parse(bytes.to_vec())
        .unwrap()
        .regions(strider_reader::elf::RegionSource::Auto, filter, true)
        .unwrap()
}
