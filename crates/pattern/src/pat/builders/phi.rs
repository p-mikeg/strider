//! `PhiPat` — matches `ControlPhi` nodes with optional varnode constraint
//! and sparse positional input constraints.

use ir::node::NodeKind;

use crate::pat::Pat;
use crate::pat::builders::CaptureBuilder;
use crate::pat::node_pat::{InputsSpec, KindSpec, NodePat, exemplar_vn};
use crate::var::{NodeVar, Var};

/// Builder for `ControlPhi` node patterns.  Created by [`crate::pat::phi`] or
/// [`crate::pat::phi_for`].
pub struct PhiPat {
    vn: Option<rsleigh::Vn>,
    inputs: Vec<(usize, Pat)>,
    output_var: Option<Var>,
    node_var: Option<NodeVar>,
}

impl PhiPat {
    pub(crate) fn new() -> Self {
        Self { vn: None, inputs: Vec::new(), output_var: None, node_var: None }
    }
    /// Restrict the match to phi nodes for varnode `vn`.
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

impl CaptureBuilder for PhiPat {
    fn output_slot(&mut self) -> &mut Option<Var> { &mut self.output_var }
    fn node_slot(&mut self) -> &mut Option<NodeVar> { &mut self.node_var }
}

impl From<PhiPat> for Pat {
    fn from(b: PhiPat) -> Pat {
        let PhiPat { vn, inputs, output_var, node_var } = b;
        let kind = match vn {
            None => KindSpec::variant(&NodeKind::ControlPhi(exemplar_vn())),
            Some(expected) => KindSpec::variant_with(
                &NodeKind::ControlPhi(exemplar_vn()),
                move |k| matches!(k, NodeKind::ControlPhi(actual) if *actual == expected),
            ),
        };
        NodePat::matcher(kind, InputsSpec::Indexed(inputs))
            .with_output_var(output_var)
            .with_node_var(node_var)
            .into_pat()
    }
}
