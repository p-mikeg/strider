//! In-place IR edits for indirect-branch resolution.
//!
//! Both `apply_link_register` and `apply_tail_call` are pure
//! re-exports from [`opt`]; this module exists only as a stable strider
//! path for the orchestrator and integration tests.  The unit tests
//! for the editors live in `opt::indirect_branch_resolve::inplace::tests`.

pub use opt::{apply_link_register, apply_tail_call};
