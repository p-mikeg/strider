//! Vertex weights for the bipartite pattern graph: [`PatNode`]
//! (mirrors an IR `Node`) and [`PatValue`] (mirrors a
//! `ValueData`), plus their kind specifiers.

use std::mem::Discriminant;

use strider_ir::node::{NodeId, NodeKind, ValueType};

use crate::matcher::Matcher;

/// How a [`PatNode`] constrains the kind of the IR node it matches.
pub enum KindSpec {
    /// Matches any node kind.
    Any,
    /// Matches any node sharing the given `NodeKind` discriminant
    /// (variant-agnostic, e.g. "any `IntBinaryOp`").
    Variant(Discriminant<NodeKind>),
    /// Matches a single exact `NodeKind` (variant + payload).
    Exact(NodeKind),
    /// Matches the given discriminant, then runs an extra predicate on
    /// the concrete kind.
    VariantWith {
        discriminant: Discriminant<NodeKind>,
        check: Box<dyn Fn(&NodeKind) -> bool>,
    },
}

impl KindSpec {
    /// The `NodeKind` discriminant this spec pins, if any (`None` for
    /// [`KindSpec::Any`]).  Used by the matcher's kind index to narrow
    /// candidates before a full match attempt.
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

    /// `KindSpec::Variant` from an exemplar node kind's discriminant.
    pub fn variant_of(exemplar: &NodeKind) -> Self {
        Self::Variant(std::mem::discriminant(exemplar))
    }

    /// Whether `kind` satisfies this spec.
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

/// Per-node predicate: given the matcher and the matched IR node, accept
/// or reject the match. Keyed on the node it constrains; a closure that
/// needs the node's output type derives it from the node.
pub type NodePredicate = Box<dyn Fn(&Matcher, NodeId) -> bool>;

/// Post-match constraint with visibility into the accumulated
/// bindings (distinct from the pre-recursion node predicate: it runs
/// after all inputs resolve and sees the matched node, its output type,
/// and the bindings).
pub type PostMatchFn = Box<dyn Fn(&Matcher, NodeId, ValueType, &crate::bindings::Bindings) -> bool>;

/// A pattern node vertex — mirrors an IR `Node`.
pub struct PatNode {
    /// Kind constraint on the matched node.
    pub kind: KindSpec,
    /// Optional capture binding the matched *node* (`Binding::Node`). Used
    /// only for value-less roots (`Return` / `If`) that have no output
    /// vertex to anchor a value capture on; value captures live on the
    /// producing [`PatValue`] instead.
    pub capture: Option<crate::capture::Capture>,
    /// Optional predicate on the matched node (runs before descending
    /// into inputs).
    pub node_predicate: Option<NodePredicate>,
    /// Optional post-match constraint over the bindings.
    pub post_match: Option<PostMatchFn>,
    /// When `true`, the matcher must not try commutative operand
    /// reorderings for this node.
    pub force_ordered: bool,
    /// The consumer input slot of each of this node's inputs, parallel to
    /// the generic graph's input order. The generic graph stores inputs
    /// densely (index 0, 1, …); a pattern's inputs are **sparse** (e.g.
    /// `call().arg(0, …)` wires only raw slot 4), so the original consumer
    /// slot is recorded here per input and recovered by the matcher /
    /// instantiation walk (the BiGraph-era `Consumes { slot }` edge label,
    /// re-homed onto the node payload).
    pub input_slots: Vec<usize>,
}

impl crate::graph_ext::HasInputSlots for PatNode {
    fn input_slots(&self) -> &[usize] {
        &self.input_slots
    }
}

impl PatNode {
    /// A node matching a single exact `NodeKind`.
    pub fn exact(k: NodeKind) -> Self {
        Self::from_kind(KindSpec::Exact(k))
    }

    /// A node with the given kind spec and no other constraints.
    pub fn from_kind(kind: KindSpec) -> Self {
        Self {
            kind,
            capture: None,
            node_predicate: None,
            post_match: None,
            force_ordered: false,
            input_slots: Vec::new(),
        }
    }
}

/// How a [`PatValue`] constrains the IR output it matches.
pub enum OutputKindSpec {
    /// Any output, of any kind — value, control, memory, or phi-token.
    /// The unconstrained wildcard used by `any()` / `var()`, which match
    /// any node regardless of what it produces. (A `width` constraint can
    /// still narrow it to a value output of that width.)
    Any,
    /// Any value-producing output (of any type).
    AnyValue,
    /// A value output pinned to an exact type. The unpinned "any value"
    /// case is [`AnyValue`](Self::AnyValue), not `Value` with a sentinel.
    Value(ValueType),
    /// A control-flow output.
    Control,
    /// The memory-token output.
    Memory,
    /// A phi-token output.
    PhiToken,
}

/// A pattern output vertex — mirrors a `ValueData`.
pub struct PatValue {
    /// The output slot index on the producing node.
    pub slot: usize,
    /// Kind constraint on the matched output.
    pub kind: OutputKindSpec,
    /// Optional bit-width constraint on the matched output's value
    /// type.
    pub width: Option<u32>,
    /// Optional capture binding the matched output value (`Binding::Value`).
    ///
    /// This is where value captures live — `add(var(x), …)` captures `x`'s
    /// matched *value*. The matcher reads it when matching this output
    /// vertex (see `walk::try_match_at`), taking precedence over the
    /// producing node's [`PatNode::capture`] (which covers only value-less
    /// roots).
    pub capture: Option<crate::capture::Capture>,
}

impl PatValue {
    /// A value output at `slot` with no type / width constraint.
    pub fn value(slot: usize) -> Self {
        Self {
            slot,
            kind: OutputKindSpec::AnyValue,
            width: None,
            capture: None,
        }
    }

    /// A control-flow output at `slot`.
    pub fn control(slot: usize) -> Self {
        Self {
            slot,
            kind: OutputKindSpec::Control,
            width: None,
            capture: None,
        }
    }

    /// A memory-token output at `slot`. Models the IR's memory side
    /// channel (`InitialMemory` / `Store` / `MemPhi` / `Call` produce a
    /// memory token that a later `Load` / `Store` consumes).
    pub fn memory(slot: usize) -> Self {
        Self {
            slot,
            kind: OutputKindSpec::Memory,
            width: None,
            capture: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_value_output_is_any_value() {
        // A value output with no pinned type is `AnyValue`, not a
        // `Value(_)` carrying an optional type — there is no redundant
        // "value, unconstrained" spelling.
        assert!(matches!(PatValue::value(0).kind, OutputKindSpec::AnyValue));
    }

    #[test]
    fn predicates_are_keyed_by_their_entity_no_value_type_arg() {
        // A node predicate constrains the matched node: (&Matcher, NodeId).
        // It takes no redundant ValueType — a closure derives it if needed.
        let _node: NodePredicate = Box::new(|_m, _node| true);
    }
}
