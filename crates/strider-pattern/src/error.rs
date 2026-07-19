//! Fallible pattern operations return plain [`anyhow::Result`]. The one named
//! sentinel, [`RewriteSkip`], lets a rewrite rule opt out without surfacing a
//! hard error.

use std::fmt;

pub type Result<T> = anyhow::Result<T>;

/// Returned by a closure inside a rewrite RHS to decline the rewrite. The
/// `rewrite_rule` interpreter detects it via [`is_skip`] and reports "no
/// change" instead of failing.
#[derive(Debug)]
pub struct RewriteSkip;

impl fmt::Display for RewriteSkip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("rewrite rule opted to skip")
    }
}

impl std::error::Error for RewriteSkip {}

#[track_caller]
pub fn skip() -> anyhow::Error {
    anyhow::Error::from(RewriteSkip)
}

pub fn is_skip(err: &anyhow::Error) -> bool {
    err.is::<RewriteSkip>()
}

/// A builder referenced a capture the LHS never bound. Carries the capture
/// kind name (`"uint"`, `"int_binary_op"`, ...) to locate the bug.
#[derive(Debug)]
pub struct MissingBinding(&'static str);

impl fmt::Display for MissingBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "missing binding for capture of kind {}", self.0)
    }
}

impl std::error::Error for MissingBinding {}

pub fn missing_binding(kind: &'static str) -> anyhow::Error {
    anyhow::Error::new(MissingBinding(kind))
}
