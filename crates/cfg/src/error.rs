use petgraph::graph::NodeIndex;
use thiserror::Error;
use crate::cfg::{PcodeInsnAddr, Region};

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    SleighError(#[from] rsleigh::error::BaseError),

    #[error("generic sleigh error {0:?}")]
    GenericSleighError(String),

    #[error("empty region {0:?}")]
    EmptyRegion(Region),

    #[error("unknown register name by sleign {0:?}")]
    UnknownRegName(String),

    #[error("invalid branch target variable {0:?} at opcode {1:?}")]
    InvalidBranchTargetVaErr(rsleigh::Vn, PcodeInsnAddr),

    #[error("invalid tail call at opcode {0:?}")]
    InvalidTailCall(PcodeInsnAddr),

    #[error("cfg failed accessing starting region")]
    FailedCreatingStartRegion,

    #[error("failed spliting region {0:?} into 2 parts at {1:?}")]
    FailedSplitingRegion(NodeIndex, PcodeInsnAddr),

    #[error("builder about to build an empty instruction region")]
    NoInstructionsRegionBuilder,

    #[error("invalid register vn")]
    InvalidRegVn(rsleigh::Vn),

    #[error(transparent)]
    FormatError(#[from] core::fmt::Error),

    #[error("invalid region index {0:?}")]
    InvalidRegion(NodeIndex)
}

/// the result type using our error.
pub type Result<T> = std::result::Result<T, Error>;