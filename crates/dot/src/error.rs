use thiserror::Error;

/// Errors that can be produced by the dot rendering pipeline.
#[derive(Debug, Error)]
pub enum Error<E> {
    #[error("svg conversion error {0:?}")]
    SvgConversionError(String),

    #[error(transparent)]
    IoError(#[from] std::io::Error),

    #[error(transparent)]
    DotDumpError(E),
}

/// Convenience `Result` alias that uses [`Error`] as the error type.
pub type Result<T, E> = std::result::Result<T, Error<E>>;
