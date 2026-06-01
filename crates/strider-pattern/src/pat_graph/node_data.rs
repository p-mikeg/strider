//! Per-node and per-edge payloads for `PatGraph`.

use std::mem::Discriminant;
use std::rc::Rc;
use strider_ir::node::{NodeId, NodeKind, NodeOutputType};

use crate::bindings::Bindings;
use crate::matcher::Matcher;

/// Kind-level constraint on a pattern node.  Ported from
/// `strider-analyze::pattern::pat::node_pat::KindSpec` — closures are
/// `Rc<dyn Fn>` (single-threaded; no Arc / Send / Sync).  `Rc` makes
/// the spec cheaply `Clone`-able so a single `PyPat` can be reused
/// across multiple `find_all` / `find_one` / `rewrite` invocations
/// (Python's surface treats each `Pat` as a reusable handle).
#[derive(Clone)]
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
        check: Rc<dyn Fn(&NodeKind) -> bool>,
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

/// Per-node payload stored as `StableDiGraph` node weight.
///
/// `Clone`: each closure-bearing field stores an `Rc<dyn Fn>`, so the
/// per-node payload is cheaply `Clone`-able and a `PatGraph` can be
/// cloned without losing any predicate, post-match hook, or
/// dynamic-build closure.
#[derive(Clone)]
pub struct NodeData {
    pub kind: KindSpec,
    pub output_ty: Option<NodeOutputType>,
    /// The capture variable this pattern node binds (`Some`) or no
    /// binding (`None`).  Records "match against this pat node ⇒ bind
    /// capture `c`".
    pub capture: Option<crate::capture::Capture>,
    pub post_match: Option<PostMatchFn>,
    pub template_spec: Option<TemplateSpec>,
    /// When `true`, the matcher MUST NOT trigger commutative-operand
    /// retry on this node even if `NodeKind::is_commutative()` would
    /// allow it.  Default `false` — the matcher honours commutativity.
    pub force_ordered: bool,
}

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
/// `Rc` (not `Box`) so a `PatGraph` carrying a post-match hook can be
/// cloned cheaply — the strider-py wrapper needs this for `Pat` reuse
/// across multiple matcher calls.
pub type PostMatchFn = Rc<dyn Fn(&Matcher, NodeId, NodeOutputType, &Bindings) -> bool>;

/// Per-edge payload — typed slot indices recovering the IR's
/// `node_inputs(node)[i]` semantics on top of petgraph.
#[derive(Clone, Copy, Debug)]
pub struct EdgeData {
    pub consumer_slot: usize,
    pub producer_output_slot: usize,
}

#[derive(Clone)]
pub struct TemplateSpec {
    pub kind: TemplateKind,
    pub ty: TemplateTy,
}

#[derive(Clone)]
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
/// `Rc` (not `Box`) so a `PatGraph` carrying a dynamic-build closure
/// can be cloned cheaply.
pub type TemplateKindFn =
    Rc<dyn Fn(&crate::matcher::TemplateCtx<'_>) -> anyhow::Result<NodeKind>>;

#[derive(Clone, Copy)]
pub enum TemplateTy {
    InheritRoot,
    Fixed(NodeOutputType),
}
