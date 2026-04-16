strider_error::define_error! {
    pub struct Error wraps ErrorKind;

    /// Errors that can be produced by the pattern crate.
    #[derive(Debug, thiserror::Error)]
    pub enum ErrorKind {
        /// A test assertion failed. Exists so tests can return `Result<(), Error>`
        /// instead of using `panic!`.
        #[error("assertion failed: {0}")]
        AssertionFailed(String),
    }
}

/// Convenience `Result` alias that uses [`Error`] as the error type.
pub type Result<T> = std::result::Result<T, Error>;
