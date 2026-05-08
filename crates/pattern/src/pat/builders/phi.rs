//! Phi node pattern builders.
//!
//! `PhiPat` matches `VarPhi` nodes (the SSA phi for a tracked variable);
//! `MemPhiPat` matches `MemPhi` (the memory-token phi at join points);
//! `ValuePhiPat` matches `ValuePhi` (the value-phi synthesised by
//! `StackLoadForward`).  All three carry an optional per-predecessor
//! input constraint.

use ir::node::NodeKind;

use crate::pat::Pat;
use crate::pat::node_pat::{InputsSpec, KindSpec, NodePat, exemplar_vn};

/// Builder for `VarPhi` node patterns.  Created by [`crate::pat::phi`] or
/// [`crate::pat::phi_for`].
///
/// Matches **only** `VarPhi`.  For `MemPhi` use [`MemPhiPat`] /
/// [`crate::pat::mem_phi`]; for `ValuePhi` use [`ValuePhiPat`] /
/// [`crate::pat::value_phi`].
///
/// Capture the matched output with `.capture(v)` from
/// [`crate::pat::IntoPat`].
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
    pub fn input(mut self, idx: usize, p: impl Into<Pat>) -> Self {
        self.inputs.push((idx, p.into()));
        self
    }
}

impl From<PhiPat> for Pat {
    fn from(b: PhiPat) -> Pat {
        let PhiPat { vn, inputs } = b;
        let kind = match vn {
            None => KindSpec::variant(&NodeKind::VarPhi(exemplar_vn())),
            Some(expected) => KindSpec::variant_with(
                &NodeKind::VarPhi(exemplar_vn()),
                move |k| matches!(k, NodeKind::VarPhi(actual) if *actual == expected),
            ),
        };
        NodePat::matcher(kind, InputsSpec::Indexed(inputs)).into_pat()
    }
}

/// Builder for `MemPhi` node patterns.  Created by [`crate::pat::mem_phi`].
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
    /// Constrain the value arriving from predecessor slot `idx`.
    pub fn input(mut self, idx: usize, p: impl Into<Pat>) -> Self {
        self.inputs.push((idx, p.into()));
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

/// Builder for `ValuePhi` node patterns.  Created by
/// [`crate::pat::value_phi`].
///
/// `ValuePhi` is synthesised by `StackLoadForward` to phi together
/// stack-store values that flow into a load through a control-flow
/// join.  Patterns that walk forwarded stack values may need this.
pub struct ValuePhiPat {
    inputs: Vec<(usize, Pat)>,
}

impl ValuePhiPat {
    pub(crate) fn new() -> Self {
        Self { inputs: Vec::new() }
    }
    /// Constrain the value arriving from predecessor slot `idx`.
    pub fn input(mut self, idx: usize, p: impl Into<Pat>) -> Self {
        self.inputs.push((idx, p.into()));
        self
    }
}

impl From<ValuePhiPat> for Pat {
    fn from(b: ValuePhiPat) -> Pat {
        let ValuePhiPat { inputs } = b;
        let kind = KindSpec::variant(&NodeKind::ValuePhi);
        NodePat::matcher(kind, InputsSpec::Indexed(inputs)).into_pat()
    }
}
