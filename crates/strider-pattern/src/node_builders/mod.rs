//! Node-family builders.
//!
//! These return a finished [`Pattern`](crate::matcher::Pattern) from
//! `.build()` rather than a typed
//! [`MatchPat`](crate::matcher::match_pat::MatchPat) struct. Only
//! value-producing fixed-arity patterns get typed structs; the variadic,
//! control and memory families (`Load`, `Store`, `Call`, `CallOther`,
//! `Return`, `If`, `Phi`, `MemPhi`, `function_arg`) are imperative.
//!
//! Each builder owns one [`MatcherBuilder`], compiles its sub-patterns into
//! it, wires them into the right input slots, then seals via `finish`. The
//! root is derived structurally, so the seal takes no root handle whether or
//! not the family produces a value.
//!
//! # Memory tokens are first-class
//!
//! The IR's memory side-channel (`InitialMemory -> Store -> MemPhi -> Call ->
//! Load`) is matched the same way as the value and control chains: a
//! producer's memory token is a real [`PatValue`](crate::matcher::PatValue)
//! carrying [`OutputKindSpec::Memory`](crate::matcher::OutputKindSpec::Memory),
//! and the memory-side builders model both their memory input and their
//! produced token as genuine vertices. A `load` chaining off a prior `store`
//! wires that store's memory output into the load's memory input slot.
//!
//! [`MemPat`] is the memory-side mirror of
//! [`MatchPat`](crate::matcher::match_pat::MatchPat).

pub mod flow;
pub mod function_arg;
pub mod memory;
pub(crate) mod node_pat;
pub mod phi;

pub use flow::{
    CallOtherPat, CallPat, EntryPat, IfPat, IndirectBranchPat, OutputPat, RegionPat, RetPat,
    SwitchPat, UnreachablePat, WithOutput, call, call_other, entry, if_node, indirect_branch,
    region, ret, switch, unreachable,
};
pub use function_arg::{
    FunctionArgPat, function_arg, function_arg_any, function_arg_reg, function_arg_stack,
};
pub use memory::{LoadPat, StorePat, load, store};
pub use phi::{MemPhiPat, PhiPat, mem_phi, phi, phi_for};

use crate::matcher::{MatcherBuilder, PatValueRef};

/// Defers a sub-pattern's compilation until `build`, once the shared
/// [`MatcherBuilder`] exists.
pub(crate) type SubCompiler = Box<dyn FnOnce(&mut MatcherBuilder) -> PatValueRef>;

/// A memory-producing sub-pattern chainable into a consumer's memory input
/// slot.
pub trait MemPat {
    /// Returns the handle of the produced memory-token output, which the
    /// consuming `load` / `store` wires at its memory input slot.
    fn compile_mem(self, b: &mut MatcherBuilder) -> PatValueRef;
}
