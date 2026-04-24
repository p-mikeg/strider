//! Human-readable and FFI-friendly formatting of error chains.

use std::error::Error;
use std::fmt::Write;

/// Formats an error chain for display — follows `source()` all the way
/// down, then appends the `Debug` of the top-level error (which for
/// wrappers produced by [`crate::define_error!`] includes the location
/// chain and backtrace).
///
/// The `strider-py` PyO3 layer calls this to produce the body of the
/// Python exception's string representation.
///
/// ```
/// use std::error::Error;
/// use std::fmt;
///
/// #[derive(Debug)]
/// struct Oops;
/// impl fmt::Display for Oops {
///     fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
///         f.write_str("oops")
///     }
/// }
/// impl Error for Oops {}
///
/// let s = strider_error::format_traceback(&Oops);
/// assert!(s.contains("oops"));
/// ```
pub fn format_traceback(err: &(dyn Error + 'static)) -> String {
    let mut out = String::new();
    // Writing into a String is infallible; explicit let _ silences the
    // Result from the Write trait.
    let _ = writeln!(out, "error: {err}");

    let mut cur = err.source();
    while let Some(e) = cur {
        let _ = writeln!(out, "  caused by: {e}");
        cur = e.source();
    }

    // The Debug of a `define_error!`-generated wrapper starts with its
    // Display line, which we already printed above. Strip it so the line
    // doesn't appear twice. For any other Debug impl the first line is
    // whatever Debug produced — preserving it is harmless.
    let dbg = format!("{err:?}");
    let tail = dbg.split_once('\n').map_or(dbg.as_str(), |(_first, rest)| rest);
    if !tail.is_empty() {
        let _ = writeln!(out);
        let _ = write!(out, "{tail}");
    }
    out
}
