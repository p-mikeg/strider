//! `IfPat` — matches `If` nodes with optional constraints on the condition
//! input and the single consumers of the true/false control outputs
//! (via `ConsumersSpec::Indexed` direct-step forward walk).
//!
//! Use [`crate::pat::IntoPat::capture`] to bind the matched If node id.

use ir::node::NodeKind;

use crate::pat::Pat;
use crate::pat::node_pat::{ConsumersSpec, InputsSpec, KindSpec, NodePat};

/// Builder for `If` node patterns.  Created by [`crate::pat::if_node`].
pub struct IfPat {
    cond: Option<Pat>,
    true_branch: Option<Pat>,
    false_branch: Option<Pat>,
}

impl IfPat {
    pub(crate) fn new() -> Self {
        Self { cond: None, true_branch: None, false_branch: None }
    }
    /// Constrain the branch condition.
    pub fn cond(mut self, p: impl Into<Pat>) -> Self {
        self.cond = Some(p.into());
        self
    }
    /// Match `p` against the single consumer of the If's true-branch output.
    pub fn true_branch(mut self, p: impl Into<Pat>) -> Self {
        self.true_branch = Some(p.into());
        self
    }
    /// Match `p` against the single consumer of the If's false-branch output.
    pub fn false_branch(mut self, p: impl Into<Pat>) -> Self {
        self.false_branch = Some(p.into());
        self
    }
}

impl From<IfPat> for Pat {
    fn from(b: IfPat) -> Pat {
        let IfPat { cond, true_branch, false_branch } = b;
        // If inputs: [ctrl(0), cond(1)]. Outputs: [true-ctrl(0), false-ctrl(1)].
        let mut indexed_inputs: Vec<(usize, Pat)> = Vec::new();
        if let Some(c) = cond {
            indexed_inputs.push((1, c));
        }
        let mut indexed_consumers: Vec<(usize, Pat)> = Vec::new();
        if let Some(tb) = true_branch {
            indexed_consumers.push((0, tb));
        }
        if let Some(fb) = false_branch {
            indexed_consumers.push((1, fb));
        }
        let consumers_spec = if indexed_consumers.is_empty() {
            ConsumersSpec::None
        } else {
            ConsumersSpec::Indexed(indexed_consumers)
        };
        NodePat::matcher(KindSpec::Exact(NodeKind::If), InputsSpec::Indexed(indexed_inputs))
            .with_consumers(consumers_spec)
            .into_pat()
    }
}
