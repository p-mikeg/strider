use thiserror::Error;
use crate::node::{NodeOutputId, NodeOutputKind};
#[derive(Debug, Error)]
pub enum Error {
    #[error("expected {1:?} params and got {0:?}")]
    InvalidNumberOfParams(Vec<NodeOutputId>, u64),

    #[error("output id {0:?} should be a value kind but got kind {1:?}")]
    InvalidOutputType(NodeOutputId, NodeOutputKind),
}

/// the result type using our error.
pub type Result<T> = std::result::Result<T, Error>;