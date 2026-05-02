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
//!   `analyze_cfg` to obtain a [`ir::BuiltFunctionGraph`]
//! - [`SleighArch`] — architecture selection for the Sleigh lifter
//! - [`CallingConvention`] — describes which registers are caller-saved
//! - [`run`] — top-level orchestrator: builds the CFG, lifts to IR, runs
//!   the optimiser pipeline, and resolves indirect branches via the
//!   tier-2 fixed-point loop

mod errors;
mod orchestrator;
mod strider;
pub mod indirect_resolve;
pub mod rewrite;

pub use errors::UnresolvedIndirectBranch;
pub use orchestrator::{run, RunConfig};
pub use rewrite::GraphRewriter;
pub use strider::{AnalyzeOutcome, RegionLiftHandles, Strider};
pub use target::{BuiltCallingConvention, CallingConvention, Endianness, SleighArch};
