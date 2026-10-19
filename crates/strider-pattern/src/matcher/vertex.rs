use std::mem::Discriminant;

use strider_ir::node::{NodeId, NodeKind, ValueType};

use crate::matcher::Matcher;

pub enum KindSpec {
    Any,
    /// Variant-agnostic: any node sharing the discriminant, e.g. "any
    /// `IntBinaryOp`".
    Variant(Discriminant<NodeKind>),
    /// Variant and payload.
    Exact(NodeKind),
}

impl KindSpec {
    pub fn discriminant(&self) -> Option<Discriminant<NodeKind>> {
        match self {
            Self::Any => None,
            Self::Variant(d) => Some(*d),
            Self::Exact(k) => Some(std::mem::discriminant(k)),
        }
    }

    pub fn variant_of(exemplar: &NodeKind) -> Self {
        Self::Variant(std::mem::discriminant(exemplar))
    }

    pub fn matches(&self, kind: &NodeKind) -> bool {
        match self {
            Self::Any => true,
            Self::Variant(d) => *d == std::mem::discriminant(kind),
            Self::Exact(k) => k == kind,
        }
    }
}

/// Accept or reject the matched IR node. Takes no output type; a closure that
/// needs one derives it from the node.
pub type NodePredicate = Box<dyn Fn(&Matcher, NodeId) -> bool + Send>;

/// Runs once every input has resolved, so it sees the accumulated bindings.
/// The type is that of the matched output, `None` where there is none: a
/// control, memory or `PhiToken` edge, or a zero-output node.
pub type PostMatchFn =
    Box<dyn Fn(&Matcher, NodeId, Option<ValueType>, &crate::bindings::Bindings) -> bool + Send>;

/// A sideways sub-walk run after [`PostMatchFn`], matching against the LIVE
/// journal so what it binds must agree with the enclosing match and survives
/// into it.
///
/// Continuation-passing like the main engine: the walk calls `k` once per
/// configuration it reaches, `true` accepting and stopping, `false` driving the
/// next. Returning `false` overall fails the configuration.
pub type BindingWalkFn = Box<
    dyn Fn(
            &Matcher,
            NodeId,
            &mut crate::bindings::Bindings,
            &mut dyn FnMut(&mut crate::bindings::Bindings) -> bool,
        ) -> bool
        + Send,
>;

/// What a [`BindingWalkFn`]'s sub-pattern binds. The sub-pattern's graph is
/// held inside the closure rather than wired into the enclosing pattern, so
/// [`Pattern::bound_captures`](crate::Pattern::bound_captures) and
/// [`guaranteed_captures`](crate::Pattern::guaranteed_captures) read it from
/// here.
#[derive(Default)]
pub struct WalkCaptures {
    pub bound: Vec<crate::capture::Capture>,
    /// Bound on every successful walk.
    pub guaranteed: Vec<crate::capture::Capture>,
}

/// Where a pattern input sits on its consumer: one IR input slot, or any of
/// them (`any_input`). A sub reaches the slots of its own output kind; a
/// wildcard reaches any input, control / memory / `PhiToken` slots included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputSlot {
    At(usize),
    Any,
}

pub struct PatNode {
    pub kind: KindSpec,
    /// Binds the matched *node* (`Binding::Node`). Only for value-less roots
    /// (`Return`, `IndirectBranch`, `Switch`, `Unreachable`, `If`) with no
    /// value output vertex to anchor a value capture on. Repeatable: every
    /// listed capture binds that node.
    pub captures: Vec<crate::capture::Capture>,
    /// Runs before descending into inputs.
    pub node_predicate: Option<NodePredicate>,
    pub post_match: Option<PostMatchFn>,
    /// A `when_match` guard sits in `post_match`, so retyping this node's
    /// output to `Control` has to re-run the refusal that guard was admitted
    /// under.
    pub typed_post_match: bool,
    /// Runs after `post_match`; may bind.
    pub binding_walk: Option<BindingWalkFn>,
    /// Declares what `binding_walk` binds.
    pub walk_captures: WalkCaptures,
    /// Suppresses commutative operand reordering for this node.
    pub force_ordered: bool,
    /// Marks a `one_of` / `first_of` node: its inputs are independent
    /// alternative sub-patterns tried against the *same* IR node, not operands.
    pub alternation: bool,
    /// On an alternation node, `true` cuts to the first arm that matches
    /// (`first_of`); `false` enumerates every matching arm (`one_of`, a union).
    pub first_match: bool,
    /// Consumer input slot per input, parallel to the generic graph's input
    /// order. A pattern's inputs are sparse (`call().arg(0, ..)` wires only
    /// raw slot 4), so the original slot is recorded here.
    pub input_slots: Vec<InputSlot>,
}

impl crate::graph_ext::HasInputSlots for PatNode {
    type Slot = InputSlot;
    fn input_slots(&self) -> &[InputSlot] {
        &self.input_slots
    }
}

impl PatNode {
    pub fn exact(k: NodeKind) -> Self {
        Self::from_kind(KindSpec::Exact(k))
    }

    /// No constraints beyond the kind spec.
    pub fn from_kind(kind: KindSpec) -> Self {
        Self {
            kind,
            captures: Vec::new(),
            node_predicate: None,
            post_match: None,
            typed_post_match: false,
            binding_walk: None,
            walk_captures: WalkCaptures::default(),
            force_ordered: false,
            alternation: false,
            first_match: false,
            input_slots: Vec::new(),
        }
    }
}

pub enum OutputKindSpec {
    /// Value, control, memory, or phi-token: the unconstrained wildcard behind
    /// `anything()` / `var()`. A `width` constraint can still narrow it to a value
    /// output of that width.
    Any,
    AnyValue,
    Value(ValueType),
    Control,
    Memory,
    PhiToken,
}

/// How a value vertex reads its `slot`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotMode {
    /// An anchor position only: as a root's sole vertex it takes the output
    /// being matched, whatever its slot; as a sibling it sits at `slot`.
    Loose,
    /// The matched output must be produced at `slot`.
    Pinned,
    /// `any_output()`: each of the node's outputs in turn.
    Enumerated,
}

pub struct PatValue {
    pub slot: usize,
    /// What `slot` means. Sibling `Enumerated` vertices on one node are NOT
    /// injective, unlike an existential input: each enumerates every output
    /// independently, so two of them can settle on the same one.
    pub slot_mode: SlotMode,
    pub kind: OutputKindSpec,
    pub width: Option<u32>,
    /// Where value captures live: `int_add(var(x), ..)` binds `x` here.
    /// Repeatable: `var(x).capture(y)` binds both to the matched value.
    pub captures: Vec<crate::capture::Capture>,
    /// Widens an `AnyValue` vertex to also accept the memory token of a node
    /// that produces no value output. Set only on an alternation arm at the
    /// root, where nothing consumes the edge.
    pub token_fallback: bool,
    /// A crate-minted capture pinning ONE vertex consumed at several pattern
    /// slots to a single IR value. Structural sharing alone does not do it:
    /// each slot is matched on its own, so `float_eq(x, x)` would otherwise
    /// accept two different operands. Never a caller's capture, and filtered
    /// out of a `Match`.
    pub identity: Option<crate::capture::Capture>,
}

impl PatValue {
    /// Unconstrained but for the kind, which is what separates the three
    /// constructors below; every other field starts empty.
    fn of_kind(slot: usize, kind: OutputKindSpec) -> Self {
        Self {
            slot,
            kind,
            width: None,
            slot_mode: SlotMode::Loose,
            captures: Vec::new(),
            token_fallback: false,
            identity: None,
        }
    }

    /// No type or width constraint.
    pub fn value(slot: usize) -> Self {
        Self::of_kind(slot, OutputKindSpec::AnyValue)
    }

    pub fn control(slot: usize) -> Self {
        Self::of_kind(slot, OutputKindSpec::Control)
    }

    /// The IR's memory side channel.
    pub fn memory(slot: usize) -> Self {
        Self::of_kind(slot, OutputKindSpec::Memory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_value_output_is_any_value() {
        assert!(matches!(PatValue::value(0).kind, OutputKindSpec::AnyValue));
    }
}
