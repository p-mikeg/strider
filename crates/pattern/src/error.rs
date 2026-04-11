use thiserror::Error;

/// Errors that can be produced by the pattern crate.
#[derive(Debug, Error)]
pub enum Error {}

/// Convenience `Result` alias that uses [`Error`] as the error type.
pub type Result<T> = std::result::Result<T, Error>;
