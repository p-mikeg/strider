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
            Self::Variant(d) | Self::VariantWith { discriminant: d, .. } => Some(*d),
            Self::Exact(k) => Some(std::mem::discriminant(k)),
        }
    }

    /// Whether `kind` satisfies this spec.
    pub fn matches(&self, kind: &NodeKind) -> bool {
        match self {
            Self::Any => true,
            Self::Variant(d) => *d == std::mem::discriminant(kind),
            Self::Exact(k) => k == kind,
            Self::VariantWith { discriminant, check } => {
                *discriminant == std::mem::discriminant(kind) && check(kind)
            }
        }
    }
}

/// Per-node local constraint: given the matched IR node + its output
/// type, accept or reject the match.
pub type LocalLimit = Box<dyn Fn(&Matcher, NodeId, ValueType) -> bool>;

/// Post-match constraint with visibility into the accumulated
/// bindings.
pub type PostMatchFn = Box<dyn Fn(&Matcher, NodeId, ValueType, &crate::bindings::Bindings) -> bool>;

/// A pattern node vertex — mirrors an IR `Node`.
pub struct PatNode {
    /// Kind constraint on the matched node.
    pub kind: KindSpec,
    /// Optional capture binding the matched node.
    pub capture: Option<crate::capture::Capture>,
    /// Optional local constraint on the matched node.
    pub node_limit: Option<LocalLimit>,
    /// Optional post-match constraint over the bindings.
    pub post_match: Option<PostMatchFn>,
    /// When `true`, the matcher must not try commutative operand
    /// reorderings for this node.
    pub force_ordered: bool,
}

impl PatNode {
    /// A node matching any IR node kind.
    pub fn wildcard() -> Self {
        Self::from_kind(KindSpec::Any)
    }

    /// A node matching a single exact `NodeKind`.
    pub fn exact(k: NodeKind) -> Self {
        Self::from_kind(KindSpec::Exact(k))
    }

    /// A node with the given kind spec and no other constraints.
    pub fn from_kind(kind: KindSpec) -> Self {
        Self {
            kind,
            capture: None,
            node_limit: None,
            post_match: None,
            force_ordered: false,
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
    /// Any value-producing output.
    AnyValue,
    /// A value output, optionally pinned to an exact type.
    Value(Option<ValueType>),
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
    /// Optional local constraint on the matched output.
    ///
    /// Read by the engine, but no builder setter wires it yet — reserved
    /// for the typed/wildcard layer.
    pub output_limit: Option<LocalLimit>,
    /// Optional capture binding the matched output.
    ///
    /// The current engine binds captures on the producing `PatNode`
    /// (value captures already receive a `Binding::Value`), so
    /// output-vertex captures are reserved for a later API layer and not
    /// yet honored by the matcher.
    pub capture: Option<crate::capture::Capture>,
}

impl PatValue {
    /// A value output at `slot` with no type / width constraint.
    pub fn value(slot: usize) -> Self {
        Self {
            slot,
            kind: OutputKindSpec::Value(None),
            width: None,
            output_limit: None,
            capture: None,
        }
    }

    /// A control-flow output at `slot`.
    pub fn control(slot: usize) -> Self {
        Self {
            slot,
            kind: OutputKindSpec::Control,
            width: None,
            output_limit: None,
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
            output_limit: None,
            capture: None,
        }
    }

}
