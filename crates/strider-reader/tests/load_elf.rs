#[path = "common/mod.rs"]
mod common;

use std::io::Write as _;

use common::elf_fixture::simple_text_elf;
use object::read::ObjectSection;
use object::{Endianness, Object};
use tempfile::NamedTempFile;

#[test]
fn load_elf_parses_valid_tempfile() {
    let bytes = simple_text_elf(0x1000, &[0x90]);
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(&bytes).unwrap();
    f.flush().unwrap();

    let obj = strider_reader::load_elf(f.path()).unwrap();
    let obj = obj.checked_file().expect("the mapped file is unchanged");
    assert_eq!(obj.endianness(), Endianness::Little);

    let sec = obj.section_by_name(".text").expect(".text section");
    assert_eq!(sec.address(), 0x1000);
}

/// A file that exists but doesn't parse must surface an ELF-parse error, not a
/// panic, a success, or a swallowed error.
#[test]
fn load_elf_rejects_garbage_bytes() {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(b"this is definitely not an ELF file").unwrap();
    f.flush().unwrap();

    let err = strider_reader::load_elf(f.path()).unwrap_err();
    assert!(
        err.to_string().contains("failed to parse ELF"),
        "got: {err}",
    );
}

#[test]
fn load_elf_missing_path_is_io_error() {
    let err = strider_reader::load_elf("/definitely/not/a/real/path/for/tests").unwrap_err();
    assert!(
        err.to_string().contains("failed to read file"),
        "got: {err}",
    );
}

/// A directory stats fine and is not a regular file. The same guard is what
/// stops a FIFO, whose read-only open blocks until a writer appears.
#[test]
fn load_elf_on_a_non_regular_file_is_an_error() {
    let err = strider_reader::load_elf("/tmp").unwrap_err();
    assert!(err.to_string().contains("not a regular file"), "got: {err}",);
}

/// A regular file that stats as empty yet reads for hundreds of GiB: the
/// fallback read stops at the stat'd size, as a mapping does.
#[cfg(target_os = "linux")]
#[test]
fn load_elf_reads_no_further_than_the_stat_size() {
    let err = strider_reader::load_elf("/proc/self/pagemap").unwrap_err();
    assert!(
        err.to_string().contains("failed to parse ELF"),
        "got: {err}"
    );
}

/// An image is the region sets its mode names, from one parse.
#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "release only: loads every fixture six ways"
)]
fn an_image_is_the_region_sets_its_mode_names() {
    use strider_reader::elf::{LoadFilter as F, RegionSource, RelocationMode as M};
    let files = common::fixture_images();
    assert!(!files.is_empty(), "no fixtures; run `make -C fixtures`");
    for path in files {
        let elf = strider_reader::load_elf(&path).expect("load");
        for source in [RegionSource::Auto, RegionSource::Sections] {
            for (mode, fetch, rom, relocate) in [
                (M::Unapplied, F::CodeAndReadOnly, F::ImmutableOnly, false),
                (
                    M::Applied { got_is_rom: false },
                    F::AllAllocatable,
                    F::ImmutableOnly,
                    true,
                ),
                (
                    M::Applied { got_is_rom: true },
                    F::AllAllocatable,
                    F::ImmutableAndRelocatedGot,
                    true,
                ),
            ] {
                let image = elf.image(source, mode).expect("image");
                let regions = |filter| {
                    format!(
                        "{:?}",
                        elf.regions(source, filter, relocate).expect("regions")
                    )
                };
                assert_eq!(
                    format!("{:?}", image.fetch),
                    regions(fetch),
                    "{path:?} {mode:?}"
                );
                assert_eq!(
                    format!("{:?}", image.rom),
                    regions(rom),
                    "{path:?} {mode:?}"
                );
            }
        }
    }
}
