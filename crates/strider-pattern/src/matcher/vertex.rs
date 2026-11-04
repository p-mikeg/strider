use std::mem::Discriminant;

use strider_ir::node::{NodeId, NodeKind, ValueType};

use crate::matcher::Matcher;

#[derive(PartialEq, Eq)]
pub enum KindSpec {
    Any,
    /// Any node sharing the discriminant, e.g. any `IntBinaryOp`.
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

    /// Whether the spec names the variant of one of `kinds`; never for `Any`.
    pub(crate) fn is_variant_in(&self, kinds: &[NodeKind]) -> bool {
        self.discriminant()
            .is_some_and(|d| kinds.iter().any(|k| std::mem::discriminant(k) == d))
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

/// Accept or reject the matched IR node. May run more than once per candidate, and on nodes the
/// full match rejects (operands are screened before the root is matched).
pub type NodePredicate = Box<dyn Fn(&Matcher, NodeId) -> bool + Send>;

/// Runs once the node's inputs have matched. Below the root it sees the
/// captures of its own cone and of operands written before it. The type is the
/// matched output's, `None` for a control, memory or `PhiToken` edge or a
/// zero-output node.
pub type PostMatchFn =
    Box<dyn Fn(&Matcher, NodeId, Option<ValueType>, &crate::bindings::Bindings) -> bool + Send>;

/// Where a pattern input sits on its consumer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputSlot {
    /// One IR input slot.
    At(usize),
    /// Any input not taken by a fixed operand (`any_input`), of the sub-pattern's output kind;
    /// a wildcard reaches every slot.
    Any,
}

pub(crate) struct PatNode {
    pub(crate) kind: KindSpec,
    /// Each binds the matched node (`Binding::Node`); for roots with no value output to capture.
    pub(crate) captures: Vec<crate::capture::Capture>,
    /// Run before descending into inputs; all must hold.
    pub(crate) node_predicates: Vec<NodePredicate>,
    pub(crate) post_matches: Vec<PostMatchFn>,
    pub(crate) role: Role,
    /// The IR input slot of each pattern input, in graph input order (inputs are sparse).
    pub(crate) input_slots: Vec<InputSlot>,
}

/// How a node's inputs are read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    /// Operands; `ordered` suppresses commutative reordering.
    Operands { ordered: bool },
    /// `one_of`: alternative sub-patterns tried against the same IR node, every matching arm kept.
    OneOf,
    /// `first_of`: [`Role::OneOf`] kept to the first arm that matches.
    FirstOf,
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
            node_predicates: Vec::new(),
            post_matches: Vec::new(),
            role: Role::Operands { ordered: false },
            input_slots: Vec::new(),
        }
    }

    pub(crate) fn is_alternation(&self) -> bool {
        !matches!(self.role, Role::Operands { .. })
    }
}

#[derive(PartialEq)]
pub(crate) enum OutputKindSpec {
    /// Any output, including phi-token: `anything()` / `var()`.
    Any,
    AnyValue,
    Value(ValueType),
    Control,
    Memory,
}

/// How a value vertex reads its `slot`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SlotMode {
    /// As an anchor (root's sole vertex, or an operand's producer), the output being matched; as
    /// a sibling, `slot`.
    Loose,
    /// The matched output must be produced at `slot`.
    Pinned,
    /// `any_output()`: each of the node's outputs in turn.
    Enumerated,
}

pub(crate) struct PatValue {
    pub(crate) slot: usize,
    /// What `slot` means. Sibling `Enumerated` vertices may settle on the same output.
    pub(crate) slot_mode: SlotMode,
    pub(crate) kind: OutputKindSpec,
    /// A width of any value type, which no `kind` arm spells.
    pub(crate) width: Option<u32>,
    /// Each binds the matched value.
    pub(crate) captures: Vec<crate::capture::Capture>,
    /// Widens an `AnyValue` vertex to also accept the memory token of a node
    /// with no value output. Set on a pattern root's output (and its
    /// alternation arms), where nothing consumes the edge.
    pub(crate) token_fallback: bool,
    /// An internal capture pinning a vertex consumed at several slots to one IR value (each slot
    /// is matched on its own). Filtered out of a `Match`.
    pub(crate) identity: Option<crate::capture::Capture>,
}

impl PatValue {
    /// Unconstrained but for the kind.
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
