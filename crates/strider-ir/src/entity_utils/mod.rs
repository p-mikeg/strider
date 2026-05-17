//! Dense entity-set + worklist data structures.
//!
//! Absorbed from the standalone `entity-utils` crate. See
//! `docs/superpowers/plans/2026-05-17-strider-v2-rewrite.md` Phase 1
//! Task 1.2.

pub mod set;
pub mod worklist;

pub use set::DenseEntitySet;
