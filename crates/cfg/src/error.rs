use crate::cfg::{PcodeInsnAddr, Region};
use petgraph::graph::NodeIndex;

strider_error::define_error! {
    pub struct Error wraps ErrorKind;
    sources: [rsleigh::error::BaseError, core::fmt::Error];

    #[derive(Debug, thiserror::Error)]
    pub enum ErrorKind {
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
        InvalidRegion(NodeIndex),

        #[error("region {0:?} has more than one outgoing edge of kind {1:?}")]
        DuplicateEdgeKind(NodeIndex, crate::cfg::RegionEdgeKind),

        #[error("non-entry work-queue item has no parent edge")]
        MissingParentEdge,

        #[error("unsupported varnode space for display: {0:?}")]
        UnsupportedVnSpaceDisplay(rsleigh::VnSpace),

        /// A test assertion failed. Exists so tests can return `Result<(), Error>`
        /// instead of using `panic!`.
        #[error("assertion failed: {0}")]
        AssertionFailed(String),
    }
}

/// the result type using our error.
pub type Result<T> = std::result::Result<T, Error>;
