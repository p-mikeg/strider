#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Pins the contract of `dot::error::Error<E>`: chain length, Display
//! delegation, Debug contains Display-line + location markers, `From<io::Error>`
//! seeds a length-1 chain, and the whole wrapper re-derives cleanly through
//! `decompose()`. Mirrors `strider-error/tests/macro_contract.rs` for the
//! hand-rolled generic equivalent.

use std::error::Error as _;
use std::fmt;

use dot::{Error, ErrorKind};

// A minimal dumper-error stand-in: `dot::Error<E>` requires `E: Debug`,
// plus `E: Display` for the Debug impl and `E: Error + 'static` for the
// `Error::source` impl. Test fixtures satisfy all of those.
#[derive(Debug)]
struct TestDumperErr(&'static str);

impl fmt::Display for TestDumperErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "dump-err: {}", self.0)
    }
}

impl std::error::Error for TestDumperErr {}

#[test]
fn from_kind_seeds_single_location() {
    let err: Error<TestDumperErr> =
        ErrorKind::<TestDumperErr>::SvgConversionError("nope".into()).into();
    assert_eq!(err.locations().len(), 1);
    assert!(matches!(err.kind(), ErrorKind::SvgConversionError(_)));
}

#[test]
fn from_io_error_seeds_single_location() {
    fn inner() -> Result<(), Error<TestDumperErr>> {
        let f = std::fs::File::open("/definitely/not/a/real/path")?;
        drop(f);
        Ok(())
    }
    let err = inner().unwrap_err();
    assert_eq!(err.locations().len(), 1);
    assert!(matches!(err.kind(), ErrorKind::IoError(_)));
}

#[test]
fn display_delegates_to_inner_kind() {
    let err: Error<TestDumperErr> =
        ErrorKind::<TestDumperErr>::DotDumpError(TestDumperErr("xyz")).into();
    assert_eq!(err.to_string(), "dump-err: xyz");
}

#[test]
fn debug_contains_display_line_and_location_marker() {
    let err: Error<TestDumperErr> =
        ErrorKind::<TestDumperErr>::DotDumpError(TestDumperErr("xyz")).into();
    let dbg = format!("{err:?}");
    assert!(
        dbg.contains("dump-err: xyz"),
        "Debug should start with the Display line; got {dbg:?}",
    );
    assert!(
        dbg.contains("  at [0] "),
        "Debug should include the origin location; got {dbg:?}",
    );
}

#[test]
fn decompose_preserves_chain_length_and_backtrace_status() {
    let err: Error<TestDumperErr> =
        ErrorKind::<TestDumperErr>::SvgConversionError("nope".into()).into();
    let before_len = err.locations().len();
    let before_status = err.backtrace().status();
    let (kind, fields) = err.decompose();
    assert!(matches!(*kind, ErrorKind::SvgConversionError(_)));
    assert_eq!(fields.locations.len(), before_len);
    assert_eq!(fields.backtrace.status(), before_status);
}

#[test]
fn error_source_delegates_to_inner_kind() {
    // All dot ErrorKind variants that wrap an inner error use
    // `#[error(transparent)]`, so Error<E>::source() must delegate to the
    // kind's source (which in turn forwards through to the inner source).
    // Pin delegation using a dumper error that itself exposes a source.
    #[derive(Debug)]
    struct Inner;
    impl fmt::Display for Inner {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("inner") }
    }
    impl std::error::Error for Inner {}

    #[derive(Debug)]
    struct WithSource(Inner);
    impl fmt::Display for WithSource {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("outer") }
    }
    impl std::error::Error for WithSource {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> { Some(&self.0) }
    }

    let err: Error<WithSource> =
        ErrorKind::<WithSource>::DotDumpError(WithSource(Inner)).into();
    // `DotDumpError` is #[error(transparent)], so source() forwards past the
    // kind straight to WithSource::source() -> Inner.
    let src = err.source().expect("transparent kind exposes inner source");
    assert!(src.is::<Inner>(), "expected Inner, got {src:?}");
}
