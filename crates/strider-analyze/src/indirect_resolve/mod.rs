//! IR-level (post-IR) resolver for `BranchIndirect` placeholders that
//! the cfg-time mini-graph resolver (in `cfg::indirect_resolve`) couldn't
//! classify.
//!
//! The IR-level indirect-branch resolver inspects the producer of each
//! placeholder's anchored target value AFTER the stable optimiser
//! subset has run on the full IR.  This gives it visibility into
//! cross-region flow, `StackLoadForward` results, `LoadReadOnly`
//! resolutions, and `KnownBits` propagation — none of which the
//! single-region cfg-time mini-graph can see.

pub use cfg::test_api::ResolvedTargets;

mod classify;
pub mod inplace;

pub use classify::{
    classify_anchor, classify_anchor_with_rom, classify_anchor_with_rom_and_sp,
};
pub use inplace::{apply_link_register, apply_tail_call};
