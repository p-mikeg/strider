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
    let obj = obj.file();
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
