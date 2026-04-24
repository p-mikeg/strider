//! `FunctionArgPat` — matches `FunctionArg` entry nodes with optional source
//! (register / stack) and index constraints.

use ir::node::NodeKind;

use crate::pat::Pat;
use crate::pat::builders::CaptureBuilder;
use crate::pat::node_pat::{InputsSpec, KindSpec, NodePat, exemplar_vn};
use crate::var::{NodeVar, Var};

/// Builder for `FunctionArg` node patterns.  Created by
/// [`crate::pat::function_arg`], [`crate::pat::function_arg_any`],
/// [`crate::pat::function_arg_reg`], [`crate::pat::function_arg_stack`].
pub struct FunctionArgPat {
    source: Option<ir::node::FunctionArgSource>,
    index: Option<u32>,
    output_var: Option<Var>,
    node_var: Option<NodeVar>,
}

impl FunctionArgPat {
    pub(crate) fn new() -> Self {
        Self { source: None, index: None, output_var: None, node_var: None }
    }
    /// Restrict the match to a specific ABI source (register or stack slot).
    pub fn source(mut self, s: ir::node::FunctionArgSource) -> Self {
        self.source = Some(s);
        self
    }
    /// Restrict the match to a specific argument index.
    pub fn index(mut self, i: u32) -> Self {
        self.index = Some(i);
        self
    }
}

impl CaptureBuilder for FunctionArgPat {
    fn output_slot(&mut self) -> &mut Option<Var> { &mut self.output_var }
    fn node_slot(&mut self) -> &mut Option<NodeVar> { &mut self.node_var }
}

impl From<FunctionArgPat> for Pat {
    fn from(b: FunctionArgPat) -> Pat {
        let FunctionArgPat { source, index, output_var, node_var } = b;
        let exemplar = NodeKind::FunctionArg {
            source: ir::node::FunctionArgSource::Register(exemplar_vn()),
            index: 0,
        };
        let kind = if source.is_none() && index.is_none() {
            KindSpec::variant(&exemplar)
        } else {
            KindSpec::variant_with(&exemplar, move |k| {
                let NodeKind::FunctionArg { source: actual_source, index: actual_index } = k else {
                    return false;
                };
                if let Some(ref expected_source) = source
                    && actual_source != expected_source
                {
                    return false;
                }
                index.is_none_or(|expected| *actual_index == expected)
            })
        };
        NodePat::matcher(kind, InputsSpec::None)
            .with_output_var(output_var)
            .with_node_var(node_var)
            .into_pat()
    }
}
