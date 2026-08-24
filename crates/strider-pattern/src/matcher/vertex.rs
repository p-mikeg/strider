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

/// Runs once every input has resolved, so it sees the accumulated bindings.
pub type PostMatchFn = Box<dyn Fn(&Matcher, NodeId, ValueType, &crate::bindings::Bindings) -> bool>;

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
    ) -> bool,
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

pub struct PatNode {
    pub kind: KindSpec,
    /// Binds the matched *node* (`Binding::Node`). Only for value-less roots
    /// (`Return`, `IndirectBranch`, `Switch`, `Unreachable`, `If`) with no
    /// value output vertex to anchor a value capture on.
    pub capture: Option<crate::capture::Capture>,
    /// Runs before descending into inputs.
    pub node_predicate: Option<NodePredicate>,
    pub post_match: Option<PostMatchFn>,
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

pub struct PatValue {
    /// Structural anchor position only, NOT a match filter; see `match_slot`.
    pub slot: usize,
    pub kind: OutputKindSpec,
    pub width: Option<u32>,
    /// Enforced producer output-slot constraint. `None` leaves the slot
    /// unchecked, so any output kind-ok against `kind` matches; `Some(s)` pins
    /// the slot.
    pub match_slot: Option<usize>,
    /// `any_output()`: satisfied by ANY of the node's outputs, enumerated like
    /// an existential input rather than pinned to `slot`.
    pub any_slot: bool,
    /// Where value captures live: `int_add(var(x), ..)` binds `x` here.
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
            any_slot: false,
            capture: None,
        }
    }

    pub fn control(slot: usize) -> Self {
        Self {
            slot,
            kind: OutputKindSpec::Control,
            width: None,
            match_slot: None,
            any_slot: false,
            capture: None,
        }
    }

    /// The IR's memory side channel.
    pub fn memory(slot: usize) -> Self {
        Self {
            slot,
            kind: OutputKindSpec::Memory,
            width: None,
            match_slot: None,
            any_slot: false,
            capture: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_value_output_is_any_value() {
        assert!(matches!(PatValue::value(0).kind, OutputKindSpec::AnyValue));
    }

    #[test]
    fn predicates_are_keyed_by_their_entity_no_value_type_arg() {
        let _node: NodePredicate = Box::new(|_m, _node| true);
    }
}
