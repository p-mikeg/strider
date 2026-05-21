#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for the top-level `strider_reader::load_elf` function.

#[path = "common/mod.rs"]
mod common;

use std::io::Write as _;

use common::elf_fixture::simple_text_elf;
use object::{Endianness, Object, read::ObjectSection};
use tempfile::NamedTempFile;

/// Happy path: `load_elf` on a valid ELF tempfile returns a parsed
/// `object::File<'static>` with expected shape.
#[test]
fn load_elf_parses_valid_tempfile() {
    let bytes = simple_text_elf(0x1000, &[0x90]);
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(&bytes).unwrap();
    f.flush().unwrap();

    let obj = strider_reader::load_elf(f.path()).unwrap();
    assert_eq!(obj.endianness(), Endianness::Little);

    // `.text` is present at 0x1000.
    let sec = obj.section_by_name(".text").expect(".text section");
    assert_eq!(sec.address(), 0x1000);
}

/// Pinned contract: when `load_elf` is given a file that exists on disk
/// but whose contents do NOT parse as a valid ELF, the function returns
/// an error whose message identifies it as an ELF-parse failure rather
/// than panicking, succeeding, or silently swallowing the error.
///
/// This test cannot directly assert that the file bytes are not leaked
/// when parse fails — `load_elf`'s entire success path leaks
/// intentionally. The bytes-aren't-leaked-on-error behavior introduced
/// in `fix(reader): validate ELF before leaking bytes in load_elf` is
/// pinned by the function's parse-before-leak structure; catching a
/// regression there would require a Miri or allocator-instrumentation
/// harness — outside the scope of this in-tree test.
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

/// A missing path produces an I/O error.
#[test]
fn load_elf_missing_path_is_io_error() {
    let err = strider_reader::load_elf("/definitely/not/a/real/path/for/tests").unwrap_err();
    assert!(
        err.to_string().contains("failed to read file"),
        "got: {err}",
    );
}
