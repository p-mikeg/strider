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

/// Defines a crate's error wrapper struct + its underlying `ErrorKind`
/// enum in one macro invocation.
///
/// The macro emits:
///   * the enum literally as written (so `#[derive(Debug, thiserror::Error)]`
///     and any variant-level `#[error(...)]` / `#[from]` attributes apply);
///   * a wrapper struct `$wrapper { kind: Box<$kind>, fields: ErrorFields }`;
///   * `impl Display` (delegates to the inner enum);
///   * `impl Debug` (prints kind + location chain + backtrace);
///   * `impl std::error::Error` (delegates `source()` to the enum so the
///     error-chain traversal works transparently);
///   * `impl From<$kind> for $wrapper` — captures a fresh backtrace and
///     seeds the location chain. This is the "origin" boundary.
///   * `impl From<$src> for $wrapper` for every `$src` listed in the
///     optional `sources: [...]` block. Each bridges through the
///     enum's `#[from]`-generated `From<$src> for $kind`.
///
/// All `From` impls are `#[track_caller]` so `Location::caller()` inside
/// [`ErrorFields::new`] resolves to the `?` site in the caller.
///
/// # Example
///
/// ```ignore
/// strider_error::define_error! {
///     pub struct Error wraps ErrorKind;
///     sources: [std::io::Error, object::Error];
///
///     #[derive(Debug, thiserror::Error)]
///     pub enum ErrorKind {
///         #[error("address {0:#x} is not mapped")]
///         NotMapped(u64),
///         #[error("failed to read file: {0}")]
///         Io(#[from] std::io::Error),
///         #[error("failed to parse ELF: {0}")]
///         Object(#[from] object::Error),
///         #[error("assertion failed: {0}")]
///         AssertionFailed(String),
///     }
/// }
/// ```
///
/// # Cross-crate bridges
///
/// Bridges that unwrap another crate's wrapper (e.g. `From<ir::Error> for
/// opt::Error`) are written by hand in the outer crate so they can call
/// [`crate::wrapper::ErrorFields::push_caller`] on the inner fields
/// instead of regenerating them. Do not list another crate's wrapper in
/// `sources: [...]`.
#[macro_export]
macro_rules! define_error {
    (
        $(#[$wrapper_attr:meta])*
        pub struct $wrapper:ident wraps $kind:ident;
        $( sources: [ $($src:ty),* $(,)? ]; )?

        $(#[$enum_attr:meta])*
        pub enum $kind_enum:ident {
            $($body:tt)*
        }
    ) => {
        $(#[$enum_attr])*
        pub enum $kind_enum {
            $($body)*
        }

        $(#[$wrapper_attr])*
        pub struct $wrapper {
            kind: ::std::boxed::Box<$kind>,
            fields: $crate::ErrorFields,
        }

        impl $wrapper {
            /// Returns a reference to the underlying `ErrorKind`.
            pub fn kind(&self) -> &$kind { &self.kind }

            /// Consumes the wrapper and returns the owned `ErrorKind`.
            pub fn into_kind(self) -> $kind { *self.kind }

            /// Splits the wrapper into its boxed kind and shared fields.
            /// Used by downstream wrappers to extend the location chain
            /// across crate boundaries without losing the origin backtrace.
            pub fn decompose(self) -> (::std::boxed::Box<$kind>, $crate::ErrorFields) {
                (self.kind, self.fields)
            }

            /// Returns the per-`?` propagation chain (origin first).
            pub fn locations(&self) -> &$crate::LocationChain {
                &self.fields.locations
            }

            /// Returns the backtrace captured at the origin of this error.
            pub fn backtrace(&self) -> &::std::backtrace::Backtrace {
                &self.fields.backtrace
            }
        }

        impl ::std::fmt::Display for $wrapper {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                ::std::fmt::Display::fmt(&*self.kind, f)
            }
        }

        impl ::std::fmt::Debug for $wrapper {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                writeln!(f, "{}", self.kind)?;
                for (i, loc) in self.fields.locations.iter().enumerate() {
                    writeln!(
                        f,
                        "  at [{}] {}:{}:{}",
                        i,
                        loc.file(),
                        loc.line(),
                        loc.column()
                    )?;
                }
                write!(f, "{}", self.fields.backtrace)
            }
        }

        impl ::std::error::Error for $wrapper {
            fn source(&self) -> ::std::option::Option<&(dyn ::std::error::Error + 'static)> {
                ::std::error::Error::source(&*self.kind)
            }
        }

        impl ::std::convert::From<$kind> for $wrapper {
            #[track_caller]
            fn from(kind: $kind) -> Self {
                Self {
                    kind: ::std::boxed::Box::new(kind),
                    fields: $crate::ErrorFields::new(),
                }
            }
        }

        $(
            $(
                impl ::std::convert::From<$src> for $wrapper {
                    #[track_caller]
                    fn from(e: $src) -> Self {
                        <$wrapper as ::std::convert::From<$kind>>::from(
                            <$kind as ::std::convert::From<$src>>::from(e),
                        )
                    }
                }
            )*
        )?
    };
}
