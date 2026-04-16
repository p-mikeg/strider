#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

//! Verifies that errors produced by the `reader` crate carry:
//!   * a non-empty location chain (one entry per `?` / `From` boundary), and
//!   * a backtrace whose status is either `Captured` (RUST_BACKTRACE set) or
//!     `Disabled` (env var unset). Unsupported is a platform-level failure
//!     and would indicate something genuinely broken.

use std::backtrace::BacktraceStatus;

#[test]
fn load_elf_missing_path_carries_traceback() {
    let err = match reader::load_elf("/definitely/not/a/real/path/for/tests") {
        Ok(_) => return,
        Err(e) => e,
    };
    assert!(
        !err.locations().is_empty(),
        "expected at least one location in chain, got empty",
    );

    let status = err.backtrace().status();
    assert!(
        matches!(
            status,
            BacktraceStatus::Captured | BacktraceStatus::Disabled
        ),
        "backtrace status should be Captured or Disabled, got {status:?}",
    );
}

#[test]
fn not_mapped_error_carries_traceback() {
    // Construct the error directly so we don't need a real ELF.
    let err: reader::Error = reader::ErrorKind::NotMapped(0xdead_beef).into();
    assert!(!err.locations().is_empty());
    assert!(matches!(
        err.backtrace().status(),
        BacktraceStatus::Captured | BacktraceStatus::Disabled,
    ));
}
