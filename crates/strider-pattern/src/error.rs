pub type Result<T> = anyhow::Result<T>;

/// Returned from a rewrite RHS closure to decline the rewrite; see [`is_skip`].
#[derive(Debug, thiserror::Error)]
#[error("rewrite rule opted to skip")]
pub struct RewriteSkip;

#[track_caller]
pub fn skip() -> anyhow::Error {
    anyhow::Error::from(RewriteSkip)
}

pub fn is_skip(err: &anyhow::Error) -> bool {
    err.is::<RewriteSkip>()
}

/// A template operand narrower or wider than the width its node is
/// evaluated at.
#[derive(Debug, thiserror::Error)]
#[error(
    "template node {kind:?} takes a {got} operand at input slot {slot}, \
     where it is evaluated at {want}"
)]
pub struct OperandWidthMismatch {
    pub kind: strider_ir::node::NodeKind,
    pub slot: usize,
    pub got: strider_ir::ValueType,
    pub want: strider_ir::ValueType,
}

/// A builder read a capture of `kind` (`"uint"`, ...) that the LHS never bound.
pub fn missing_binding(kind: &'static str) -> anyhow::Error {
    anyhow::anyhow!("missing binding for capture of kind {kind}")
}
