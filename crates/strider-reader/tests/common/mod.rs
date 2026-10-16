// Included per test file via `#[path = "common/mod.rs"] mod common;`, so each
// test crate compiles its own copy and exercises only a subset. Hence the
// blanket `dead_code` allow.

#![allow(dead_code)]

pub(crate) mod elf_fixture;
pub(crate) mod reader_contract;
pub(crate) mod write_object;

use std::path::PathBuf;

/// `fixtures/out`, where `make -C fixtures` writes the built images.
pub(crate) fn fixtures_out() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/out")
}

/// Every built `fixtures/out/<arch>/*.{elf,o}`, sorted; empty when nothing
/// is built.
pub(crate) fn fixture_images() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for arch in std::fs::read_dir(fixtures_out())
        .into_iter()
        .flatten()
        .flatten()
    {
        let arch = arch.path();
        if !arch.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(&arch)
            .expect("an arch directory")
            .flatten()
        {
            let path = file.path();
            if path.extension().is_some_and(|e| e == "elf" || e == "o") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// `ranges` sorted, with everything overlapping or touching merged.
pub(crate) fn merge_ranges(mut ranges: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    ranges.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
    for (lo, hi) in ranges {
        match out.last_mut() {
            Some(last) if lo <= last.1 => last.1 = last.1.max(hi),
            _ => out.push((lo, hi)),
        }
    }
    out
}

/// Every region's extent, ascending and merged.
pub(crate) fn spans(regions: &[strider_reader::MemRegion]) -> Vec<(u64, u64)> {
    merge_ranges(
        regions
            .iter()
            .map(|r| (r.start_addr(), r.end_addr()))
            .collect(),
    )
}

/// Everything a region maps, read through the public API so relocation patches
/// land.
pub(crate) fn region_bytes(r: &strider_reader::MemRegion) -> Vec<u8> {
    let mut out = vec![0u8; (r.end_addr() - r.start_addr()) as usize];
    if !out.is_empty() {
        assert_eq!(r.read(r.start_addr(), &mut out), Some(out.len()));
    }
    out
}

/// Every mapping of the ELF at `path`, under `filter`, relocated or not.
pub(crate) fn regions_of_file(
    path: &std::path::Path,
    filter: strider_reader::elf::LoadFilter,
    relocate: bool,
) -> Vec<strider_reader::MemRegion> {
    strider_reader::load_elf(path)
        .expect("load_elf")
        .regions(strider_reader::elf::RegionSource::Auto, filter, relocate)
        .expect("regions")
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

/// Every mapping `bytes` loads under `filter`, with its relocations applied.
pub(crate) fn relocated(
    bytes: &[u8],
    filter: strider_reader::elf::LoadFilter,
) -> Vec<strider_reader::MemRegion> {
    strider_reader::OwnedElf::parse(bytes.to_vec())
        .unwrap()
        .regions(strider_reader::elf::RegionSource::Auto, filter, true)
        .unwrap()
}

/// Where `bytes`' section `name` is loaded. An ET_REL is rebased off a
/// synthetic image base, so a declared `sh_addr` is not the loaded address.
pub(crate) fn section_base(bytes: &[u8], name: &str) -> u64 {
    use object::{Object as _, ObjectSection as _};
    let obj = object::File::parse(bytes).expect("parse ELF");
    let sec = obj
        .sections()
        .find(|s| s.name() == Ok(name))
        .unwrap_or_else(|| panic!("no section {name}"));
    strider_reader::elf::ElfSectionLayout::new(&obj).section_base(&sec)
}
