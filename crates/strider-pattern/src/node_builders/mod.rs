//! The IR's memory side-channel (`InitialMemory -> Store -> MemPhi -> Call ->
//! Load`) is matched like the value and control chains: a memory token is a
//! real [`PatValue`](crate::matcher::PatValue) carrying
//! [`OutputKindSpec::Memory`](crate::matcher::OutputKindSpec::Memory), so a
//! memory-side builder's input and produced token are both genuine vertices.

pub mod flow;
pub mod function_arg;
pub mod memory;
pub(crate) mod node_pat;
pub mod phi;

pub use flow::{
    CallOtherPat, CallPat, EntryPat, IfPat, IndirectBranchPat, OutputPat, RegionPat, RetPat,
    SwitchPat, UnreachablePat, WithOutput, call, call_other, entry, if_else, indirect_branch,
    region, ret, switch, unreachable,
};
pub use function_arg::{
    FunctionArgClass, FunctionArgPat, any_function_arg, function_arg, function_arg_float,
    function_arg_reg, function_arg_stack,
};
pub use memory::{LoadPat, StorePat, load, store};
pub use phi::{MemPhiPat, PhiPat, mem_phi, phi, phi_for};

use crate::matcher::{MatcherBuilder, PatValueRef};

/// [`WithOutput`] for a builder that wraps one [`NodePat`](node_pat::NodePat)
/// and forwards every output constraint to it unchanged. `$inner` names the
/// field, `0` for a newtype.
macro_rules! delegate_with_output {
    ($ty:ty, $inner:tt) => {
        impl $crate::node_builders::WithOutput for $ty {
            fn capture_output(mut self, slot: Option<usize>, c: $crate::capture::Capture) -> Self {
                self.$inner = self.$inner.capture_output(slot, c);
                self
            }
            fn output_width(mut self, slot: Option<usize>, bits: u32) -> Self {
                self.$inner = self.$inner.output_width(slot, bits);
                self
            }
            fn output_ty(mut self, slot: Option<usize>, ty: strider_ir::node::ValueType) -> Self {
                self.$inner = self.$inner.output_ty(slot, ty);
                self
            }
        }
    };
}
pub(crate) use delegate_with_output;

/// Defers a sub-pattern's compilation until `build`, once the shared
/// [`MatcherBuilder`] exists.
pub(crate) type SubCompiler = Box<dyn FnOnce(&mut MatcherBuilder) -> PatValueRef>;

/// A sub-pattern that produces a memory token, so it can be chained into a
/// consumer's memory input slot. The lowering itself is
/// [`crate::matcher::match_pat::MatchPat::compile_mem`]; this bound is what
/// keeps a value-only pattern out of a memory slot.
pub trait MemPat: crate::matcher::match_pat::MatchPat {}
