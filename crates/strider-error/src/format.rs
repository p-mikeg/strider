//! Human-readable and FFI-friendly formatting of error chains.

use std::error::Error;
use std::fmt::Write;

use crate::Traceback;

/// Renders a [`Traceback`]-bearing error into a single string:
/// Display line, source-chain walk (one `"  caused by: "` per hop),
/// per-`?` location chain (`"  at [N] file:line:column"`), then the
/// origin backtrace.
///
/// The `strider-py` PyO3 layer calls this to produce the body of the
/// Python exception's string representation.
///
/// ```
/// #[derive(Debug, thiserror::Error)]
/// pub enum MyKind {
///     #[error("something went wrong")]
///     Oops,
/// }
///
/// strider_error::define_error! {
///     pub struct MyError wraps MyKind;
/// }
///
/// let err: MyError = MyKind::Oops.into();
/// let s = strider_error::format_traceback(&err);
/// assert!(s.starts_with("error: something went wrong"));
/// assert!(s.contains("  at [0] "));
/// ```
pub fn format_traceback(err: &(dyn Traceback + 'static)) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "error: {err}");

    for e in std::iter::successors(Error::source(err), |e| Error::source(*e)) {
        let _ = writeln!(out, "  caused by: {e}");
    }

    let _ = crate::fields::write_chain_and_backtrace(
        err.location_chain(),
        err.origin_backtrace(),
        &mut out,
    );
    out
}
