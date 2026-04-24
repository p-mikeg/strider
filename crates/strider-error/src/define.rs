/// Generates a wrapper struct over an existing `thiserror`-derived enum.
///
/// The enum is a separate, vanilla Rust declaration — the macro does
/// **not** take the enum body as input. This keeps the enum's attributes
/// (`#[derive(Debug, thiserror::Error)]`, variant-level `#[error(...)]`
/// and `#[from]`) in plain Rust, so rustfmt, rust-analyzer, and other
/// tooling see an ordinary enum.
///
/// The macro emits:
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
/// #[derive(Debug, thiserror::Error)]
/// pub enum ErrorKind {
///     #[error("address {0:#x} is not mapped")]
///     NotMapped(u64),
///     #[error("io: {0}")]
///     Io(#[from] std::io::Error),
/// }
///
/// strider_error::define_error! {
///     pub struct Error wraps ErrorKind;
///     sources: [std::io::Error];
/// }
///
/// let err = Error::from(ErrorKind::NotMapped(0xdead_beef));
/// assert_eq!(err.to_string(), "address 0xdeadbeef is not mapped");
/// assert_eq!(err.locations().len(), 1);
/// ```
///
/// # Caveats
///
/// **`.into()` loses the `#[track_caller]` chain.** The wrapper's
/// `From<$kind>` impl is `#[track_caller]`, so `?` and explicit
/// `Wrapper::from(kind)` both place the first entry of `err.locations()`
/// at the user's call site. The std blanket `Into::into` is *not*
/// `#[track_caller]`, though, so `ErrorKind::X.into()` resolves
/// `Location::caller()` to a line inside `core/src/convert/mod.rs`
/// rather than the user's code. The backtrace is unaffected (it's
/// captured unconditionally inside `ErrorFields::new`), but the
/// location chain's origin entry is misleading.
///
/// If you need the chain to point at your own site and don't have a
/// `?` context handy, use `Wrapper::from(kind)` explicitly:
///
/// ```ignore
/// let err = MyError::from(MyKind::Boom);   // caller site captured
/// // vs.
/// let err: MyError = MyKind::Boom.into();  // core::convert captured
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
    ) => {
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
                self.fields.fmt_chain_and_backtrace(f)
            }
        }

        impl ::std::error::Error for $wrapper {
            fn source(&self) -> ::std::option::Option<&(dyn ::std::error::Error + 'static)> {
                ::std::error::Error::source(&*self.kind)
            }
        }

        impl $crate::Traceback for $wrapper {
            fn location_chain(&self) -> &$crate::LocationChain {
                &self.fields.locations
            }
            fn origin_backtrace(&self) -> &::std::backtrace::Backtrace {
                &self.fields.backtrace
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

/// Generates a `#[track_caller] impl From<$inner> for $outer` that decomposes
/// the inner wrapper, appends the caller's site, and re-assembles as the outer
/// wrapper with kind wrapped in `$outer_kind::$variant`.
///
/// The inner type must expose `.decompose() -> (Box<InnerKind>, ErrorFields)`
/// — any type produced by [`define_error!`](crate::define_error) does, as does
/// the hand-rolled `dot::error::Error<E>` via its manual `decompose` method.
///
/// `$outer_kind::$variant` must be a **tuple variant** that takes the inner
/// kind as its single positional field (e.g. `Inner(InnerKind)`). The
/// expansion calls `$outer_kind::$variant(*kind)`, so struct variants like
/// `Inner { kind: InnerKind }` will not compile. If you need a struct
/// variant, write the bridge `impl From<$inner> for $outer` by hand.
///
/// # Example
///
/// ```
/// #[derive(Debug, thiserror::Error)]
/// pub enum InnerKind { #[error("boom")] Boom }
///
/// strider_error::define_error! {
///     pub struct InnerError wraps InnerKind;
/// }
///
/// #[derive(Debug, thiserror::Error)]
/// pub enum OuterKind {
///     #[error(transparent)]
///     Inner(InnerKind),
/// }
///
/// strider_error::define_error! {
///     pub struct OuterError wraps OuterKind;
/// }
///
/// strider_error::bridge_error!(InnerError => OuterError, OuterKind::Inner);
///
/// fn inner() -> Result<(), InnerError> { Err(InnerKind::Boom.into()) }
/// fn outer() -> Result<(), OuterError> { inner()?; Ok(()) }
///
/// let err = outer().unwrap_err();
/// assert_eq!(err.locations().len(), 2, "origin + bridge push_caller");
/// ```
///
/// Expands to:
///
/// ```text
/// impl ::core::convert::From<InnerError> for OuterError {
///     #[track_caller]
///     fn from(e: InnerError) -> Self {
///         let (kind, fields) = e.decompose();
///         Self {
///             kind: ::std::boxed::Box::new(OuterKind::Inner(*kind)),
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
