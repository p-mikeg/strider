//! Node-family builders.
//!
//! These builders return a finished [`Pattern`](crate::pattern::Pattern)
//! directly (via `.build()`), rather than a typed
//! [`MatchPat`](crate::matcher::match_pat::MatchPat) struct: per the design
//! boundary, only value-producing fixed-arity patterns are typed
//! structs; the variadic / control / memory node families
//! (`Load` / `Store` / `Call` / `CallOther` / `Return` / `If` /
//! `Phi` / `MemPhi` / `function_arg`) are imperative.
//!
//! Each builder owns a single [`MatcherBuilder`], compiles its
//! sub-patterns into it (sharing one [`Pattern`](crate::pattern::Pattern)
//! store), wires the
//! sub-patterns into the right input slots, then seals via `finish`
//! (the match root is derived structurally, so the seal takes no root
//! handle regardless of whether the family has a value output).
//!
//! # Memory tokens are first-class
//!
//! The IR has a memory side-channel
//! (`InitialMemory → Store → MemPhi → Call → Load`): a producer's
//! memory token is a real [`PatValue`](crate::pattern::PatValue) with
//! [`OutputKindSpec::Memory`](crate::pattern::OutputKindSpec::Memory),
//! and a consumer wires it at its memory input slot. The memory-side
//! builders below model BOTH their memory input (when chained) AND their
//! produced memory token (via [`MatcherBuilder::memory_output`]) as
//! genuine vertices, so the memory chain is matchable the same way as
//! the value and control chains: a `load` chaining off a prior `store`'s
//! memory token wires the store's memory output into the load's memory
//! input slot.
//!
//! The [`MemPat`] trait is the memory-side mirror of
//! [`MatchPat`](crate::matcher::match_pat::MatchPat): its
//! [`compile_mem`](MemPat::compile_mem) lowers a memory-producing
//! sub-pattern (a `store` / `mem_phi` / `call`) onto the shared builder
//! and returns its memory-token output handle, which the consumer
//! (`load` / `store`) wires at its memory input slot.

pub mod flow;
pub mod function_arg;
pub mod memory;
pub(crate) mod node_pat;
pub mod phi;

pub use flow::{
    CallOtherPat, CallPat, IfPat, RetPat, call, call_other, if_node, ret,
};
pub use function_arg::{
    FunctionArgPat, function_arg, function_arg_any, function_arg_reg, function_arg_stack,
};
pub use memory::{LoadPat, StorePat, load, store};
pub use phi::{MemPhiPat, PhiPat, mem_phi, phi, phi_for};

use crate::builder::{MatcherBuilder, PatValueRef};

/// A boxed one-shot lowering closure for a sub-pattern: compiles the
/// sub-pattern onto a shared [`MatcherBuilder`] and returns its root
/// output handle. Used by the node-family builders to defer
/// sub-pattern compilation until `build` (when the shared builder
/// exists).
pub(crate) type SubCompiler = Box<dyn FnOnce(&mut MatcherBuilder) -> PatValueRef>;

/// Sparse indexed sub-pattern constraints (raw input slot → compiler).
/// Shared by the control [`flow`] and [`phi`] builders.
pub(crate) type IndexedInputs = Vec<(usize, SubCompiler)>;

/// A memory-producing sub-pattern that can be chained into a consumer's
/// memory input slot.
///
/// The memory-side mirror of [`MatchPat`](crate::matcher::match_pat::MatchPat):
/// [`compile_mem`](Self::compile_mem) lowers the sub-pattern onto the
/// shared [`MatcherBuilder`] and returns the handle of its produced
/// memory-token output — the consumer (`load` / `store`) wires that
/// handle at its memory input slot, so the IR memory chain is walked the
/// same way as the value and control chains.
pub trait MemPat {
    /// Lower this memory-producing pattern into `b`, returning the
    /// handle of its memory-token output.
    fn compile_mem(self, b: &mut MatcherBuilder) -> PatValueRef;
}
