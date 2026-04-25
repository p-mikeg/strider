/// Errors produced by optimization passes.
#[derive(Debug, thiserror::Error)]
pub enum ErrorKind {
    /// Propagated from the underlying IR layer.
    #[error(transparent)]
    IrError(ir::ErrorKind),
    /// Propagated from the `pattern` crate — raised by rewrite-rule
    /// closures (`pattern::rewrite_rule`, `pattern::apply_rules_in_order`).
    #[error(transparent)]
    PatternError(pattern::ErrorKind),
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
    /// An expected node of a specific kind was not found at a given site.
    /// Carries a human-readable site label and the actual node kind present.
    #[error("expected {0} node, got {1:?}")]
    ExpectedNodeNotFound(&'static str, ir::node::NodeKind),
    /// A post-match capture extraction returned `None`. Indicates a bug in the
    /// pattern-rewrite pipeline: the match succeeded but a named capture
    /// couldn't be resolved (should be impossible if the pattern and
    /// extraction stay in sync).
    #[error("internal: pattern capture `{0}` not bound in successful match")]
    InternalCaptureMissing(&'static str),
    /// A test assertion failed. Exists so tests can return `Result<(), Error>`
    /// instead of using `panic!`.
    #[error("assertion failed: {0}")]
    AssertionFailed(String),
    /// The fixed-point loop in `OptimizerPipeline::run` did not converge
    /// within the iteration limit. Indicates a non-monotone pass.
    #[error("optimizer pipeline did not converge after {0} iterations")]
    FixedPointLimitExceeded(u32),
}

strider_error::define_error! {
    pub struct Error wraps ErrorKind;
}

// Preserves origin backtrace + location chain captured by `ir`.
strider_error::bridge_error!(ir::Error => Error, ErrorKind::IrError);

/// `ir::ValidationErrors` is produced fresh at the validator call site, so
/// route it through `ir::Error` (which captures a fresh backtrace) and then
/// through the bridge above.
impl From<ir::ValidationErrors> for Error {
    #[track_caller]
    fn from(errs: ir::ValidationErrors) -> Self {
        Error::from(ir::Error::from(errs))
    }
}

// Preserves origin backtrace + location chain captured by `pattern`.
strider_error::bridge_error!(pattern::Error => Error, ErrorKind::PatternError);

/// Convenience `Result` alias that uses [`Error`] as the error type.
pub type Result<T> = std::result::Result<T, Error>;
