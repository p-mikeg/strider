//! Phi-family chained builders: `PhiPat`, `MemPhiPat`, `ValuePhiPat`.
//!
//! `Phi` and `MemPhi` are distinguished by `NodeKind` discriminant.
//! Tagged vs. anonymous `Phi` distinction requires reading
//! `Function::phi_var_tag` at match time, which needs a `MatchCtx`-aware
//! `post_match` closure — currently the stub `Box<dyn Fn() -> bool>`.
//! `PhiPat::for_vn(vn)` and the anonymous-only filter of `ValuePhiPat`
//! are deferred until the closure widens (Task 11).  Today both
//! `phi()` and `value_phi()` match any `Phi` discriminant.
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
    BuildKind, BuildSpec, BuildTy, EdgeData, KindSpec, NodeData, PatGraph, Role, Wildcard,
    merge_subgraph,
};

use super::Pat;

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
}

impl PhiPat {
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

impl From<PhiPat> for Pat<Wildcard> {
    fn from(b: PhiPat) -> Pat<Wildcard> {
        let PhiPat { inputs } = b;
        finalise_phi_kind(NodeKind::Phi, inputs)
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
        finalise_phi_kind(NodeKind::MemPhi, inputs)
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
        finalise_phi_kind(NodeKind::Phi, inputs)
    }
}

// ── Shared finaliser ─────────────────────────────────────────────────────────

/// Build a phi-family pattern with the given discriminant and indexed
/// predecessor sub-patterns.  Used by `Phi`, `MemPhi`, and anonymous
/// `Phi` finalisers; the only difference between the three is the kind
/// discriminant and (eventually) the `phi_var_tag` post-match filter.
fn finalise_phi_kind(
    kind_exemplar: NodeKind,
    inputs: Vec<(usize, Pat<Wildcard>)>,
) -> Pat<Wildcard> {
    let mut parent: PatGraph<Wildcard> = PatGraph::new();
    let root = parent.add_node(NodeData {
        kind: KindSpec::Variant(std::mem::discriminant(&kind_exemplar)),
        output_ty: None,
        capture: None,
        post_match: None,
        build_spec: Some(BuildSpec {
            kind: BuildKind::Exact(kind_exemplar),
            ty: BuildTy::InheritRoot,
        }),
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
