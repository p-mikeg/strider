//! Shim — contents moved to strider-ir. See docs/superpowers/plans/
//! 2026-05-17-strider-v2-rewrite.md Phase 1 Task 1.3.
//!
//! The former `ir::dot` (IR-specific Graphviz renderer) lives at
//! `strider_ir::graph_dot` now to disambiguate from the absorbed generic
//! Graphviz library `strider_ir::dot`.  Re-exported here as `ir::dot` for
//! backwards compatibility with callers that pre-date the rename.
pub use strider_ir::*;
pub use strider_ir::graph_dot as dot;
