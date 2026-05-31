//! Per-node and per-edge payloads for `PatGraph`.

use std::mem::Discriminant;
use strider_ir::node::{NodeKind, NodeOutputType};

/// Kind-level constraint on a pattern node.  Ported from
/// `strider-analyze::pattern::pat::node_pat::KindSpec` — closures are
/// `Box<dyn Fn>` (move-only, single-threaded; no Arc / Send / Sync).
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

/// Per-node payload stored as `StableDiGraph` node weight.
///
/// `NodeData` is move-only — the `Box<dyn Fn>` closures inside aren't
/// `Clone`.  `merge_subgraph` consumes a child graph by value via
/// `StableDiGraph::into_nodes_edges_iters` so cloning is never needed.
pub struct NodeData {
    pub kind: KindSpec,
    pub output_ty: Option<NodeOutputType>,
    /// Reserved for future capture support (Task 3 will introduce
    /// `crate::capture::CaptureRef` and wire it here).  Left as a
    /// type-aliased `()` for now so the field exists structurally.
    pub capture: Option<()>,
    pub post_match: Option<PostMatchFn>,
    pub build_spec: Option<BuildSpec>,
}

/// Reserved for the matcher's post-match hook closure.  Task 4 will
/// re-type the inner function once `MatchCtx` and `Bindings` exist.
pub type PostMatchFn = Box<dyn Fn() -> bool>;

/// Per-edge payload — typed slot indices recovering the IR's
/// `node_inputs(node)[i]` semantics on top of petgraph.
#[derive(Clone, Copy, Debug)]
pub struct EdgeData {
    pub consumer_slot: usize,
    pub producer_output_slot: usize,
}

pub struct BuildSpec {
    pub kind: BuildKind,
    pub ty: BuildTy,
}

pub enum BuildKind {
    Exact(NodeKind),
    /// Placeholder for the dynamic-kind closure variant (Task 12 will
    /// re-type this when `BuildCtx` exists).
    Fn(Box<dyn Fn() -> anyhow::Result<NodeKind>>),
}

pub enum BuildTy {
    InheritRoot,
    Fixed(NodeOutputType),
}
