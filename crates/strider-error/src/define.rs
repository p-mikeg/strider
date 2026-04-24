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
/// [`ErrorFields::new`](crate::ErrorFields::new) resolves to the `?` site
/// in the caller.
///
/// # Example
///
/// ```
/// strider_error::define_error! {
///     pub struct Error wraps ErrorKind;
///     sources: [std::io::Error];
///
///     #[derive(Debug, thiserror::Error)]
///     pub enum ErrorKind {
///         #[error("address {0:#x} is not mapped")]
///         NotMapped(u64),
///         #[error("io: {0}")]
///         Io(#[from] std::io::Error),
///     }
/// }
///
/// let err: Error = ErrorKind::NotMapped(0xdead_beef).into();
/// assert_eq!(err.to_string(), "address 0xdeadbeef is not mapped");
/// assert_eq!(err.locations().len(), 1);
/// ```
///
/// # Cross-crate bridges
///
/// Bridges that unwrap another crate's wrapper (e.g. `From<ir::Error> for
/// opt::Error`) use [`bridge_error!`](crate::bridge_error) so they can call
/// [`ErrorFields::push_caller`](crate::ErrorFields::push_caller) on the inner
/// fields instead of regenerating them. Do not list another crate's wrapper
/// in `sources: [...]`.
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
