use thiserror::Error;

/// Errors produced by optimization passes.
#[derive(Debug, Error)]
pub enum Error {
    /// Propagated from the underlying IR layer.
    #[error(transparent)]
    IrError(#[from] ir::Error),
    /// An output was expected to carry a concrete value but doesn't.
    #[error("expected value output, got {0:?}")]
    ExpectedValueOutput(ir::node::NodeOutputKind),
    /// An output was expected to carry an integer type but carries another.
    #[error("expected integer type, got {0:?}")]
    ExpectedIntegerType(ir::node::NodeOutputType),
    /// The function has no `Return` node (malformed IR).
    #[error("no Return node found in function")]
    NoReturnNode,
    /// Dead-branch elimination could not find the unique live control input to
    /// a `ControlState` node.
    #[error("unique control edge not found in control-state inputs")]
    UniqueCtrlNotFound,
}

/// Convenience `Result` alias that uses [`Error`] as the error type.
pub type Result<T> = std::result::Result<T, Error>;

impl From<ir::ValidationErrors> for Error {
    fn from(errs: ir::ValidationErrors) -> Self {
        Error::IrError(ir::Error::from(errs))
    }
}
