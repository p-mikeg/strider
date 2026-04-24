strider_error::define_error! {
    pub struct Error wraps ErrorKind;
    sources: [std::io::Error, object::Error];

    /// Errors that can be produced by the reader crate.
    #[derive(Debug, thiserror::Error)]
    pub enum ErrorKind {
        /// The requested address is not mapped in any loaded region.
        #[error("address {0:#x} is not mapped")]
        NotMapped(u64),

        /// A `MemRegion` was constructed with a (start_addr, len) pair
        /// whose end would exceed `u64::MAX`.
        #[error("region at {start_addr:#x} with length {len} would overflow u64")]
        RegionOverflow { start_addr: u64, len: u64 },

        /// An I/O error occurred while reading a file.
        #[error("failed to read file: {0}")]
        Io(#[from] std::io::Error),

        /// An `object` crate error occurred while parsing or loading an ELF.
        #[error("failed to parse ELF: {0}")]
        Object(#[from] object::Error),
    }
}

/// Convenience `Result` alias that uses [`Error`] as the error type.
pub type Result<T> = std::result::Result<T, Error>;
