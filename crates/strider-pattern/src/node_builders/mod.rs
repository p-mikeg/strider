//! The IR's memory side-channel (`InitialMemory -> Store -> MemPhi -> Call ->
//! Load`) is matched like the value and control chains: a memory token is a
//! real `PatValue` vertex carrying
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
        /// Matches *some* input without pinning a slot. The candidates are the
        /// inputs no fixed operand pinned, which on a commutative node is every
        /// input, its fixed operands roaming both slots. The sub-pattern
        /// discriminates: a typed value sub binds only a value input, while
        /// `var` / `anything` also reaches the control and memory edges.
        /// Repeatable, each call adding one constraint; several existentials
        /// on one node take distinct slots.
        ///
        /// # Cost
        ///
        /// `k` existentials over `n` candidate inputs enumerate all
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

type BoxedCompile = Box<dyn FnOnce(&mut MatcherBuilder) -> PatValueRef + Send>;

/// Defers a sub-pattern's compilation until `build`, once the shared
/// [`MatcherBuilder`] exists.
pub(crate) struct SubCompiler(Option<BoxedCompile>);

impl SubCompiler {
    pub(crate) fn new(f: impl FnOnce(&mut MatcherBuilder) -> PatValueRef + Send + 'static) -> Self {
        Self(Some(Box::new(f)))
    }

    /// Lowers the sub-pattern one nesting level down. Past the nesting cap it
    /// hands back a wildcard instead: the pattern is refused by then, and
    /// descending is what would abort the process.
    pub(crate) fn call(mut self, b: &mut MatcherBuilder) -> PatValueRef {
        if !b.enter_nesting() {
            return b.leaf(crate::matcher::KindSpec::Any);
        }
        let f = self.0.take().expect("a SubCompiler runs once");
        let out = f(b);
        b.leave_nesting();
        out
    }
}

impl Drop for SubCompiler {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            defer_drop(f);
        }
    }
}

thread_local! {
    static DROP_PIT: std::cell::RefCell<Vec<Box<dyn Send>>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static DRAINING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Drops `value` without recursing through the tower hanging off it: a chain
/// of boxed operands one link per nesting level, which dropped in place
/// unwinds a frame per level. A nested link's own drop lands back here and is
/// popped by the outermost call instead. Once thread-local teardown has taken
/// the pit away, `value` is dropped in place, the one option left.
pub fn defer_drop<T: Send + 'static>(value: T) {
    let mut boxed: Option<Box<dyn Send>> = Some(Box::new(value));
    let queued = DROP_PIT
        .try_with(|pit| pit.borrow_mut().push(boxed.take().expect("not yet queued")))
        .is_ok();
    drop(boxed);
    if !queued || DRAINING.replace(true) {
        return;
    }
    // A panicking drop must not leave the flag set, or every later call parks
    // its value in the pit and nothing drains it again.
    struct Draining;
    impl Drop for Draining {
        fn drop(&mut self) {
            DRAINING.set(false);
        }
    }
    let _guard = Draining;
    while let Some(link) = DROP_PIT.with_borrow_mut(Vec::pop) {
        drop(link);
    }
}

/// Renders a sealed pattern's vertices in DECLARATION order, one line per pat
/// node and one per output vertex and wired input.
///
/// Vertex order is observable: [`root_output_vertex_for`] picks among a node's
/// output vertices and [`anchor_repeats`] compares vertex identity, so a
/// lowering that declares the same vertices in another order is a behaviour
/// change, not a refactor. A builder moved onto [`NodePat`](node_pat::NodePat)
/// must reproduce this string.
///
/// [`root_output_vertex_for`]: crate::matcher::walk::root_output_vertex_for
/// [`anchor_repeats`]: crate::matcher::walk::anchor_repeats
#[cfg(test)]
pub(crate) fn staged_shape(pat: &crate::Pattern) -> String {
    use crate::matcher::{KindSpec, OutputKindSpec};
    use std::fmt::Write as _;

    let g = &pat.graph;
    let mut out = String::new();
    for n in g.all_node_ids() {
        let k = g.node_kind(n);
        let kind = match &k.kind {
            KindSpec::Any => "Any".to_string(),
            KindSpec::Variant(_) => "Variant".to_string(),
            KindSpec::Exact(e) => format!("Exact({e:?})"),
        };
        let _ = writeln!(
            out,
            "n{} {kind} caps={} pred={} alt={}/{} ordered={}",
            n.as_u32(),
            k.captures.len(),
            u8::from(k.node_predicate.is_some()),
            u8::from(k.alternation),
            u8::from(k.first_match),
            u8::from(k.force_ordered),
        );
        for &v in g.node_outputs(n) {
            let o = g.value_kind_ref(v);
            let kind = match o.kind {
                OutputKindSpec::Any => "Any",
                OutputKindSpec::AnyValue => "AnyValue",
                OutputKindSpec::Value(_) => "Value",
                OutputKindSpec::Control => "Control",
                OutputKindSpec::Memory => "Memory",
                OutputKindSpec::PhiToken => "PhiToken",
            };
            let _ = writeln!(
                out,
                "  out v{} {kind} slot={} mode={:?} width={:?} caps={} ident={} token={}",
                v.as_u32(),
                o.slot,
                o.slot_mode,
                o.width,
                o.captures.len(),
                u8::from(o.identity.is_some()),
                u8::from(o.token_fallback),
            );
        }
        for (slot, v) in k.input_slots.iter().zip(g.node_inputs(n)) {
            let _ = writeln!(out, "  in {slot:?} <- v{}", v.as_u32());
        }
    }
    let _ = writeln!(
        out,
        "root={:?} total_nodes={}",
        pat.root().map(|r| r.as_u32()).map_err(|e| e.to_string()),
        pat.total_nodes(),
    );
    out
}

/// A sub-pattern that produces a memory token, so it can be chained into a
/// consumer's memory input slot. The lowering itself is
/// [`crate::matcher::match_pat::MatchPat::compile_mem`]; this bound is what
/// keeps a value-only pattern out of a memory slot.
pub trait MemPat: crate::matcher::match_pat::MatchPat {}
