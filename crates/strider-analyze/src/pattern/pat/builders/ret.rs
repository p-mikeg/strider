//! `RetPat` — matches `Return` nodes with optional constraints on the direct
//! ctrl predecessor and return-value inputs.
//!
//! Use [`crate::pattern::pat::IntoPat::capture`] to bind the matched Return node id.

use strider_ir::node::NodeKind;

use crate::pattern::pat::Pat;
use crate::pattern::pat::node_pat::{InputsSpec, KindSpec, NodePat};

/// Builder for `Return` node patterns.  Created by [`crate::pattern::pat::ret`].
pub struct RetPat {
    preceded_by: Option<Pat>,
    ret_vals: Vec<(usize, Pat)>,
}

impl RetPat {
    pub(crate) fn new() -> Self {
        Self { preceded_by: None, ret_vals: Vec::new() }
    }
    /// Match `p` against the Return's **direct** ctrl predecessor (the node
    /// producing input slot 0 — typically a `Region` at a region
    /// header).  This is a single-step match, not a backward walk through
    /// the CFG; to reach a non-adjacent ancestor the caller must structure
    /// `p` accordingly.  When the matcher's
    /// [`crate::pattern::Matcher::ignore_regions`] flag is set, the match
    /// transparently walks through `Region` join nodes when looking
    /// for `p`.
    pub fn preceded_by(mut self, p: impl Into<Pat>) -> Self {
        self.preceded_by = Some(p.into());
        self
    }
    /// Constrain return value at position `idx` (0-based after the ctrl
    /// and mem inputs — i.e. mapped to the Return's input slot `2 + idx`).
    pub fn ret_val(mut self, idx: usize, p: impl Into<Pat>) -> Self {
        self.ret_vals.push((idx, p.into()));
        self
    }
}

impl From<RetPat> for Pat {
    fn from(b: RetPat) -> Pat {
        let RetPat { preceded_by, ret_vals } = b;
        // Return inputs: [ctrl(0), mem(1), retval0(2), retval1(3), ...].
        // `preceded_by` matches against the ctrl input (index 0); the default
        // `Pattern::try_match` on the sub-pattern then does
        // `node_for_output`, giving a direct-step backward match.
        let mut indexed_inputs: Vec<(usize, Pat)> = Vec::new();
        if let Some(prev) = preceded_by {
            indexed_inputs.push((0, prev));
        }
        for (i, p) in ret_vals {
            indexed_inputs.push((2 + i, p));
        }
        NodePat::matcher(KindSpec::Exact(NodeKind::Return), InputsSpec::Indexed(indexed_inputs))
            .into_pat()
    }
}
