use thiserror::Error;
use crate::node::NodeOutputId;
#[derive(Debug, Error)]
pub enum Error {
    #[error("expected {1:?} params and got {0:?}")]
    InvalidNumberOfParams(Vec<NodeOutputId>, u64)
}

/// the result type using our error.
pub type Result<T> = std::result::Result<T, Error>;