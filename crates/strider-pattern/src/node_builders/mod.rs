//! # Memory tokens are first-class
//!
//! The IR's memory side-channel (`InitialMemory -> Store -> MemPhi -> Call ->
//! Load`) is matched the same way as the value and control chains: a
//! producer's memory token is a real [`PatValue`](crate::matcher::PatValue)
//! carrying [`OutputKindSpec::Memory`](crate::matcher::OutputKindSpec::Memory),
//! and the memory-side builders model both their memory input and their
//! produced token as genuine vertices. A `load` chaining off a prior `store`
//! wires that store's memory output into the load's memory input slot.

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
    /// Returns the handle of the produced memory-token output.
    fn compile_mem(self, b: &mut MatcherBuilder) -> PatValueRef;
}
