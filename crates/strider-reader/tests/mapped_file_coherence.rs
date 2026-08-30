//! A file rebuilt under a live `OwnedElf` must surface as an `Err`.
//!
//! Every mutation here GROWS the file. The mapping keeps its original length,
//! so nothing these tests read can fault; shrinking it and then reading is
//! what SIGBUSes, and is exactly what the guard exists to get ahead of.

#[path = "common/mod.rs"]
mod common;

use std::io::Write as _;

use common::elf_fixture::simple_text_elf;
use strider_reader::elf::{LoadFilter, RegionSource};
use tempfile::NamedTempFile;

/// The guard only exists for a mapping, and `STRIDER_NO_MMAP=1` reads instead.
fn mapping_disabled() -> bool {
    std::env::var_os("STRIDER_NO_MMAP").is_some_and(|v| v != "0")
}

fn elf_tempfile() -> NamedTempFile {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(&simple_text_elf(0x1000, &[0x90; 16])).unwrap();
    f.flush().unwrap();
    f
}

/// Appending leaves the ELF headers intact, so nothing but the guard can be
/// what rejects the load.
fn append_junk(f: &NamedTempFile) {
    std::fs::OpenOptions::new()
        .append(true)
        .open(f.path())
        .unwrap()
        .write_all(&[0xAA; 4096])
        .unwrap();
}

#[test]
fn an_untouched_mapping_stays_valid() {
    let f = elf_tempfile();
    let elf = strider_reader::load_elf(f.path()).unwrap();
    elf.check_unchanged().unwrap();
    elf.regions(RegionSource::Auto, LoadFilter::CodeAndReadOnly, false)
        .unwrap();
}

#[test]
fn a_file_changed_under_a_live_handle_is_an_error() {
    if mapping_disabled() {
        return;
    }
    let f = elf_tempfile();
    let elf = strider_reader::load_elf(f.path()).unwrap();
    append_junk(&f);

    let err = elf.check_unchanged().unwrap_err().to_string();
    assert!(err.contains("changed on disk"), "got: {err}");
    assert!(
        err.contains(&f.path().display().to_string()),
        "the error must name the file, got: {err}"
    );
}

/// The region build is one of the two choke points: an analysis must not get a
/// region set cut from a file that is no longer the one that was mapped.
#[test]
fn building_regions_over_a_changed_file_is_an_error() {
    if mapping_disabled() {
        return;
    }
    let f = elf_tempfile();
    let elf = strider_reader::load_elf(f.path()).unwrap();
    append_junk(&f);

    let err = elf
        .regions(RegionSource::Auto, LoadFilter::CodeAndReadOnly, false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("changed on disk"), "got: {err}");
}

/// The other choke point.
#[test]
fn building_a_mem_reader_over_a_changed_file_is_an_error() {
    if mapping_disabled() {
        return;
    }
    let f = elf_tempfile();
    let elf = strider_reader::load_elf(f.path()).unwrap();
    append_junk(&f);

    let err = strider_reader::ElfFileMemReader::from_elf(&elf)
        .unwrap_err()
        .to_string();
    assert!(err.contains("changed on disk"), "got: {err}");
}

/// A file replaced wholesale -- the `mv` half of a rebuild -- is caught even
/// when the new file has the same size and, on a coarse clock, the same mtime.
#[test]
fn replacing_the_path_leaves_the_mapping_coherent() {
    if mapping_disabled() {
        return;
    }
    let f = elf_tempfile();
    let elf = strider_reader::load_elf(f.path()).unwrap();

    // A rename over the path gives that NAME a new inode. The mapping still
    // holds the old one, and its bytes are exactly what was mapped, so there
    // is nothing incoherent to report. The check follows the held fd, not the
    // path, which is also what makes it immune to the caller's `chdir`.
    let replacement = NamedTempFile::new_in(f.path().parent().unwrap()).unwrap();
    std::fs::write(replacement.path(), [0u8; 64]).unwrap();
    std::fs::rename(replacement.path(), f.path()).unwrap();

    elf.check_unchanged()
        .expect("the mapped inode did not change");
}

/// Unlinking the path does not disturb the mapping: the inode outlives the
/// name. Analysing a build-system temp file that is cleaned up mid-run used to
/// fail here.
#[test]
fn unlinking_the_path_leaves_the_mapping_coherent() {
    if mapping_disabled() {
        return;
    }
    let f = elf_tempfile();
    let elf = strider_reader::load_elf(f.path()).unwrap();
    std::fs::remove_file(f.path()).unwrap();

    elf.check_unchanged()
        .expect("the mapped inode did not change");
}

/// drvfs truncates mtime to whole seconds on `utimensat`, so re-applying a
/// file's own timestamp changes it. Comparing at second granularity keeps that
/// from reporting a change on identical bytes.
#[test]
fn reapplying_the_files_own_mtime_is_not_a_change() {
    if mapping_disabled() {
        return;
    }
    let f = elf_tempfile();
    let elf = strider_reader::load_elf(f.path()).unwrap();
    let mtime = std::fs::metadata(f.path()).unwrap().modified().unwrap();
    std::fs::File::open(f.path())
        .unwrap()
        .set_modified(mtime)
        .unwrap();

    elf.check_unchanged()
        .expect("re-applying the same mtime is not a change");
}

/// Bytes that were copied rather than mapped -- what `STRIDER_NO_MMAP=1` and
/// `OwnedElf::parse` produce -- cannot tear, so they never fail the check. The
/// env var itself is process-global and is not set here, which would race the
/// other tests in this binary.
#[test]
fn owned_bytes_are_always_coherent() {
    let elf = strider_reader::OwnedElf::parse(simple_text_elf(0x1000, &[0x90; 16])).unwrap();
    elf.check_unchanged().unwrap();
    elf.checked_file().unwrap();
    let regions = elf
        .regions(RegionSource::Auto, LoadFilter::CodeAndReadOnly, false)
        .unwrap();
    regions[0].check_unchanged().unwrap();
    strider_reader::MemRegionsLookupTable::new(regions)
        .check_unchanged()
        .unwrap();
}

/// Parsing the image reads its headers out of the mapping, so every accessor
/// built on it -- entry point, symbol table, header flags -- needs the guard
/// as much as the region build does.
#[test]
fn parsing_a_changed_file_is_an_error() {
    if mapping_disabled() {
        return;
    }
    let f = elf_tempfile();
    let elf = strider_reader::load_elf(f.path()).unwrap();
    append_junk(&f);

    let err = elf.checked_file().unwrap_err().to_string();
    assert!(err.contains("changed on disk"), "got: {err}");
}

/// Regions cut before the change are still live afterwards, and reading one is
/// what faults; the table-level check is how a holder of those regions gets an
/// `Err` first.
#[test]
fn a_region_table_over_a_changed_file_is_an_error() {
    if mapping_disabled() {
        return;
    }
    let f = elf_tempfile();
    let elf = strider_reader::load_elf(f.path()).unwrap();
    let regions = elf
        .regions(RegionSource::Auto, LoadFilter::CodeAndReadOnly, false)
        .unwrap();
    let region = regions[0].clone();
    let table = strider_reader::MemRegionsLookupTable::new(regions);
    let reader = strider_reader::ElfFileMemReader::from_elf(&elf).unwrap();
    append_junk(&f);

    for err in [
        region.check_unchanged().unwrap_err().to_string(),
        table.check_unchanged().unwrap_err().to_string(),
        reader.check_unchanged().unwrap_err().to_string(),
    ] {
        assert!(err.contains("changed on disk"), "got: {err}");
    }
}
