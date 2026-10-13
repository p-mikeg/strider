//! `LoadFilter::ImmutableAndRelocatedGot`'s carve-out is keyed on a section the
//! loader really maps and really leaves writable, not on its name alone: every
//! address a carved range names stops being cut out of the read-only image, so
//! a name match over another section's bytes would serve a writable section as
//! read-only.

#[path = "common/mod.rs"]
mod common;

use common::elf_fixture::SectionSpec;
use strider_reader::elf::LoadFilter;
use strider_reader::{MemRegion, MemRegionsLookupTable};

const OPT_IN: LoadFilter = LoadFilter::ImmutableAndRelocatedGot;

fn read8(regions: &[MemRegion], addr: u64) -> Option<[u8; 8]> {
    let mut buf = [0u8; 8];
    MemRegionsLookupTable::new(regions.to_vec())
        .read_exact(addr, &mut buf)
        .ok()
        .map(|()| buf)
}

/// A `SHF_TLS` `.got` is addressed as an offset into the per-thread block, so
/// the layout leaves it at its `sh_addr` and the loader maps nothing there. Its
/// name must not carve the real `.data` that sits at that address out of the
/// writable set.
#[test]
fn a_tls_got_does_not_carve_out_the_data_at_its_address() {
    let data = vec![0x44u8; 8];
    let tls_got = |addr: u64| SectionSpec {
        name: b".got",
        addr,
        data: vec![0x11u8; 8],
        exec: false,
        writable: true,
        nobits: false,
        tls: true,
        sh_size: None,
    };
    // Two passes: the ET_REL rebase decides where `.data` lands, and the
    // crafted `.got` has to name that address.
    let probe = common::elf_fixture::build_elf_with_sections(&[
        SectionSpec::text(0, vec![0x90; 8]),
        SectionSpec::data(0, data.clone()),
        tls_got(0),
    ]);
    let at = common::section_base(&probe, ".data");
    let bytes = common::elf_fixture::build_elf_with_sections(&[
        SectionSpec::text(0, vec![0x90; 8]),
        SectionSpec::data(0, data),
        tls_got(at),
    ]);
    assert_eq!(
        common::section_base(&bytes, ".data"),
        at,
        "the crafted .got must not move .data",
    );
    assert_eq!(
        read8(&common::relocated(&bytes, OPT_IN), at),
        None,
        "`.data` is writable and nothing maps the TLS `.got` over it",
    );
}
