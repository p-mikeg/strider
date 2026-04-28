//! Control-flow graph construction for the Strider binary analysis framework.
//!
//! This crate lifts a binary function to a [`Cfg`] of basic blocks using
//! GHIDRA's Sleigh p-code lifter ([`rsleigh`]).  Each basic block (region) in
//! the CFG contains a sequence of p-code instructions ([`rsleigh::Insn`]).
//!
//! # Key types
//!
//! - [`Cfg`] — a control-flow graph parameterized over an arbitrary memory
//!   reader; built via [`Builder`]
//! - [`Builder`] / [`OptionsBuilder`] — fluent constructors for a [`Cfg`]
//! - [`RegionId`] — identifies a basic block within the CFG
//! - [`RegionEdgeKind`] — `Fallthrough`, `Branch`, `IfCaseTrue`, `IfCaseFalse`
//! - [`IfRegionState`] — tracks the resolved/unresolved state of an if-case

mod cfg;
pub mod error;
pub use cfg::{
    Builder, Cfg, IfRegionState, MachineInsnAddr, OptionsBuilder, PcodeInsnAddr, Region,
    RegionEdgeKind, RegionId, RegionInstruction, RegionTerminator, ResolvedTargets,
};
pub use error::{Error, ErrorKind, Result};

#[doc(hidden)]
pub mod test_api;
