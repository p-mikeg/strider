//! Error types — [`RewriteSkip`] sentinel + [`Result`] alias.
//!
//! Most fallible operations in the pattern crate return [`Result`] (= an
//! [`anyhow::Result`]).  One named sentinel — [`RewriteSkip`] — opts a
//! rewrite rule out without surfacing a hard error: the
//! `rewrite_rule` interpreter detects it via [`is_skip`] and converts
//! it back to "no change".

use std::fmt;

/// Convenience `Result` alias used throughout the pattern crate.
pub type Result<T> = anyhow::Result<T>;

/// Sentinel returned by a closure inside a rewrite RHS to opt out of
/// the rewrite without a hard error.  The rewriter detects this via
/// [`is_skip`] and returns `Ok(false)`.
#[derive(Debug)]
pub struct RewriteSkip;

impl fmt::Display for RewriteSkip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("rewrite rule opted to skip")
    }
}

impl std::error::Error for RewriteSkip {}

/// Returns an [`anyhow::Error`] carrying the [`RewriteSkip`] sentinel.
/// The `rewrite_rule` interpreter converts this back to "no change"
/// rather than treating it as a hard failure.
#[track_caller]
pub fn skip() -> anyhow::Error {
    anyhow::Error::from(RewriteSkip)
}

/// Returns `true` if `err` is the [`RewriteSkip`] sentinel.
pub fn is_skip(err: &anyhow::Error) -> bool {
    err.is::<RewriteSkip>()
}

/// Pattern-build error: a builder referenced a capture that the LHS
/// failed to bind.  Carries the capture **kind name** (e.g. `"uint"`,
/// `"int_binary_op"`) so the site of the bug is obvious from the
/// error message.
#[derive(Debug)]
pub struct MissingBinding(&'static str);

impl fmt::Display for MissingBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "missing binding for capture of kind {}", self.0)
    }
}

impl std::error::Error for MissingBinding {}

/// Returns an [`anyhow::Error`] wrapping a [`MissingBinding`] for the
/// given capture-kind name.  Used uniformly by every build-time
/// closure that materialises captured bindings (including the
/// `*_const_with!` macro expansions).
pub fn missing_binding(kind: &'static str) -> anyhow::Error {
    anyhow::Error::new(MissingBinding(kind))
}
