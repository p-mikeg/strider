#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Comprehensive error-path tests — every `ErrorKind` variant, every
//! `From` conversion, and the traceback invariants.

#[path = "common/mod.rs"]
mod common;

use std::backtrace::BacktraceStatus;

use common::elf_fixture::simple_text_elf;
use reader::{ElfFileMemReader, Error, ErrorKind};

fn assert_has_traceback(err: &Error) {
    assert!(!err.locations().is_empty(), "location chain is empty");
    let s = err.backtrace().status();
    assert!(
        matches!(s, BacktraceStatus::Captured | BacktraceStatus::Disabled),
        "unexpected backtrace status: {s:?}",
    );
}

// ── Direct construction of each variant ───────────────────────────────────

#[test]
fn not_mapped_carries_traceback_and_address() {
    let err: Error = ErrorKind::NotMapped(0xdead_beef).into();
    assert_has_traceback(&err);
    assert!(err.to_string().contains("0xdeadbeef"), "display: {err}");
    assert!(matches!(err.kind(), ErrorKind::NotMapped(addr) if *addr == 0xdead_beef));
}

// ── From<io::Error> path ──────────────────────────────────────────────────

#[test]
fn load_elf_missing_path_produces_io_error_variant() {
    let err = reader::load_elf("/definitely/not/a/real/path").unwrap_err();
    assert_has_traceback(&err);
    assert!(matches!(err.kind(), ErrorKind::Io(_)), "got {:?}", err.kind());
}

#[test]
fn elf_reader_from_path_missing_produces_io_error_variant() {
    let err = ElfFileMemReader::from_path("/definitely/not/a/real/path").unwrap_err();
    assert_has_traceback(&err);
    assert!(matches!(err.kind(), ErrorKind::Io(_)), "got {:?}", err.kind());
}

// ── From<object::Error> path ──────────────────────────────────────────────

#[test]
fn elf_reader_from_bytes_garbage_produces_object_error_variant() {
    let err = ElfFileMemReader::from_bytes(b"not an elf at all").unwrap_err();
    assert_has_traceback(&err);
    assert!(matches!(err.kind(), ErrorKind::Object(_)), "got {:?}", err.kind());
}

// ── ? propagation: chain length contract ──────────────────────────────────

/// Pinned contract: `?` on a Result<T, Error> does NOT extend the location
/// chain — the chain is seeded once, at the `From<ErrorKind> for Error`
/// boundary, and same-error-type propagation is a bitwise move (no From
/// invoked). Cross-crate bridges explicitly call `push_caller` to extend.
#[test]
fn question_mark_propagation_preserves_single_location() {
    fn inner() -> Result<(), Error> {
        Err::<(), Error>(ErrorKind::NotMapped(0).into())?;
        Ok(())
    }
    fn outer() -> Result<(), Error> {
        inner()?;
        Ok(())
    }
    let err = outer().unwrap_err();
    assert_eq!(
        err.locations().len(),
        1,
        "chain should have exactly 1 entry (seeded by .into()); got {}",
        err.locations().len(),
    );
}

// ── Positive case: loading a valid ELF does NOT error ─────────────────────

#[test]
fn elf_reader_from_bytes_valid_returns_ok() {
    let bytes = simple_text_elf(0x1000, &[0x90]);
    ElfFileMemReader::from_bytes(&bytes).expect("valid synthetic ELF parses");
}
