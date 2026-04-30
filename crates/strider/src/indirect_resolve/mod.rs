//! Tier-2 (post-IR) resolver for `BranchIndirect` placeholders that
//! tier 1 (the cfg-time mini-graph in `cfg::indirect_resolve`) couldn't
//! classify.
//!
//! Tier 2 inspects the producer of each placeholder's anchored target
//! value AFTER the stable optimiser subset has run on the full IR.
//! This gives it visibility into cross-region flow, `StackLoadForward`
//! results, `LoadReadOnly` resolutions, and `KnownBits` propagation —
//! none of which the single-region tier 1 mini-graph can see.

pub use cfg::test_api::ResolvedTargets;

mod classify;
pub mod inplace;

pub use classify::{
    classify_anchor, classify_anchor_with_rom, classify_anchor_with_rom_and_sp,
};
pub use inplace::{apply_link_register, apply_tail_call};
