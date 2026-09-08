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

/// The forwarders every [`NodePat`](node_pat::NodePat) wrapper repeats
/// verbatim. `$inner` names the field, `0` for a newtype; each method is opted
/// into by name, so a builder that has to do work in one of them (`StorePat`
/// synthesises a data producer in `build`) simply leaves it out and writes its
/// own.
///
/// Only the forwarders whose DOC is the same for every builder live here.
/// `ctrl`, `mem` and `target` name a specific slot per node kind, so they stay
/// hand-written where that sentence is worth reading.
macro_rules! delegate_node_pat {
    ($ty:ty, $inner:tt, [$($m:ident),* $(,)?]) => {
        impl $ty {
            $($crate::node_builders::delegate_node_pat!(@m $inner, $m);)*
        }
    };

    (@m $inner:tt, capture) => {
        /// Binds this node to `c`, so a match reports which node matched.
        pub fn capture(mut self, c: $crate::capture::Capture) -> Self {
            self.$inner = self.$inner.capture(c);
            self
        }
    };
    (@m $inner:tt, build) => {
        /// Seals the builder into a [`Pattern`](crate::Pattern).
        pub fn build(self) -> $crate::Pattern {
            self.$inner.build()
        }
    };
    (@m $inner:tt, input) => {
        /// Raw input slot `slot`, unshifted. Slot numbering is per node kind,
        /// laid out by the IR's `expected_signature`; the named accessors are
        /// the intended surface and this is the escape hatch beneath them.
        pub fn input<P: $crate::matcher::match_pat::MatchPat + 'static>(
            mut self,
            slot: usize,
            p: P,
        ) -> Self {
            self.$inner = self.$inner.input(slot, p);
            self
        }
    };
    (@m $inner:tt, any_input) => {
        /// Matches *some* input without pinning a slot. Every input a fixed
        /// operand has not already pinned is a candidate, and the sub-pattern
        /// discriminates: a typed value sub binds only a value input, while
        /// `var` / `anything` also reaches the control and memory edges.
        /// Repeatable, each call adding one constraint; several existentials
        /// on one node take distinct slots.
        ///
        /// # Cost
        ///
        /// `k` existentials on a node with `n` inputs enumerate all
        /// `n * (n-1) * ... * (n-k+1)` injective assignments, whether or not
        /// they capture: uncaptured ones share one binding signature and
        /// collapse to a single reported match, so the cost is in
        /// configurations explored rather than results, and a guard above
        /// prunes none of them. It scales with the matched node's arity, so
        /// pin a slot with `input` where that arity is large (a wide phi, a
        /// variadic call).
        pub fn any_input<P: $crate::matcher::match_pat::MatchPat + 'static>(
            mut self,
            p: P,
        ) -> Self {
            self.$inner = self.$inner.input_any(p);
            self
        }
    };
}
pub(crate) use delegate_node_pat;

/// Defers a sub-pattern's compilation until `build`, once the shared
/// [`MatcherBuilder`] exists.
pub(crate) type SubCompiler = Box<dyn FnOnce(&mut MatcherBuilder) -> PatValueRef + Send>;

/// A sub-pattern that produces a memory token, so it can be chained into a
/// consumer's memory input slot. The lowering itself is
/// [`crate::matcher::match_pat::MatchPat::compile_mem`]; this bound is what
/// keeps a value-only pattern out of a memory slot.
pub trait MemPat: crate::matcher::match_pat::MatchPat {}
