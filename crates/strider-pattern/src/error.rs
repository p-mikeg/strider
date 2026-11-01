//! Fallible pattern operations return plain [`anyhow::Result`]. The one named
//! sentinel, [`RewriteSkip`], lets a rewrite rule opt out without surfacing a
//! hard error.

pub type Result<T> = anyhow::Result<T>;

/// Returned by a closure inside a rewrite RHS to decline the rewrite,
/// detected via [`is_skip`].
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

/// A builder referenced a capture the LHS never bound, naming the capture
/// kind (`"uint"`, `"float_bits"`, ...).
pub fn missing_binding(kind: &'static str) -> anyhow::Error {
    anyhow::anyhow!("missing binding for capture of kind {kind}")
}
