#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

//! CFG-to-IR translator for the Strider binary analysis framework.
//!
//! This crate drives [`ir::FunctionBuilder`] from a Sleigh p-code CFG
//! ([`cfg::Cfg`]).  The main entry point is [`Strider`], which lifts a
//! function at a given address into a [`ir::BuiltFunctionGraph`] ready for
//! optimization and pattern queries.
//!
//! # Register aliasing
//!
//! x86 has overlapping sub-registers (`rax`/`eax`/`ax`/`al` etc.).  The
//! internal `IrStrider` handles these transparently via
//! [`pcode_lift::ValueLifter::read_vn`] / [`pcode_lift::ValueLifter::write_vn`]:
//! all reads and writes go through the largest containing register, with
//! shift/mask operations inserted for sub-register accesses.
//!
//! # Key types
//!
//! - [`Strider`] — wraps a Sleigh lifter and a [`CallingConvention`]; call
//!   `analyze_function` to obtain a [`ir::BuiltFunctionGraph`]
//! - [`SleighArch`] — architecture selection for the Sleigh lifter
//! - [`CallingConvention`] — describes which registers are caller-saved

pub mod cache;
mod strider;
pub mod error;
pub mod indirect_resolve_tier2;
pub mod rewrite;

// W6: `ir_cache` is the historical name for what is now `cache::*`.  Keep
// the alias so external callers continue to compile while we migrate.
#[doc(hidden)]
pub use cache as ir_cache;
pub use cache::{
    cache_key_for_region, count_uncached_regions, extend_predecessors_into,
    extend_predecessors_with_handle, invalidate_split_regions, lift_new_regions_into,
    lift_new_regions_into_with_stats, predecessor_diffs, LiftStats, PredecessorHandles,
    RegionIrCache, RegionIrEntry,
};
pub use strider::{AnalyzeOutcome, RegionLiftHandles, Strider};
pub use rewrite::GraphRewriter;
pub use error::{Error, ErrorKind, Result};
pub use target::{BuiltCallingConvention, CallingConvention, Endianness, SleighArch};
