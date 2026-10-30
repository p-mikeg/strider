use std::mem::Discriminant;

use strider_ir::node::{NodeId, NodeKind, ValueType};

use crate::matcher::Matcher;

#[derive(PartialEq, Eq)]
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

/// Where a pattern input sits on its consumer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputSlot {
    /// One IR input slot.
    At(usize),
    /// Any input not taken by a fixed operand (`any_input`). A sub reaches the
    /// slots of its own output kind; a wildcard reaches any input, control /
    /// memory / `PhiToken` slots included.
    Any,
}

pub(crate) struct PatNode {
    pub(crate) kind: KindSpec,
    /// Binds the matched *node* (`Binding::Node`). Only for value-less roots
    /// (`Return`, `IndirectBranch`, `Switch`, `Unreachable`, `If`) with no
    /// value output vertex to anchor a value capture on. Repeatable: every
    /// listed capture binds that node.
    pub(crate) captures: Vec<crate::capture::Capture>,
    /// Runs before descending into inputs.
    pub(crate) node_predicate: Option<NodePredicate>,
    pub(crate) post_match: Option<PostMatchFn>,
    /// A `when_match` guard sits in `post_match`, so retyping this node's
    /// output to `Control` has to re-run the refusal that guard was admitted
    /// under.
    pub(crate) typed_post_match: bool,
    /// Suppresses commutative operand reordering for this node.
    pub(crate) force_ordered: bool,
    /// Marks a `one_of` / `first_of` node: its inputs are independent
    /// alternative sub-patterns tried against the *same* IR node, not operands.
    pub(crate) alternation: bool,
    /// On an alternation node, `true` cuts to the first arm that matches
    /// (`first_of`); `false` enumerates every matching arm (`one_of`, a union).
    pub(crate) first_match: bool,
    /// Consumer input slot per input, parallel to the generic graph's input
    /// order. A pattern's inputs are sparse (`call().arg(0, ..)` wires only
    /// raw slot 4), so the original slot is recorded here.
    pub(crate) input_slots: Vec<InputSlot>,
}

impl crate::graph_ext::HasInputSlots for PatNode {
    type Slot = InputSlot;
    fn input_slots(&self) -> &[InputSlot] {
        &self.input_slots
    }
}

impl PatNode {
    pub(crate) fn exact(k: NodeKind) -> Self {
        Self::from_kind(KindSpec::Exact(k))
    }

    /// No constraints beyond the kind spec.
    pub(crate) fn from_kind(kind: KindSpec) -> Self {
        Self {
            kind,
            captures: Vec::new(),
            node_predicate: None,
            post_match: None,
            typed_post_match: false,
            force_ordered: false,
            alternation: false,
            first_match: false,
            input_slots: Vec::new(),
        }
    }
}

/// Public for the `PhiToken` arm alone: nothing builds such a vertex yet, and
/// narrowing this would make the variant dead rather than reserved.
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
pub(crate) enum SlotMode {
    /// An anchor position only: as an anchor (a root's sole vertex, or an
    /// operand's producer) it takes the output being matched, whatever its
    /// slot; as a sibling it sits at `slot`.
    Loose,
    /// The matched output must be produced at `slot`.
    Pinned,
    /// `any_output()`: each of the node's outputs in turn.
    Enumerated,
}

pub(crate) struct PatValue {
    pub(crate) slot: usize,
    /// What `slot` means. Sibling `Enumerated` vertices on one node are NOT
    /// injective, unlike an existential input: each enumerates every output
    /// independently, so two of them can settle on the same one.
    pub(crate) slot_mode: SlotMode,
    pub(crate) kind: OutputKindSpec,
    /// Not implied by `kind`: `Value(ValueType)` fixes int-vs-float too, while
    /// `value_of_width(32)` means "any 32-bit value", which no `kind` arm
    /// spells. `root_requires_value_output` reads the two independently.
    pub(crate) width: Option<u32>,
    /// Where value captures live: `int_add(var(x), ..)` binds `x` here.
    /// Repeatable: `var(x).capture(y)` binds both to the matched value.
    pub(crate) captures: Vec<crate::capture::Capture>,
    /// Widens an `AnyValue` vertex to also accept the memory token of a node
    /// that produces no value output. Set only on an alternation arm at the
    /// root, where nothing consumes the edge.
    pub(crate) token_fallback: bool,
    /// A crate-minted capture pinning ONE vertex consumed at several pattern
    /// slots to a single IR value. Structural sharing alone does not do it:
    /// each slot is matched on its own, so `float_eq(x, x)` would otherwise
    /// accept two different operands. Never a caller's capture, and filtered
    /// out of a `Match`.
    pub(crate) identity: Option<crate::capture::Capture>,
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
    pub(crate) fn value(slot: usize) -> Self {
        Self::of_kind(slot, OutputKindSpec::AnyValue)
    }

    pub(crate) fn control(slot: usize) -> Self {
        Self::of_kind(slot, OutputKindSpec::Control)
    }

    /// The IR's memory side channel.
    pub(crate) fn memory(slot: usize) -> Self {
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
