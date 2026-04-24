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
        for (i, loc) in self.locations.iter().enumerate() {
            writeln!(
                f,
                "  at [{}] {}:{}:{}",
                i,
                loc.file(),
                loc.line(),
                loc.column(),
            )?;
        }
        write!(f, "{}", self.backtrace)
    }
}
