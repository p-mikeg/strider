#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for the top-level `reader::load_elf` function.

#[path = "common/mod.rs"]
mod common;

use std::io::Write as _;

use common::elf_fixture::simple_text_elf;
use object::{Endianness, Object, read::ObjectSection};
use reader::ErrorKind;
use tempfile::NamedTempFile;

/// Happy path: `load_elf` on a valid ELF tempfile returns a parsed
/// `object::File<'static>` with expected shape.
#[test]
fn load_elf_parses_valid_tempfile() {
    let bytes = simple_text_elf(0x1000, &[0x90]);
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(&bytes).unwrap();
    f.flush().unwrap();

    let obj = reader::load_elf(f.path().to_str().expect("utf8 path")).unwrap();
    assert_eq!(obj.endianness(), Endianness::Little);

    // `.text` is present at 0x1000.
    let sec = obj.section_by_name(".text").expect(".text section");
    assert_eq!(sec.address(), 0x1000);
}

/// A non-ELF file produces `ErrorKind::Object(_)`.
#[test]
fn load_elf_rejects_garbage_bytes() {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(b"this is definitely not an ELF file").unwrap();
    f.flush().unwrap();

    let err = reader::load_elf(f.path().to_str().expect("utf8 path")).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::Object(_)), "got {:?}", err.kind());
}

/// A missing path produces `ErrorKind::Io(_)`.
#[test]
fn load_elf_missing_path_is_io_error() {
    let err = reader::load_elf("/definitely/not/a/real/path/for/tests").unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::Io(_)), "got {:?}", err.kind());
}
