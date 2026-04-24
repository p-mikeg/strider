//! Manual generic wrapper.
//!
//! `Error<E>` is generic over the dumper's own error type, so we can't use
//! the `strider_error::define_error!` macro (which is monomorphic). This
//! file mirrors exactly what the macro emits, with `<E>` threaded through.

use std::fmt::Debug;

use strider_error::{ErrorFields, LocationChain};
use thiserror::Error;

/// Errors that can be produced by the dot rendering pipeline.
#[derive(Debug, Error)]
pub enum ErrorKind<E: Debug> {
    #[error("svg conversion error {0:?}")]
    SvgConversionError(String),

    #[error(transparent)]
    IoError(#[from] std::io::Error),

    #[error(transparent)]
    DotDumpError(E),
}

/// Wrapper that carries a backtrace + location chain alongside an
/// [`ErrorKind`]. Mirrors the shape produced by
/// [`strider_error::define_error!`] for non-generic error types.
pub struct Error<E: Debug> {
    kind: Box<ErrorKind<E>>,
    fields: ErrorFields,
}

impl<E: Debug> Error<E> {
    /// Returns a reference to the underlying `ErrorKind`.
    pub fn kind(&self) -> &ErrorKind<E> {
        &self.kind
    }

    /// Consumes the wrapper and returns the owned `ErrorKind`.
    pub fn into_kind(self) -> ErrorKind<E> {
        *self.kind
    }

    /// Splits the wrapper into its boxed kind and shared fields.
    pub fn decompose(self) -> (Box<ErrorKind<E>>, ErrorFields) {
        (self.kind, self.fields)
    }

    /// Returns the per-`?` propagation chain (origin first).
    pub fn locations(&self) -> &LocationChain {
        &self.fields.locations
    }

    /// Returns the backtrace captured at the origin of this error.
    pub fn backtrace(&self) -> &std::backtrace::Backtrace {
        &self.fields.backtrace
    }
}

impl<E: Debug + std::fmt::Display> std::fmt::Display for Error<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&*self.kind, f)
    }
}

impl<E: Debug + std::fmt::Display> std::fmt::Debug for Error<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "{}", self.kind)?;
        self.fields.fmt_chain_and_backtrace(f)
    }
}

impl<E: Debug + std::error::Error + 'static> std::error::Error for Error<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        std::error::Error::source(&*self.kind)
    }
}

impl<E: Debug + std::error::Error + std::fmt::Display + 'static> strider_error::Traceback for Error<E> {
    fn location_chain(&self) -> &strider_error::LocationChain {
        &self.fields.locations
    }
    fn origin_backtrace(&self) -> &std::backtrace::Backtrace {
        &self.fields.backtrace
    }
}

impl<E: Debug> From<ErrorKind<E>> for Error<E> {
    #[track_caller]
    fn from(kind: ErrorKind<E>) -> Self {
        Self {
            kind: Box::new(kind),
            fields: ErrorFields::new(),
        }
    }
}

impl<E: Debug> From<std::io::Error> for Error<E> {
    #[track_caller]
    fn from(e: std::io::Error) -> Self {
        <Error<E> as From<ErrorKind<E>>>::from(<ErrorKind<E> as From<std::io::Error>>::from(e))
    }
}

/// Convenience `Result` alias that uses [`Error`] as the error type.
pub type Result<T, E> = std::result::Result<T, Error<E>>;
