#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Pins `format_traceback` output invariants against a `Traceback` wrapper:
//!   * Display line appears exactly once (no duplication across the Debug tail).
//!   * Location markers `  at [N] ` appear for each chain entry.
//!   * Multi-line Display does not duplicate any line (regression for C1).
//!   * Source-chain walk prints outer → caused-by → inner in order.

#[derive(Debug, thiserror::Error)]
pub enum MyKind {
    #[error("unique-display-marker-7a3f")]
    Boom,
    #[error("line1\nline2-marker-8b4e")]
    MultiLine,
}

strider_error::define_error! {
    pub struct MyError wraps MyKind;
}

#[derive(Debug, thiserror::Error)]
pub enum WithSourceKind {
    #[error("outer-marker")]
    Io(#[from] std::io::Error),
}

strider_error::define_error! {
    pub struct WithSource wraps WithSourceKind;
    sources: [std::io::Error];
}

#[test]
fn format_traceback_prints_wrapper_display_exactly_once() {
    let err: MyError = MyKind::Boom.into();
    let s = strider_error::format_traceback(&err);
    let count = s.matches("unique-display-marker-7a3f").count();
    assert_eq!(
        count, 1,
        "expected the Display line once; got {count} occurrences in:\n{s}",
    );
    assert!(s.contains("  at [0] "), "locations dropped; got:\n{s}");
}

#[test]
fn format_traceback_does_not_duplicate_multiline_display() {
    // Regression for round-3 C1: the previous strip-first-line heuristic
    // duplicated every line past the first in a multi-line Display.
    let err: MyError = MyKind::MultiLine.into();
    let s = strider_error::format_traceback(&err);
    let count = s.matches("line2-marker-8b4e").count();
    assert_eq!(
        count, 1,
        "multi-line Display second line must appear once; got {count} in:\n{s}",
    );
    let first = s.matches("line1").count();
    assert_eq!(
        first, 1,
        "multi-line Display first line must appear once; got {first} in:\n{s}",
    );
}

#[test]
fn format_traceback_walks_source_chain_top_to_bottom() {
    let io_err = std::fs::File::open("/definitely/not/a/real/path").unwrap_err();
    let err: WithSource = io_err.into();
    let s = strider_error::format_traceback(&err);

    let outer_at = s.find("outer-marker").expect("outer printed");
    let caused_at = s.find("caused by:").expect("caused-by line present");
    assert!(
        outer_at < caused_at,
        "outer must precede the caused-by line; got:\n{s}",
    );
}

#[test]
fn format_traceback_includes_location_marker() {
    let err: MyError = MyKind::Boom.into();
    let s = strider_error::format_traceback(&err);
    assert!(s.contains("  at [0] "), "missing location[0] marker in:\n{s}");
    // Output must contain more than just the locations — either the
    // backtrace Display or its "disabled backtrace" placeholder follows.
    assert!(!s.trim_end().ends_with("  at [0] "), "backtrace section missing in:\n{s}");
}
