//! `FunctionArgPat` — matches `FunctionArg` entry nodes with optional source
//! (register / stack) and index constraints.

use strider_ir::node::NodeKind;

use crate::pattern::pat::Pat;
use crate::pattern::pat::node_pat::{InputsSpec, KindSpec, NodePat, exemplar_vn};

/// Builder for `FunctionArg` node patterns.  Created by
/// [`crate::pattern::pat::function_arg`], [`crate::pattern::pat::function_arg_any`],
/// [`crate::pattern::pat::function_arg_reg`], [`crate::pattern::pat::function_arg_stack`].
///
/// Capture the matched output with `.capture(v)` from
/// [`crate::pattern::pat::IntoPat`].
pub struct FunctionArgPat {
    source: Option<strider_ir::node::FunctionArgSource>,
    index: Option<u32>,
}

impl FunctionArgPat {
    pub(crate) fn new() -> Self {
        Self { source: None, index: None }
    }
    /// Restrict the match to a specific ABI source (register or stack slot).
    #[must_use]
    pub fn source(mut self, s: strider_ir::node::FunctionArgSource) -> Self {
        self.source = Some(s);
        self
    }
    /// Restrict the match to a specific argument index.
    #[must_use]
    pub fn index(mut self, i: u32) -> Self {
        self.index = Some(i);
        self
    }
}

impl From<FunctionArgPat> for Pat {
    fn from(b: FunctionArgPat) -> Pat {
        let FunctionArgPat { source, index } = b;
        let exemplar = NodeKind::FunctionArg {
            source: strider_ir::node::FunctionArgSource::Register(exemplar_vn()),
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
        NodePat::matcher(kind, InputsSpec::None).into_pat()
    }
}
