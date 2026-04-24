//! `PhiPat` — matches `ControlPhi` nodes with optional varnode constraint
//! and sparse positional input constraints.

use ir::node::NodeKind;

use crate::pat::Pat;
use crate::pat::node_pat::{InputsSpec, KindSpec, NodePat, exemplar_vn};

/// Builder for `ControlPhi` node patterns.  Created by [`crate::pat::phi`] or
/// [`crate::pat::phi_for`].
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
            None => KindSpec::variant(&NodeKind::ControlPhi(exemplar_vn())),
            Some(expected) => KindSpec::variant_with(
                &NodeKind::ControlPhi(exemplar_vn()),
                move |k| matches!(k, NodeKind::ControlPhi(actual) if *actual == expected),
            ),
        };
        NodePat::matcher(kind, InputsSpec::Indexed(inputs)).into_pat()
    }
}
