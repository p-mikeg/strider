//! Error types for the `pattern` crate.
//!
//! Most fallible operations return [`Result`] (= [`anyhow::Result<T>`]).
//! Three named sentinel structs preserve test-asserted public API:
//!
//! - [`RewriteSkip`] — opts a rewrite rule out without surfacing a hard
//!   error. The `rewrite_rule` interpreter detects this via [`is_skip`]
//!   and converts it back to "no change".
//! - [`NotBuildable`] — pattern is match-only (wildcards, guards, control
//!   patterns) and has no build semantics. Surfaced when a pattern that
//!   doesn't implement `try_build` appears on the RHS of a rewrite rule.
//! - [`MissingBinding`] — a capture variable referenced by a builder is
//!   not bound by the LHS match. Indicates a pattern-authoring bug.
//!
//! Tests downcast the propagated [`anyhow::Error`] to these structs to
//! assert which error path fired.

/// Sentinel produced by [`skip`]; detected by the `rewrite_rule`
/// interpreter via [`is_skip`] and converted to "no change".
#[derive(Debug, thiserror::Error)]
#[error("rewrite rule opted to skip")]
pub struct RewriteSkip;

/// Returned by `Pattern::try_build` defaults for patterns that have no
/// build semantics (wildcards, guards, control patterns).
#[derive(Debug, thiserror::Error)]
#[error("pattern {0} is not buildable (match-only)")]
pub struct NotBuildable(pub &'static str);

/// A capture variable referenced by a builder is not bound by the LHS
/// match. Carries the capture **kind name** (e.g. `"IntVar"`) so the
/// site of the bug is obvious from the error message.
#[derive(Debug, thiserror::Error)]
#[error("missing binding for capture of kind {0}")]
pub struct MissingBinding(pub &'static str);

/// Returns an [`anyhow::Error`] carrying the [`RewriteSkip`] sentinel.
/// The `rewrite_rule` interpreter converts this back to "no change"
/// rather than treating it as a hard failure.
#[track_caller]
#[must_use]
pub fn skip() -> anyhow::Error {
    anyhow::Error::new(RewriteSkip)
}

/// Returns `true` if `err` is the [`RewriteSkip`] sentinel.
#[must_use]
pub fn is_skip(err: &anyhow::Error) -> bool {
    err.downcast_ref::<RewriteSkip>().is_some()
}

/// Convenience `Result` alias.
pub type Result<T> = anyhow::Result<T>;
