//! Shim — moved to strider-lift. See docs/superpowers/plans/
//! 2026-05-17-strider-v2-rewrite.md Phase 2 Task 2.3.
pub use strider_lift::cfg::*;

/// Crate-level `Result` alias.  Re-exported for compatibility with the
/// original `cfg` crate's public surface; downstream callers may write
/// `cfg::Result<T>`.
pub type Result<T> = anyhow::Result<T>;

/// Test-only API surface.  Re-exported from the absorbed module so the
/// integration tests in `crates/cfg/tests/` keep compiling.
#[doc(hidden)]
pub mod test_api {
    pub use strider_lift::cfg_test_api::*;
}
