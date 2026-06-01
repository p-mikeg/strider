//! Per-node and per-edge payloads for `PatGraph`.

use std::mem::Discriminant;
use strider_ir::node::{NodeId, NodeKind, NodeOutputType};

use crate::bindings::Bindings;
use crate::matcher::Matcher;

/// Kind-level constraint on a pattern node.  Ported from
/// `strider-analyze::pattern::pat::node_pat::KindSpec` — closures are
/// `Box<dyn Fn>` (single-threaded; no Arc / Rc / Send / Sync in this
/// crate's public surface).  Move-only.  Any refcounting / reuse the
/// Python wrapper needs lives there, not here.
pub enum KindSpec {
    /// Accepts any `NodeKind`.
    Any,
    /// Matches a `NodeKind` variant by discriminant, ignoring payload.
    Variant(Discriminant<NodeKind>),
    /// Matches a `NodeKind` value exactly (discriminant + payload equality).
    Exact(NodeKind),
    /// Variant match plus a payload-only predicate.
    VariantWith {
        discriminant: Discriminant<NodeKind>,
        check: Box<dyn Fn(&NodeKind) -> bool>,
    },
}

impl KindSpec {
    /// Returns the unique `NodeKind` discriminant this spec accepts,
    /// or `None` for [`KindSpec::Any`].
    #[must_use]
    pub fn discriminant(&self) -> Option<Discriminant<NodeKind>> {
        match self {
            Self::Any => None,
            Self::Variant(d) | Self::VariantWith { discriminant: d, .. } => Some(*d),
            Self::Exact(k) => Some(std::mem::discriminant(k)),
        }
    }
    /// Full check: discriminant + payload.
    #[must_use]
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

/// Per-node payload stored as `StableDiGraph` node weight.  Move-only:
/// closure-bearing fields are `Box<dyn Fn>` (single-threaded; no Arc /
/// Rc / Send / Sync).  A lossy structural clone exists for the small
/// set of builders (`float_le`, `float_is_nan`) that need to reference
/// the same operand twice — see `crate::pat_graph::clone_lossy`.
pub struct NodeData {
    pub kind: KindSpec,
    pub output_ty: Option<NodeOutputType>,
    /// The capture variable this pattern node binds (`Some`) or no
    /// binding (`None`).  Records "match against this pat node ⇒ bind
    /// capture `c`".
    pub capture: Option<crate::capture::Capture>,
    /// Pre-match filter.  Fires AFTER the kind + `output_ty` check
    /// passes and BEFORE the recursive matcher walks into child
    /// inputs.  No bindings are available — children haven't matched
    /// yet.  Use for node-only predicates (output width, varnode tag,
    /// side-table lookups) that should short-circuit before paying for
    /// child recursion; for predicates that need cross-binding state,
    /// see [`PostMatchFn`].
    pub node_filter: Option<NodeFilterFn>,
    pub post_match: Option<PostMatchFn>,
    pub template_spec: Option<TemplateSpec>,
    /// When `true`, the matcher MUST NOT trigger commutative-operand
    /// retry on this node even if `NodeKind::is_commutative()` would
    /// allow it.  Default `false` — the matcher honours commutativity.
    pub force_ordered: bool,
}

/// Pre-match filter closure.  Fires right after the kind + `output_ty`
/// check passes and BEFORE the recursive matcher walks into child
/// inputs.  Returning `false` rejects the match.  No bindings are
/// available — children haven't matched yet, so this hook is the
/// fastest short-circuit for node-only predicates (width, varnode
/// tag, side-table lookups).
///
/// Arguments:
/// - `matcher` — the active [`Matcher`]; reach the function under
///   inspection via [`Matcher::function`].
/// - `node` — the matched IR `NodeId`.
/// - `ty` — the matched IR output's `NodeOutputType` (zero-output
///   match sites pass `NodeOutputType::I1` as a placeholder).
///
/// `Box` (single-threaded; no Arc / Rc / Send / Sync in this crate's
/// public surface).  Move-only.
pub type NodeFilterFn = Box<dyn Fn(&Matcher, NodeId, NodeOutputType) -> bool>;

/// Post-match hook closure.  Runs after the recursive matcher has
/// bound every sub-pattern and the current pat node's capture (if any).
/// Returning `false` rejects the match (and triggers commutative
/// retry for arity-2 nodes whose kind is commutative).
///
/// Arguments:
/// - `matcher` — the active [`Matcher`]; reach the function under
///   inspection via [`Matcher::function`] and the options via
///   [`Matcher::options`].
/// - `node` — the matched IR `NodeId` (load-bearing for closures that
///   inspect side-tables like `Function::stack_offset` /
///   `Function::phi_var_tag` / `Function::call_other_name`).
/// - `ty`   — the matched IR output's `NodeOutputType` (zero-output
///   match sites pass `NodeOutputType::I1` as a placeholder; closures
///   that only need to inspect the matched node's side-table state can
///   ignore it).
/// - `b`    — the bindings accumulated so far.
///
/// `Box` (single-threaded; no Arc / Rc / Send / Sync in this crate's
/// public surface).  Move-only.  The strider-py wrapper handles `Pat`
/// reuse via its own `Rc<Pat<Wildcard>>` storage — refcounting stays
/// behind the FFI boundary instead of leaking into the core types.
pub type PostMatchFn = Box<dyn Fn(&Matcher, NodeId, NodeOutputType, &Bindings) -> bool>;

/// Per-edge payload — typed slot indices recovering the IR's
/// `node_inputs(node)[i]` semantics on top of petgraph.
#[derive(Clone, Copy, Debug)]
pub struct EdgeData {
    pub consumer_slot: usize,
    pub producer_output_slot: usize,
}

pub struct TemplateSpec {
    pub kind: TemplateKind,
    pub ty: TemplateTy,
}

pub enum TemplateKind {
    Exact(NodeKind),
    /// Dynamic-kind closure variant.  The closure receives a
    /// [`TemplateCtx`](crate::matcher::TemplateCtx) — exposing the captured
    /// LHS [`Bindings`], the matched-root NodeId / output type, and a
    /// shared [`Function`](strider_ir::Function) — and returns the
    /// `NodeKind` to materialise.  Used by the `*_const_with` family
    /// of builders to emit constants whose value is computed from
    /// captured operand values at rewrite time.
    Fn(TemplateKindFn),
}

/// Type alias for the [`TemplateKind::Fn`] closure shape.  Factored out
/// to keep `TemplateKind` legible under clippy's `type_complexity` lint.
/// `Box` (single-threaded; no Arc / Rc / Send / Sync in the core).
pub type TemplateKindFn =
    Box<dyn Fn(&crate::matcher::TemplateCtx<'_>) -> anyhow::Result<NodeKind>>;

#[derive(Clone, Copy)]
pub enum TemplateTy {
    InheritRoot,
    Fixed(NodeOutputType),
}
