//! Compatibility re-export of [`strider_pattern`].  This module
//! exists only so existing call sites that import
//! `crate::pattern::*` / `strider_analyze::pattern::*` continue to
//! compile during the migration to the new `strider-pattern` crate.
//! Once every consumer is migrated (next commit), this module is
//! deleted entirely.

pub use strider_pattern::*;

/// Compatibility sub-module — the strider-analyze pattern crate
/// nested its `*_const_with!` macros under `pattern::macros::*`.
/// `strider-pattern` hoists them to the crate root via
/// `#[macro_export]`; this module re-exports them under the old
/// `pattern::macros` path so existing `use crate::pattern::macros::*`
/// imports keep working.
pub mod macros {
    pub use strider_pattern::{bool_const_with, float_const_with, int_const_with};
}
