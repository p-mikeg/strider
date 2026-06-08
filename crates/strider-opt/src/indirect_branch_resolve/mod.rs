//! IR-level indirect-branch resolver.
//!
//! Classifies placeholder anchors that the strider lifter inserts at
//! `BranchIndirect` sites.  The strider orchestrator drives the outer loop
//! (CFG rebuild, cache invalidation, iteration cap) and calls into the
//! classifier functions directly — there is no opt-pipeline pass for
//! indirect-branch resolution.
//!
//! ## Submodules
//!
//! - [`classify`] — producer-shape classifier ([`classify_anchor`])
//!   returning [`strider_cfg::ResolvedTargets`], plus the
//!   analysis-only post-pass that drives it over every live
//!   `IndirectBranch` placeholder ([`IndirectBranchClassify`]).
//! - [`table`] — unified table-dispatch arm covering both the rodata
//!   jump-table (absolute base) and on-stack label-array (SP-rooted base)
//!   shapes ([`classify_table_dispatch`]).
//!
//! ## Where `ResolvedTargets` lives
//!
//! Defined in `strider_cfg::indirect_resolver` (the
//! lowest layer that needs the enum: the cfg builder consumes it via
//! `LiftOptions::known_targets` to seat indirect-branch terminators, and
//! it is the return type of [`classify_anchor`] itself).  Import it directly
//! from there.

#![allow(clippy::module_name_repetitions)]

pub mod classify;
pub mod table;

/// Per-anchor enumeration cap for the table-dispatch arm
/// (`table::classify_table_dispatch`), covering both the rodata jump-table
/// (absolute base) and on-stack label-array (SP-rooted base) shapes.
///
/// `u32::MAX + 1` if a known-bits mask were all-ones, so without this cap
/// a buggy KnownBits result could force iteration through 4 GiB of slots.
/// Real jump tables emitted by gcc/clang are bounded by the source-level
/// `switch` arm count, almost always well under 4096.  Tables larger than
/// this cap are unusual enough that we prefer `None` (defer to
/// `UnresolvedIndirectBranch`) over the pathological enumeration cost.
pub(crate) const MAX_TABLE_ENTRIES: u64 = 4096;

pub use classify::{IndirectBranchClassify, classify_anchor};
pub use table::classify_table_dispatch;
