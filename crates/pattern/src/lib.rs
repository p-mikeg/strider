//! Shim — moved to strider-analyze. See docs/superpowers/plans/
//! 2026-05-17-strider-v2-rewrite.md Phase 3 Task 3.0.
pub use strider_analyze::pattern::*;

// `#[macro_export]` macros live at the strider_analyze crate root, so the glob
// above doesn't re-export them.  Forward them by name.
pub use strider_analyze::{
    __const_with_bind_one, __const_with_bindings, __const_with_extract, bool_const_with,
    float_const_with, int_const_with,
};
