//! Phi node pattern builders.
//!
//! `PhiPat` matches Vn-tagged `Phi` nodes (the SSA phi for a tracked
//! variable); `MemPhiPat` matches `MemPhi` (the memory-token phi at
//! join points); `ValuePhiPat` matches anonymous `Phi` (the value-phi
//! synthesised by `LoadForward`).  All three carry an optional
//! per-predecessor input constraint.
//!
//! The Vn tag for a `Phi` node lives in the
//! `strider_ir::Graph::phi_var_tag` side-table; the kind discriminant
//! alone cannot distinguish anonymous from tagged phis, so the
//! distinction is enforced by a `post_match` predicate after the kind
//! discriminant has matched.

use std::sync::Arc;

use strider_ir::node::NodeKind;

use crate::pattern::pat::Pat;
use crate::pattern::pat::node_pat::{InputsSpec, KindSpec, NodePat};

/// Builder for Vn-tagged `Phi` node patterns.  Created by
/// [`crate::pattern::pat::phi`] or [`crate::pattern::pat::phi_for`].
///
/// Matches **only** phi nodes whose `phi_var_tag` side-table entry is
/// `Some(_)`.  For `MemPhi` use [`MemPhiPat`] /
/// [`crate::pattern::pat::mem_phi`]; for anonymous `Phi` use
/// [`ValuePhiPat`] / [`crate::pattern::pat::value_phi`].
///
/// Capture the matched output with `.capture(v)` from
/// [`crate::pattern::pat::IntoPat`].
pub struct PhiPat {
    vn: Option<rsleigh::Vn>,
    inputs: Vec<(usize, Pat)>,
}

impl PhiPat {
    pub(crate) fn new() -> Self {
        Self { vn: None, inputs: Vec::new() }
    }
    /// Restrict the match to phi nodes for varnode `vn`.
    #[must_use]
    pub fn for_vn(mut self, v: rsleigh::Vn) -> Self {
        self.vn = Some(v);
        self
    }
    /// Constrain the value arriving from predecessor slot `idx`.
    ///
    /// Predecessor 0's value lives at raw input index 1 — input 0 is
    /// the phi-token edge from the owning `Region`.  This
    /// builder shifts `idx` by +1 so callers can address predecessor
    /// slots directly.
    pub fn input(mut self, idx: usize, p: impl Into<Pat>) -> Self {
        self.inputs.push((idx + 1, p.into()));
        self
    }
}

impl From<PhiPat> for Pat {
    fn from(b: PhiPat) -> Pat {
        let PhiPat { vn, inputs } = b;
        // KindSpec accepts any Phi discriminant; the Vn-tag check
        // runs as a post_match predicate against the graph's
        // phi_var_tag side-table.
        let kind = KindSpec::variant(&NodeKind::Phi);
        let post_match = match vn {
            None => Arc::new(|ctx: &crate::pattern::pat::traits::MatchCtx, node: strider_ir::node::NodeId, _b: &mut crate::pattern::matcher::Bindings| {
                ctx.function.phi_var_tag(node).is_some()
            }) as crate::pattern::pat::node_pat::PostMatchFn,
            Some(expected) => Arc::new(move |ctx: &crate::pattern::pat::traits::MatchCtx, node: strider_ir::node::NodeId, _b: &mut crate::pattern::matcher::Bindings| {
                ctx.function.phi_var_tag(node) == Some(expected)
            }) as crate::pattern::pat::node_pat::PostMatchFn,
        };
        NodePat::matcher(kind, InputsSpec::Indexed(inputs))
            .with_post_match(post_match)
            .into_pat()
    }
}

/// Builder for `MemPhi` node patterns.  Created by [`crate::pattern::pat::mem_phi`].
///
/// `MemPhi` is the memory-token phi at join points.  Carries an optional
/// per-predecessor memory-input constraint via [`Self::input`].  Most
/// `MemPhi`s in the optimised IR are eliminated by `RedundantPhis`;
/// patterns that target raw / pre-optimisation IR may need this.
pub struct MemPhiPat {
    inputs: Vec<(usize, Pat)>,
}

impl MemPhiPat {
    pub(crate) fn new() -> Self {
        Self { inputs: Vec::new() }
    }
    /// Constrain the memory token arriving from predecessor slot `idx`.
    ///
    /// Predecessor 0 lives at raw input index 1 — input 0 is the
    /// phi-token edge from the owning `Region`.  This builder
    /// shifts `idx` by +1 so callers can address predecessor slots
    /// directly.
    pub fn input(mut self, idx: usize, p: impl Into<Pat>) -> Self {
        self.inputs.push((idx + 1, p.into()));
        self
    }
}

impl From<MemPhiPat> for Pat {
    fn from(b: MemPhiPat) -> Pat {
        let MemPhiPat { inputs } = b;
        // MemPhi has no payload — `KindSpec::variant` matches any MemPhi.
        let kind = KindSpec::variant(&NodeKind::MemPhi);
        NodePat::matcher(kind, InputsSpec::Indexed(inputs)).into_pat()
    }
}

/// Builder for anonymous `Phi` (value-phi) node patterns.  Created by
/// [`crate::pattern::pat::value_phi`].
///
/// Anonymous phis (those with no `phi_var_tag` entry) are synthesised
/// by `LoadForward` to phi together stack-store values that flow
/// into a load through a control-flow join.  Patterns that walk
/// forwarded stack values may need this.
pub struct ValuePhiPat {
    inputs: Vec<(usize, Pat)>,
}

impl ValuePhiPat {
    pub(crate) fn new() -> Self {
        Self { inputs: Vec::new() }
    }
    /// Constrain the value arriving from predecessor slot `idx`.
    ///
    /// Predecessor 0's value lives at raw input index 1 — input 0 is
    /// the phi-token edge from the owning `Region`.  This
    /// builder shifts `idx` by +1 so callers can address predecessor
    /// slots directly.
    pub fn input(mut self, idx: usize, p: impl Into<Pat>) -> Self {
        self.inputs.push((idx + 1, p.into()));
        self
    }
}

impl From<ValuePhiPat> for Pat {
    fn from(b: ValuePhiPat) -> Pat {
        let ValuePhiPat { inputs } = b;
        let kind = KindSpec::variant(&NodeKind::Phi);
        // Post-match: anonymous phis have no entry in phi_var_tag.
        let post_match = Arc::new(|ctx: &crate::pattern::pat::traits::MatchCtx, node: strider_ir::node::NodeId, _b: &mut crate::pattern::matcher::Bindings| {
            ctx.function.phi_var_tag(node).is_none()
        }) as crate::pattern::pat::node_pat::PostMatchFn;
        NodePat::matcher(kind, InputsSpec::Indexed(inputs))
            .with_post_match(post_match)
            .into_pat()
    }
}
