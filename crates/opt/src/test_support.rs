//! Re-exports of shared mock-IR helpers for white-box tests inside `opt`.
//!
//! These live in `ir::test_utils` (feature-gated) so all crates that build
//! mock IR for testing share one canonical implementation.

pub(crate) use ir::test_utils::{make_empty_fn as make_fn, make_fn_with_var};
