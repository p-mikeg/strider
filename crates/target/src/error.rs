strider_error::define_error! {
    pub struct Error wraps ErrorKind;

    /// Errors produced while resolving a target description (architecture or
    /// calling convention) against a Sleigh register table.
    #[derive(Debug, thiserror::Error)]
    pub enum ErrorKind {
        /// A register name listed in the target description does not resolve
        /// to a known Sleigh register for the active architecture.
        #[error("unknown register name by sleigh {0:?}")]
        UnknownRegName(String),
    }
}

/// Convenience `Result` alias that uses [`Error`] as the error type.
pub type Result<T> = std::result::Result<T, Error>;
