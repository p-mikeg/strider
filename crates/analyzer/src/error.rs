use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    SleighError(#[from] rsleigh::error::BaseError),

    #[error("generic sleigh error {0:?}")]
    GenericSleighError(String),

    #[error("unknown register name by sleign {0:?}")]
    UnknownRegName(String),

    #[error("no region {0:?} in cfg")]
    CfgNoRegion(cfg::RegionId),

    #[error(transparent)]
    CfgError(#[from] cfg::Error),
}

/// the result type using our error.
pub type Result<T> = std::result::Result<T, Error>;
