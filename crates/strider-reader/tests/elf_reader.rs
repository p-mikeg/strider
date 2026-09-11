#[path = "common/mod.rs"]
mod common;

use std::io::Write as _;

use common::elf_fixture::simple_text_elf;
use common::reader_contract::{
    assert_mem_reader_partial_read_ok, assert_mem_reader_reads,
    assert_mem_reader_unmapped_is_not_mapped_error, assert_readonly_errors, assert_readonly_reads,
};
use rsleigh::{MemReader, VnAddr, VnSpace};
use strider_reader::{ElfFileMemReader, ReadOnlyMemory};
use tempfile::NamedTempFile;

/// A reader over synthetic ELF bytes, copying them the way `from_object`
/// does.
fn reader(bytes: &[u8]) -> ElfFileMemReader {
    let obj = object::File::parse(bytes).expect("parse synthetic ELF");
    ElfFileMemReader::from_object(&obj).expect("from_object")
}

fn read_raw(r: &ElfFileMemReader, addr: u64, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    ReadOnlyMemory::read(r, addr, &mut buf).expect("ReadOnlyMemory::read");
    buf
}

/// The fixture builder produces parseable bytes, and the reader reflects the
/// single `.text` section at the chosen address, raw and unswapped.
#[test]
fn simple_text_elf_fixture_round_trips_through_elf_reader() {
    let elf = simple_text_elf(0x1000, &[0xaa, 0xbb, 0xcc, 0xdd]);
    let r = reader(&elf);

    assert_eq!(read_raw(&r, 0x1000, 4), &[0xaa, 0xbb, 0xcc, 0xdd]);
}

/// Bytes come back verbatim regardless of endianness.
#[test]
fn ro_read_fills_raw_bytes() {
    let elf = simple_text_elf(0x1000, &[0x01, 0x02, 0x03, 0x04]);
    let r = reader(&elf);
    assert_eq!(read_raw(&r, 0x1000, 4), &[0x01, 0x02, 0x03, 0x04]);
    assert_eq!(read_raw(&r, 0x1001, 2), &[0x02, 0x03]);
    let mut empty: [u8; 0] = [];
    ReadOnlyMemory::read(&r, 0x1000, &mut empty).unwrap();
}

/// All-or-nothing: a range supplying only a prefix, one at an unmapped address
/// and one that starts mapped but runs past the end must each error, not
/// truncate.
#[test]
fn ro_read_errors_unless_the_whole_range_is_mapped() {
    let elf = simple_text_elf(0x1000, &[1, 2, 3, 4]);
    let r = reader(&elf);
    for (addr, len) in [(0x1002, 4), (0x9000, 4), (0x1000, 8)] {
        assert_readonly_errors(&r, addr, len);
    }
}

/// The two traits treat short reads differently by design: `MemReader` returns
/// `Ok(n)` with `n < buf.len()`, `ReadOnlyMemory` errors.
///
/// `ReadOnlyMemory` backs `LoadReadOnly`, which must never synthesize a
/// constant from partial bytes. `MemReader` backs Sleigh instruction fetch,
/// where a short read at a section's end is expected.
#[test]
fn elf_reader_partial_read_asymmetry_between_traits() {
    let elf = simple_text_elf(0x1000, &[1, 2, 3, 4]);
    let r = reader(&elf);

    let mut buf = [0u8; 8];
    let n = MemReader::read(
        &r,
        VnAddr {
            off: 0x1000,
            space: VnSpace::RAM,
        },
        &mut buf,
    )
    .expect("MemReader read");
    assert_eq!(n, 4, "MemReader permits partial reads");
    assert_eq!(&buf[..4], &[1, 2, 3, 4]);

    assert_readonly_errors(&r, 0x1000, 8);
}

#[test]
fn elf_reader_satisfies_mem_reader_contract() {
    let elf = simple_text_elf(0x1000, &[0x11, 0x22, 0x33, 0x44]);
    let r = reader(&elf);

    assert_mem_reader_reads(&r, 0x1000, &[0x11, 0x22, 0x33, 0x44]);
    assert_mem_reader_unmapped_is_not_mapped_error(&r, 0x9000);
    // Partial: ask 6, get 4.
    assert_mem_reader_partial_read_ok(&r, 0x1000, 6, 4);
}

#[test]
fn elf_reader_satisfies_read_only_memory_contract() {
    let elf = simple_text_elf(0x1000, &[0x11, 0x22, 0x33, 0x44]);
    let r = reader(&elf);

    assert_readonly_reads(&r, 0x1000, &[0x11, 0x22, 0x33, 0x44]);
    assert_readonly_errors(&r, 0x9000, 4);
}

/// `from_object` copies the mappings and `from_elf` windows into the ELF's own
/// bytes, so the two must serve the same image.
#[test]
fn from_object_and_from_elf_serve_the_same_bytes() {
    let elf = simple_text_elf(0x1000, &[1, 2, 3, 4]);
    let copied = reader(&elf);
    let windowed =
        ElfFileMemReader::from_elf(&strider_reader::OwnedElf::parse(elf).expect("parse")).unwrap();

    for addr in [0x1000u64, 0x1001, 0x1002, 0x1003] {
        assert_eq!(
            read_raw(&copied, addr, 1),
            read_raw(&windowed, addr, 1),
            "read mismatch at {addr:#x}",
        );
    }
}

/// The mapping path: the ELF comes off disk rather than out of a `Vec`, which
/// is where the `stat` identity check runs.
#[test]
fn elf_reader_over_a_mapped_temp_elf() {
    let elf = simple_text_elf(0x1000, &[0xde, 0xad, 0xbe, 0xef]);
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(&elf).unwrap();
    f.flush().unwrap();

    let owned = strider_reader::load_elf(f.path()).unwrap();
    let r = ElfFileMemReader::from_elf(&owned).unwrap();
    assert_eq!(read_raw(&r, 0x1000, 4), &[0xde, 0xad, 0xbe, 0xef]);
}

/// This reader is file-initial whatever the image's relocations say; the
/// relocated view of the same site is a `regions(.., relocate)` load.
#[test]
fn the_reader_serves_a_relocation_site_unpatched() {
    let fx = common::elf_fixture::build_rel_elf_placed(
        common::elf_fixture::RelOpts {
            endian: object::Endianness::Big,
            is_64: false,
            e_machine: object::elf::EM_MIPS,
            r_type: object::elf::R_MIPS_REL32,
            defined_symbol: true,
            slot_init: vec![0u8; 4],
        },
        // The fetch image is code and read-only mappings, so the site has to
        // sit in one to be visible through this reader at all.
        common::elf_fixture::RelPlacement {
            slot_exec: true,
            ..Default::default()
        },
    );
    let elf = strider_reader::OwnedElf::parse(fx.bytes.clone()).expect("parse");

    assert_eq!(
        read_raw(&ElfFileMemReader::from_elf(&elf).unwrap(), fx.slot_addr, 4),
        vec![0u8; 4],
        "the reader serves the file-initial bytes"
    );

    let relocated = strider_reader::MemRegionsLookupTable::new(
        elf.regions(
            strider_reader::elf::RegionSource::Auto,
            strider_reader::elf::LoadFilter::CodeAndReadOnly,
            true,
        )
        .expect("relocated regions"),
    );
    let mut got = [0u8; 4];
    relocated.read_exact(fx.slot_addr, &mut got).expect("site");
    assert_eq!(
        got,
        (fx.sym_addr as u32).to_be_bytes(),
        "the same load with relocations applied serves S + A"
    );
}

/// A writable PT_LOAD over an accepted read-only one is dropped by the fetch
/// filter before it can become a region, so the only trace of it is the range
/// it claimed. Without that range the `ReadOnlyMemory` view serves the RX
/// mapping's file-initial bytes for addresses that are RW at runtime, and
/// `LoadReadOnly` folds a store-then-reload there to a stale byte.
#[test]
fn a_filtered_out_writable_mapping_still_bars_the_read_only_view() {
    use object::elf::{PF_R, PF_W, PF_X};
    let base = common::elf_fixture::EQUAL_VADDR_LOAD_BASE;
    let elf = common::elf_fixture::build_overlapping_loads_elf(&[
        (PF_R | PF_X, 0x100, 0xaa),
        (PF_R | PF_W, 0x80, 0xbb),
    ]);
    let r = reader(&elf);

    // The RX mapping is the one that got loaded, so fetch still works over it.
    assert_mem_reader_reads(&r, base, &[0xaa; 4]);
    assert_mem_reader_reads(&r, base + 0x7e, &[0xaa; 4]);

    for addr in [base, base + 0x10, base + 0x7c] {
        let mut buf = [0u8; 4];
        assert!(
            ReadOnlyMemory::read(&r, addr, &mut buf).is_err(),
            "{addr:#x} is inside a writable PT_LOAD"
        );
    }
    assert_eq!(
        read_raw(&r, base + 0x80, 4),
        &[0xaa; 4],
        "past the writable mapping's end the image is read-only again"
    );
}
