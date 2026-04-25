//! Core data types shared across `strider-error` wrappers.
//!
//! - [`ErrorFields`] — backtrace + per-`?` location chain.
//! - [`LocationChain`] — type alias for the chain vector.
//! - [`Traceback`] — trait implemented by every wrapper so
//!   [`crate::format_traceback`] can render locations/backtrace without
//!   inspecting `Debug` output.

use std::backtrace::Backtrace;
use std::panic::Location;

/// A chain of `Location::caller()` entries captured at every `?` /
/// `From::from` boundary an error crossed on its way up the stack.
///
/// The chain grows newest-last: the first entry is where the error was
/// first wrapped, the last entry is the outermost `?`. Reading the chain
/// from index 0 → N gives the propagation path from origin to top.
pub type LocationChain = Vec<&'static Location<'static>>;

/// Shared payload carried by every crate's `Error` wrapper struct.
///
/// The backtrace is heap-allocated in a `Box` for a stable 1-pointer
/// footprint. Backtraces are never cloned — `decompose`, `push_caller`,
/// and every cross-crate bridge move the whole `ErrorFields` by value.
pub struct ErrorFields {
    /// Backtrace captured at the point the error was first constructed.
    /// `Backtrace::capture()` respects `RUST_BACKTRACE`; when unset, it
    /// returns a `Disabled` status and carries no frames (cheap).
    pub backtrace: Box<Backtrace>,
    /// Per-`?` propagation chain. See type docs.
    pub locations: LocationChain,
}

impl ErrorFields {
    /// Captures a fresh backtrace and seeds the location chain with the
    /// caller's site. Called once, at the outermost `From<ErrorKind>
    /// for Error` boundary when an error originates.
    // No Default impl: callers never want an un-tracked fields payload;
    // every construction must go through `new()` so `#[track_caller]` lands.
    #[allow(clippy::new_without_default)]
    #[must_use]
    #[track_caller]
    pub fn new() -> Self {
        Self {
            backtrace: Box::new(Backtrace::capture()),
            locations: vec![Location::caller()],
        }
    }

    /// Appends the caller's site to the existing location chain without
    /// regenerating the backtrace. Used by cross-crate bridges to extend
    /// the chain while preserving origin info.
    #[must_use]
    #[track_caller]
    pub fn push_caller(mut self) -> Self {
        self.locations.push(Location::caller());
        self
    }

    /// Writes the location chain + backtrace into `f`, using the same
    /// format that `define_error!`-generated Debug impls (and `dot::Error<E>`)
    /// use. Callers must already have written the kind's own representation
    /// (typically one `writeln!(f, "{}", kind)` line before this call).
    ///
    /// # Errors
    ///
    /// Propagates any `fmt::Error` raised by the underlying formatter.
    pub fn fmt_chain_and_backtrace(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write_chain_and_backtrace(&self.locations, &self.backtrace, f)
    }
}

/// Shared implementation for writing a location chain followed by a
/// backtrace, used by [`ErrorFields::fmt_chain_and_backtrace`] (which
/// writes into a `std::fmt::Formatter`) and by
/// [`crate::format_traceback`] (which writes into a `String`).
///
/// Generic over `W: std::fmt::Write` so both sinks work with one body;
/// `Formatter<'_>` and `String` both implement the trait.
pub(crate) fn write_chain_and_backtrace<W: std::fmt::Write>(
    chain: &[&'static Location<'static>],
    backtrace: &Backtrace,
    w: &mut W,
) -> std::fmt::Result {
    for (i, loc) in chain.iter().enumerate() {
        writeln!(w, "  at [{i}] {loc}")?;
    }
    write!(w, "{backtrace}")
}

/// Implemented by every error wrapper that carries an [`ErrorFields`] payload.
///
/// Supertrait on [`std::error::Error`] so that `&dyn Traceback` upcasts to
/// `&dyn Error` for source-chain walks (trait upcasting, stable since Rust
/// 1.86). Object-safe by design — [`crate::format_traceback`] takes
/// `&dyn Traceback` without monomorphizing.
///
/// Implementations are provided automatically by
/// [`crate::define_error!`] for non-generic wrappers, and by hand for the
/// generic `dot::error::Error<E>`.
pub trait Traceback: std::error::Error {
    /// Returns the propagation chain (origin first, top-of-stack last).
    fn location_chain(&self) -> &LocationChain;

    /// Returns the backtrace captured at the origin of this error.
    fn origin_backtrace(&self) -> &Backtrace;
}
