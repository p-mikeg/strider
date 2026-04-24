//! `RetPat` — matches `Return` nodes with optional constraints on the direct
//! ctrl predecessor and return-value inputs.

use std::sync::Arc;

use ir::node::NodeKind;

use crate::pat::Pat;
use crate::pat::node_pat::{InputsSpec, KindFilter, NodePat};
use crate::var::NodeVar;

/// Builder for `Return` node patterns.  Created by [`crate::pat::ret`].
pub struct RetPat {
    preceded_by: Option<Pat>,
    ret_vals: Vec<(usize, Pat)>,
    node_var: Option<NodeVar>,
}

impl RetPat {
    pub(crate) fn new() -> Self {
        Self { preceded_by: None, ret_vals: Vec::new(), node_var: None }
    }
    /// Match `p` against the Return's **direct** ctrl predecessor (the node
    /// producing input slot 0 — typically a `ControlState` at a region
    /// header).  This is a single-step match, not a backward walk through the
    /// CFG; to reach a non-adjacent ancestor the caller must structure `p`
    /// accordingly (e.g. `.preceded_by(cs().preceded_by(call()))`).
    pub fn preceded_by(mut self, p: impl Into<Pat>) -> Self {
        self.preceded_by = Some(p.into());
        self
    }
    /// Constrain return value at position `idx` (0-based after the ctrl input).
    pub fn ret_val(mut self, idx: usize, p: impl Into<Pat>) -> Self {
        self.ret_vals.push((idx, p.into()));
        self
    }
    /// Bind the matched `Return` node to `nv`.
    pub fn capture(mut self, nv: NodeVar) -> Self {
        self.node_var = Some(nv);
        self
    }
}

impl From<RetPat> for Pat {
    fn from(b: RetPat) -> Pat {
        let RetPat { preceded_by, ret_vals, node_var } = b;
        // Return inputs: [ctrl(0), mem(1), retval0(2), retval1(3), ...].
        // `preceded_by` matches against the ctrl input (index 0); the default
        // `Pattern::try_match` on the sub-pattern then does
        // `get_node_from_output`, giving a direct-step backward match.
        let mut indexed_inputs: Vec<(usize, Pat)> = Vec::new();
        if let Some(prev) = preceded_by {
            indexed_inputs.push((0, prev));
        }
        for (i, p) in ret_vals {
            indexed_inputs.push((2 + i, p));
        }
        NodePat::matcher(
            KindFilter::exact(&NodeKind::Return),
            Arc::new(|ctx, node, _b| matches!(ctx.graph.graph.node_kind(node), NodeKind::Return)),
            InputsSpec::Indexed(indexed_inputs),
        )
        .with_node_var(node_var)
        .into_pat()
    }
}
