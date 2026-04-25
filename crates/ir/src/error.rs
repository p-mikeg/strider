use crate::node::{NodeId, NodeInputId, NodeOutputId, NodeOutputKind, NodeOutputType};

/// Errors that can be produced by the IR builder and graph operations.
#[derive(Debug, thiserror::Error)]
pub enum ErrorKind {
    /// A node was constructed with the wrong number of parameter outputs.
    #[error("expected {1:?} params and got {0:?}")]
    InvalidNumberOfParams(Vec<NodeOutputId>, u64),

    /// An output was expected to carry a concrete value type but doesn't.
    #[error("output id {0:?} should be a value kind but got kind {1:?}")]
    InvalidOutputType(NodeOutputId, NodeOutputKind),

    /// A builder operation was attempted with no active region.
    #[error("no current region is set")]
    NoCurrentRegion,

    /// A builder operation was attempted on a region that has already been terminated.
    #[error("attempted to insert into terminated region {0}")]
    RegionTerminated(u32),

    /// An output was expected to be a `Control` edge.
    #[error("output {0:?} is not a control edge (got {1:?})")]
    ExpectedControl(NodeOutputId, NodeOutputKind),

    /// An output was expected to be a `Memory` edge.
    #[error("output {0:?} is not a memory edge (got {1:?})")]
    ExpectedMemory(NodeOutputId, NodeOutputKind),

    /// An output was expected to carry a concrete value.
    #[error("output {0:?} is not a value edge (got {1:?})")]
    ExpectedValue(NodeOutputId, NodeOutputKind),

    /// An output was expected to carry a concrete value type but is a
    /// control/memory/control-phi edge instead. Unlike [`Self::ExpectedValue`],
    /// this variant carries only the mismatched kind (no output id), used
    /// by [`crate::node::NodeOutputKind::as_value_or_err`].
    #[error("expected value output, got {0:?}")]
    ExpectedValueOutput(NodeOutputKind),

    /// An output was expected to carry a `Bool` value.
    #[error("output {0:?} is not a bool value")]
    ExpectedBool(NodeOutputId),

    /// An output was expected to carry an integer value.
    #[error("output {0:?} is not an integer value")]
    ExpectedInteger(NodeOutputId),

    /// An output was expected to carry a float value (F32 or F64).
    #[error("output {0:?} is not a float value")]
    ExpectedFloat(NodeOutputId),

    /// A type was expected to be a float type (F32 or F64).
    #[error("type {0:?} is not a float type")]
    ExpectedFloatType(NodeOutputType),

    /// A type was expected to be an integer type (U8/U16/U32/U64).
    #[error("type {0:?} is not an integer type")]
    ExpectedIntegerType(NodeOutputType),

    /// An output was expected to be a `ControlPhi` dispatch edge.
    #[error("output {0:?} is not a control-phi edge")]
    ExpectedControlPhi(NodeOutputId),

    /// An input index was out of range for a node's input list.
    #[error("input index {index} out of bounds for node {node:?} (len={len})")]
    InputIndexOutOfBounds {
        node: NodeId,
        index: usize,
        len: usize,
    },

    /// A cursor operation was attempted on a null (empty) use.
    #[error("attempted to replace a null cursor use")]
    NullCursorUse,

    /// `add_node_input` was called on a cacheable (deduplicated) node.
    #[error("attempted to add input to cacheable node {0:?}")]
    AddInputToCacheableNode(NodeId),

    /// `remove_node_input` was called on a cacheable (deduplicated) node.
    #[error("attempted to remove input from cacheable node {0:?}")]
    RemoveInputFromCacheableNode(NodeId),

    /// A varnode was referenced that is not tracked by the builder.
    #[error("variable {0:?} not found in builder")]
    VariableNotFound(rsleigh::Vn),

    /// A varnode had a byte size with no corresponding [`NodeOutputType`].
    #[error("unsupported node output size: {0} bytes")]
    UnsupportedOutputSize(u32),

    /// `build_int_const` was called with a `NodeOutputType` whose width
    /// exceeds 64 bits. The IR stores integer constants as `u64`, so values
    /// of width `U128` or `U256` cannot be faithfully represented.
    #[error("cannot build an IntConst of type {0}: constants are stored as u64")]
    IntConstWidthExceedsU64(NodeOutputType),

    /// An input slot was already part of a use-list when it should be fresh.
    #[error("input {0:?} is already linked")]
    InputAlreadyLinked(NodeInputId),

    /// A node was queried for exactly `N` outputs but had a different count.
    #[error("node {0:?} does not have exactly {1} outputs (has {2})")]
    WrongOutputCount(NodeId, usize, usize),

    /// A node was queried for exactly `N` inputs but had a different count.
    #[error("node {0:?} does not have exactly {1} inputs (has {2})")]
    WrongInputCount(NodeId, usize, usize),

    /// Whole-graph validation detected one or more structural violations.
    #[error("ir validation failed:\n{0}")]
    ValidationFailed(crate::validate::ValidationErrors),

    /// A test assertion failed. Exists so tests can return `Result<(), Error>`
    /// instead of using `panic!`.
    #[error("assertion failed: {0}")]
    AssertionFailed(String),
}

strider_error::define_error! {
    pub struct Error wraps ErrorKind;
}

/// Hand-rolled bridge so call sites can write
/// `validate::validate(...)?` and have the `ValidationErrors` bundle turn
/// into a fully-constructed [`Error`] (backtrace + seeded location chain).
/// `ValidationErrors` itself carries no origin info — all entries originate
/// in one validation pass, so capturing a single backtrace here is correct.
impl From<crate::validate::ValidationErrors> for Error {
    #[track_caller]
    fn from(e: crate::validate::ValidationErrors) -> Self {
        ErrorKind::ValidationFailed(e).into()
    }
}

/// Convenience `Result` alias that uses [`Error`] as the error type.
pub type Result<T> = std::result::Result<T, Error>;
