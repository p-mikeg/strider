pub mod flow;
pub mod function_arg;
pub mod memory;
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

/// Forwarders shared by every [`Node`](crate::expr::Node) wrapper, opted into by
/// name. `$inner` names the field (`0` for a newtype); a trailing `with_output`
/// implements [`WithOutput`].
macro_rules! delegate_node_pat {
    ($ty:ty, $inner:tt, [$($m:ident),* $(,)?] $(, $extra:ident)?) => {
        impl $ty {
            $($crate::node_builders::delegate_node_pat!(@m $inner, $m);)*
        }
        $($crate::node_builders::delegate_node_pat!(@$extra $ty, $inner);)?
    };

    (@with_output $ty:ty, $inner:tt) => {
        impl $crate::node_builders::WithOutput for $ty {
            fn with_output(mut self, slot: Option<usize>, aspect: $crate::OutputAspect) -> Self {
                self.$inner = self.$inner.output(slot, aspect);
                self
            }
        }
    };

    (@m $inner:tt, capture) => {
        /// Binds the anchor output to `c`, or the node when it has no anchor.
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
        /// Raw input slot `slot`, unshifted, per the IR's `expected_signature`.
        /// A `call` / `call_other` there binds any of its outputs.
        pub fn input<P: $crate::matcher::match_pat::MatchPat + 'static>(
            mut self,
            slot: usize,
            p: P,
        ) -> Self {
            self.$inner = self.$inner.raw_input(slot, p);
            self
        }
    };
    (@m $inner:tt, ctrl) => {
        /// Input [`NodeKind::CTRL_SLOT`](strider_ir::node::NodeKind::CTRL_SLOT),
        /// a control edge.
        pub fn ctrl<P: $crate::matcher::match_pat::MatchPat + 'static>(mut self, p: P) -> Self {
            self.$inner = self
                .$inner
                .input_control(strider_ir::node::NodeKind::CTRL_SLOT, p);
            self
        }
    };
    (@m $inner:tt, any_input) => {
        /// Matches some input no fixed operand pinned.
        /// Repeatable; several on one node take distinct slots, so `k` of them over `n` inputs
        /// explore `n!/(n-k)!` assignments.
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

/// Implements [`IntoExpr`](crate::IntoExpr) and `MatchPat` for each
/// builder, `$node` its configured node.
macro_rules! node_match_pat {
    ($($ty:ty => |$p:ident| $node:expr),* $(,)?) => {$(
        impl $crate::expr::IntoExpr for $ty {
            fn into_expr(self) -> $crate::Expr {
                let $p = self;
                $node.into()
            }
        }

        impl $crate::MatchPat for $ty {}
    )*};
}
pub(crate) use node_match_pat;

thread_local! {
    static DROP_PIT: std::cell::RefCell<Vec<Box<dyn Send>>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static DRAINING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Drops `value` iteratively rather than one stack frame per nesting level: a
/// nested link's drop is queued and popped by the outermost call. During
/// thread-local teardown, `value` is dropped in place.
pub fn defer_drop<T: Send + 'static>(value: T) {
    let mut boxed: Option<Box<dyn Send>> = Some(Box::new(value));
    let queued = DROP_PIT
        .try_with(|pit| pit.borrow_mut().push(boxed.take().expect("not yet queued")))
        .is_ok();
    drop(boxed);
    if !queued || DRAINING.replace(true) {
        return;
    }
    // Reset on panic, or later values would park in the pit undrained.
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

/// Renders a pattern's vertices in declaration order, which matching observes
/// ([`root_output_vertex_for`], [`anchor_repeats`]).
///
/// [`root_output_vertex_for`]: crate::matcher::walk::root_output_vertex_for
/// [`anchor_repeats`]: crate::matcher::walk::anchor_repeats
#[cfg(test)]
pub(crate) fn pattern_shape(pat: &crate::Pattern) -> String {
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
            "n{} {kind} caps={} pred={} {:?}",
            n.as_u32(),
            k.captures.len(),
            k.node_predicates.len(),
            k.role,
        );
        for &v in g.node_outputs(n) {
            let o = g.value_kind_ref(v);
            let kind = match o.kind {
                OutputKindSpec::Any => "Any",
                OutputKindSpec::AnyValue => "AnyValue",
                OutputKindSpec::Value(_) => "Value",
                OutputKindSpec::Control => "Control",
                OutputKindSpec::Memory => "Memory",
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

/// A sub-pattern that produces a memory token, accepted in a memory input slot.
pub trait MemPat: crate::matcher::match_pat::MatchPat {}
