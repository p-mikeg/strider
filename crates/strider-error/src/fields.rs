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
}

/// Generates a `#[track_caller] impl From<$inner> for $outer` that decomposes
/// the inner wrapper, appends the caller's site, and re-assembles as the outer
/// wrapper with kind wrapped in `$outer_kind::$variant`.
///
/// The inner type must expose `.decompose() -> (Box<InnerKind>, ErrorFields)`
/// — any type produced by [`define_error!`](crate::define_error) does, as does
/// the hand-rolled `dot::error::Error<E>` via its manual `decompose` method.
///
/// # Example
///
/// ```ignore
/// strider_error::bridge_error!(ir::Error => Error, ErrorKind::IrError);
/// ```
///
/// expands to:
///
/// ```ignore
/// impl ::core::convert::From<ir::Error> for Error {
///     #[track_caller]
///     fn from(e: ir::Error) -> Self {
///         let (kind, fields) = e.decompose();
///         Self {
///             kind: ::std::boxed::Box::new(ErrorKind::IrError(*kind)),
///             fields: fields.push_caller(),
///         }
///     }
/// }
/// ```
#[macro_export]
macro_rules! bridge_error {
    ($inner:ty => $outer:ident, $outer_kind:ident :: $variant:ident) => {
        impl ::core::convert::From<$inner> for $outer {
            #[track_caller]
            fn from(e: $inner) -> Self {
                let (kind, fields) = e.decompose();
                Self {
                    kind: ::std::boxed::Box::new($outer_kind::$variant(*kind)),
                    fields: fields.push_caller(),
                }
            }
        }
    };
}
