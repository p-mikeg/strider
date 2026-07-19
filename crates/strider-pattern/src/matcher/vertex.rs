//! Vertex weights for the bipartite pattern graph: [`PatNode`] mirrors an IR
//! `Node`, [`PatValue`] a `ValueData`.

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
    VariantWith {
        discriminant: Discriminant<NodeKind>,
        check: Box<dyn Fn(&NodeKind) -> bool>,
    },
}

impl KindSpec {
    /// The discriminant this spec pins, if any. The matcher's kind index uses
    /// it to narrow candidates before attempting a full match.
    pub fn discriminant(&self) -> Option<Discriminant<NodeKind>> {
        match self {
            Self::Any => None,
            Self::Variant(d)
            | Self::VariantWith {
                discriminant: d, ..
            } => Some(*d),
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
            Self::VariantWith {
                discriminant,
                check,
            } => *discriminant == std::mem::discriminant(kind) && check(kind),
        }
    }
}

/// Accept or reject the matched IR node. Takes no output type; a closure that
/// needs one derives it from the node.
pub type NodePredicate = Box<dyn Fn(&Matcher, NodeId) -> bool>;

/// Unlike [`NodePredicate`], runs after all inputs resolve and sees the
/// accumulated bindings.
pub type PostMatchFn = Box<dyn Fn(&Matcher, NodeId, ValueType, &crate::bindings::Bindings) -> bool>;

pub struct PatNode {
    pub kind: KindSpec,
    /// Binds the matched *node* (`Binding::Node`). Only for value-less roots
    /// (`Return` / `If`) with no output vertex to anchor a value capture on.
    pub capture: Option<crate::capture::Capture>,
    /// Runs before descending into inputs.
    pub node_predicate: Option<NodePredicate>,
    pub post_match: Option<PostMatchFn>,
    /// Suppresses commutative operand reordering for this node.
    pub force_ordered: bool,
    /// Marks a `one_of` node: its inputs are independent alternative
    /// sub-patterns tried against the *same* IR node, not operands. First
    /// match wins, with the usual backtracking. Its own kind is
    /// [`KindSpec::Any`]; the alternatives carry the real kind checks.
    pub alternation: bool,
    /// Consumer input slot per input, parallel to the generic graph's input
    /// order. The generic graph stores inputs densely, but a pattern's inputs
    /// are sparse (`call().arg(0, ..)` wires only raw slot 4), so the original
    /// slot is recorded here and recovered by the matcher / instantiation walk.
    pub input_slots: Vec<usize>,
}

impl crate::graph_ext::HasInputSlots for PatNode {
    fn input_slots(&self) -> &[usize] {
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
            capture: None,
            node_predicate: None,
            post_match: None,
            force_ordered: false,
            alternation: false,
            input_slots: Vec::new(),
        }
    }
}

pub enum OutputKindSpec {
    /// Value, control, memory, or phi-token. The unconstrained wildcard behind
    /// `any()` / `var()`. A `width` constraint can still narrow it to a value
    /// output of that width.
    Any,
    AnyValue,
    Value(ValueType),
    Control,
    Memory,
    PhiToken,
}

pub struct PatValue {
    /// Structural anchor position only, NOT a match filter; see `match_slot`.
    pub slot: usize,
    pub kind: OutputKindSpec,
    pub width: Option<u32>,
    /// Enforced producer output-slot constraint. `None` leaves the slot
    /// unchecked, so any output kind-ok against `kind` matches; that is what
    /// lets a nested Call/CallOther value operand match any value output.
    /// `Some(s)` pins the slot, as `call_other().res()` does to select the
    /// declared result and exclude clobbers.
    pub match_slot: Option<usize>,
    /// Where value captures live: `add(var(x), ..)` binds `x` here. Takes
    /// precedence over the producing node's [`PatNode::capture`], which covers
    /// only value-less roots.
    pub capture: Option<crate::capture::Capture>,
}

impl PatValue {
    /// No type or width constraint.
    pub fn value(slot: usize) -> Self {
        Self {
            slot,
            kind: OutputKindSpec::AnyValue,
            width: None,
            match_slot: None,
            capture: None,
        }
    }

    pub fn control(slot: usize) -> Self {
        Self {
            slot,
            kind: OutputKindSpec::Control,
            width: None,
            match_slot: None,
            capture: None,
        }
    }

    /// The IR's memory side channel: `InitialMemory` / `Store` / `MemPhi` /
    /// `Call` produce a token a later `Load` / `Store` consumes.
    pub fn memory(slot: usize) -> Self {
        Self {
            slot,
            kind: OutputKindSpec::Memory,
            width: None,
            match_slot: None,
            capture: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_value_output_is_any_value() {
        // No pinned type means `AnyValue`, not `Value(Option<_>)`.
        assert!(matches!(PatValue::value(0).kind, OutputKindSpec::AnyValue));
    }

    #[test]
    fn predicates_are_keyed_by_their_entity_no_value_type_arg() {
        // No ValueType arg; a closure derives it from the node if needed.
        let _node: NodePredicate = Box::new(|_m, _node| true);
    }
}
