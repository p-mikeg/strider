//! Phi-family chained builders: `PhiPat`, `MemPhiPat`, `ValuePhiPat`.
//!
//! `Phi` and `MemPhi` are distinguished by `NodeKind` discriminant.
//! Tagged vs. anonymous `Phi` distinction reads `Function::phi_var_tag`
//! at match time via the post_match closure:
//!
//! * `PhiPat::for_vn(vn)` — restrict matches to `Phi` nodes whose
//!   `phi_var_tag` is `Some(vn)`.
//! * `ValuePhiPat` — restrict matches to anonymous phis
//!   (`phi_var_tag == None`).  Synthesised by `LoadForward` when
//!   forwarding a `Load[sp+K]` across a `MemPhi`.
//!
//! Input layout: predecessor 0's value lives at raw input index 1 —
//! input 0 is the phi-token edge from the owning `Region`.  `.input(i, p)`
//! shifts by +1 so callers address predecessor slots directly.
//!
//! ## Role handling
//!
//! Phi builders are arity-N; sub-patterns are widened to `Wildcard` at
//! insertion time and the finalised pattern is `Pat<Wildcard>`.  Phi
//! nodes are rarely built on the RHS of a rewrite, so the
//! Wildcard-fallback path is sufficient.

use strider_ir::node::NodeKind;

use crate::pat_graph::{
    TemplateKind, TemplateSpec, TemplateTy, EdgeData, KindSpec, NodeData, PatGraph, PostMatchFn, Role,
    Wildcard, merge_subgraph,
};

use super::Pat;

/// Filter applied at match time over `Function::phi_var_tag`.
#[derive(Clone, Copy)]
enum PhiVarFilter {
    /// Match only phis whose tag equals `Some(vn)`.
    Exact(rsleigh::Vn),
    /// Match only anonymous phis (`tag == None`).
    Anonymous,
}

// ── PhiPat (tagged) ───────────────────────────────────────────────────────────

/// Builder for `Phi` node patterns.  Created by [`phi`].
///
/// Today the builder accepts **any** `Phi` discriminant — distinguishing
/// tagged (lifter-emitted SSA φ) from anonymous (`LoadForward`-synthesised)
/// phis requires reading `Function::phi_var_tag` at match time, which
/// the current stub `post_match` closure cannot do.  Once the closure
/// signature widens, `phi()` will narrow to tagged phis and `for_vn(vn)`
/// will pin the source varnode.
pub struct PhiPat {
    inputs: Vec<(usize, Pat<Wildcard>)>,
    var_filter: Option<PhiVarFilter>,
}

impl PhiPat {
    fn new() -> Self {
        Self {
            inputs: Vec::new(),
            var_filter: None,
        }
    }

    /// Constrain the value arriving from predecessor slot `idx`.  The
    /// builder shifts to raw input slot `idx + 1` to skip the phi-token
    /// input.
    #[must_use]
    pub fn input<R: Role>(mut self, idx: usize, p: Pat<R>) -> Self {
        self.inputs.push((idx + 1, p.into_wildcard()));
        self
    }

    /// Restrict the match to lifter-emitted SSA φ nodes whose
    /// `Function::phi_var_tag` is `Some(vn)`.
    #[must_use]
    pub fn for_vn(mut self, vn: rsleigh::Vn) -> Self {
        self.var_filter = Some(PhiVarFilter::Exact(vn));
        self
    }
}

impl From<PhiPat> for Pat<Wildcard> {
    fn from(b: PhiPat) -> Pat<Wildcard> {
        let PhiPat { inputs, var_filter } = b;
        finalise_phi_kind(NodeKind::Phi, inputs, var_filter)
    }
}

// ── MemPhiPat ─────────────────────────────────────────────────────────────────

/// Builder for `MemPhi` node patterns.  Created by [`mem_phi`].
///
/// `MemPhi` is the memory-token phi at join points.  Same input shift
/// (+1) as `PhiPat`: input 0 is the phi-token edge from the owning
/// `Region`.
pub struct MemPhiPat {
    inputs: Vec<(usize, Pat<Wildcard>)>,
}

impl MemPhiPat {
    fn new() -> Self {
        Self { inputs: Vec::new() }
    }

    /// Constrain the memory token arriving from predecessor slot `idx`.
    /// The builder shifts to raw input slot `idx + 1` to skip the
    /// phi-token input.
    #[must_use]
    pub fn input<R: Role>(mut self, idx: usize, p: Pat<R>) -> Self {
        self.inputs.push((idx + 1, p.into_wildcard()));
        self
    }
}

impl From<MemPhiPat> for Pat<Wildcard> {
    fn from(b: MemPhiPat) -> Pat<Wildcard> {
        let MemPhiPat { inputs } = b;
        finalise_phi_kind(NodeKind::MemPhi, inputs, None)
    }
}

// ── ValuePhiPat (anonymous) ──────────────────────────────────────────────────

/// Builder for anonymous `Phi` (value-phi) node patterns.  Created by
/// [`value_phi`].
///
/// Same kind discriminant as [`PhiPat`].  The anonymous-vs-tagged
/// distinction requires `Function::phi_var_tag` at match time
/// (deferred — see [`PhiPat`]).  Today this builder is equivalent to
/// `phi()`.
pub struct ValuePhiPat {
    inputs: Vec<(usize, Pat<Wildcard>)>,
}

impl ValuePhiPat {
    fn new() -> Self {
        Self { inputs: Vec::new() }
    }

    /// Constrain the value arriving from predecessor slot `idx`.  The
    /// builder shifts to raw input slot `idx + 1` to skip the phi-token
    /// input.
    #[must_use]
    pub fn input<R: Role>(mut self, idx: usize, p: Pat<R>) -> Self {
        self.inputs.push((idx + 1, p.into_wildcard()));
        self
    }
}

impl From<ValuePhiPat> for Pat<Wildcard> {
    fn from(b: ValuePhiPat) -> Pat<Wildcard> {
        let ValuePhiPat { inputs } = b;
        // Anonymous-only filter: `phi_var_tag == None`.
        finalise_phi_kind(NodeKind::Phi, inputs, Some(PhiVarFilter::Anonymous))
    }
}

// ── Shared finaliser ─────────────────────────────────────────────────────────

/// Build a phi-family pattern with the given discriminant, indexed
/// predecessor sub-patterns, and an optional `phi_var_tag` filter.
/// Used by `Phi`, `MemPhi`, and anonymous `Phi` finalisers; the only
/// difference between the three is the kind discriminant and the
/// optional post-match filter.
fn finalise_phi_kind(
    kind_exemplar: NodeKind,
    inputs: Vec<(usize, Pat<Wildcard>)>,
    var_filter: Option<PhiVarFilter>,
) -> Pat<Wildcard> {
    let mut parent: PatGraph<Wildcard> = PatGraph::new();
    let post_match: Option<PostMatchFn> = var_filter.map(|f| -> PostMatchFn {
        std::rc::Rc::new(move |ctx, node, _ty, _b| {
            let tag = ctx.function.phi_var_tag(node);
            match f {
                PhiVarFilter::Exact(want) => tag == Some(want),
                PhiVarFilter::Anonymous => tag.is_none(),
            }
        })
    });
    let root = parent.add_node(NodeData {
        kind: KindSpec::Variant(std::mem::discriminant(&kind_exemplar)),
        output_ty: None,
        capture: None,
        post_match,
        template_spec: Some(TemplateSpec {
            kind: TemplateKind::Exact(kind_exemplar),
            ty: TemplateTy::InheritRoot,
        }),
        force_ordered: false,
    });
    for (slot, sub) in inputs {
        let sub_root = merge_subgraph(&mut parent, sub.0);
        parent.add_edge(
            sub_root,
            root,
            EdgeData {
                consumer_slot: slot,
                producer_output_slot: 0,
            },
        );
    }
    parent.set_root(root);
    Pat::from_graph(parent)
}

// ── Factories ─────────────────────────────────────────────────────────────────

/// Construct a fresh [`PhiPat`].  Chain `.input(idx, p)` for each
/// predecessor constraint then call `.into()` to finalise.
#[must_use]
pub fn phi() -> PhiPat {
    PhiPat::new()
}

/// Starts building a tagged-`Phi` pattern (see [`phi`]) pinned to
/// varnode `vn` in `Function::phi_var_tag`.
#[must_use]
pub fn phi_for(vn: rsleigh::Vn) -> PhiPat {
    PhiPat::new().for_vn(vn)
}

/// Construct a fresh [`MemPhiPat`].
#[must_use]
pub fn mem_phi() -> MemPhiPat {
    MemPhiPat::new()
}

/// Construct a fresh [`ValuePhiPat`].  Today equivalent to [`phi`] —
/// the anonymous-vs-tagged filter is deferred (see module-level docs).
#[must_use]
pub fn value_phi() -> ValuePhiPat {
    ValuePhiPat::new()
}
