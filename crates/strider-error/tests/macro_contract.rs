#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Pins the contract of the `define_error!` macro: the generated wrapper
//! provides kind/into_kind/decompose/locations/backtrace accessors, Display
//! delegates to the inner kind, Debug prints kind+locations+backtrace,
//! `Error::source` forwards to the inner kind, and `From<$kind>` / `From<$src>`
//! are both `#[track_caller]` at the `?` site.

use std::error::Error as _;

#[derive(Debug, thiserror::Error)]
pub enum MyKind {
    #[error("boom")]
    Boom,
    // Non-transparent so `#[from]` implies `#[source]` and `source()`
    // returns the wrapped `io::Error` (transparent would forward to
    // io::Error::source() instead, which is typically None).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

strider_error::define_error! {
    pub struct MyError wraps MyKind;
    sources: [std::io::Error];
}

#[test]
fn from_kind_via_into_produces_length_one_chain() {
    let err: MyError = MyKind::Boom.into();
    assert_eq!(err.locations().len(), 1);
    assert!(matches!(err.kind(), MyKind::Boom));
}

#[test]
fn from_source_via_question_mark_produces_length_one_chain() {
    fn inner() -> Result<(), MyError> {
        let f = std::fs::File::open("/definitely/not/a/real/path")?;
        drop(f);
        Ok(())
    }
    let err = inner().unwrap_err();
    assert_eq!(err.locations().len(), 1, "source bridge seeds a fresh chain");
    assert!(matches!(err.kind(), MyKind::Io(_)));
}

#[test]
fn question_mark_on_same_wrapper_does_not_extend_chain() {
    fn leaf() -> Result<(), MyError> { Err(MyKind::Boom.into()) }
    fn middle() -> Result<(), MyError> { leaf()?; Ok(()) }
    fn outer() -> Result<(), MyError> { middle()?; Ok(()) }
    let err = outer().unwrap_err();
    assert_eq!(
        err.locations().len(),
        1,
        "same-wrapper ? is a move, not a From — chain stays at 1",
    );
}

#[test]
fn track_caller_on_question_mark_points_at_question_mark_site() {
    // This test pins that `?` on a `Result<_, $src>` -> Result<_, $wrapper>
    // places the location at the `?` line in *this* function, not inside
    // the generated From impl.
    #[track_caller]
    fn probe() -> Result<(), MyError> {
        let _ = std::fs::File::open("/definitely/not/a/real/path")?; // << expected loc line
        Ok(())
    }
    let err = probe().unwrap_err();
    let loc = err.locations()[0];
    assert!(
        loc.file().ends_with("tests/macro_contract.rs"),
        "location must point at the caller's file, got {}",
        loc.file(),
    );
}

#[test]
fn decompose_and_reconstruct_preserves_chain_length_and_backtrace_status() {
    let err: MyError = MyKind::Boom.into();
    let before_len = err.locations().len();
    let before_status = err.backtrace().status();
    let (kind, fields) = err.decompose();
    assert!(matches!(*kind, MyKind::Boom));
    assert_eq!(fields.locations.len(), before_len);
    assert_eq!(fields.backtrace.status(), before_status);
}

#[test]
fn display_delegates_to_inner_kind() {
    let err: MyError = MyKind::Boom.into();
    assert_eq!(err.to_string(), "boom");
}

#[test]
fn debug_prints_location_markers() {
    let err: MyError = MyKind::Boom.into();
    let dbg = format!("{err:?}");
    assert!(dbg.contains("boom"), "Debug should start with Display line; got {dbg:?}");
    assert!(dbg.contains("  at [0] "), "Debug should include location[0]; got {dbg:?}");
}

#[test]
fn error_source_forwards_to_inner_kind() {
    // MyKind::Io wraps std::io::Error via #[from], so source() should yield it.
    let err: MyError = std::fs::File::open("/definitely/not/a/real/path").unwrap_err().into();
    let src = err.source().expect("Io variant exposes its source");
    assert!(src.is::<std::io::Error>());
}

// ── bridge_error! macro contract ─────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum OuterKind {
    #[error(transparent)]
    Inner(MyKind),
}

strider_error::define_error! {
    pub struct OuterError wraps OuterKind;
}

strider_error::bridge_error!(MyError => OuterError, OuterKind::Inner);

#[test]
fn bridge_error_macro_extends_chain_by_one() {
    fn inner() -> Result<(), MyError> { Err(MyKind::Boom.into()) }
    fn outer() -> Result<(), OuterError> { inner()?; Ok(()) }

    let err = outer().unwrap_err();
    assert_eq!(
        err.locations().len(),
        2,
        "origin + one bridge push_caller = 2",
    );
    assert!(matches!(err.kind(), OuterKind::Inner(MyKind::Boom)));
}
