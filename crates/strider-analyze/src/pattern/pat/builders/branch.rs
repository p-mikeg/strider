//! `IfPat` — matches `If` nodes with optional constraints on the condition
//! input and the single consumers of the true/false control outputs.
//!
//! Match layout (single, direct):
//!  - cond matches input 1;
//!  - true_branch matches consumer of output 0;
//!  - false_branch matches consumer of output 1.
//!
//! The compiler-inverted layout (`If(BoolNeg(C)){B}{A}` for source-level
//! `if (c) A else B`) is handled upstream of pattern matching by the
//! `opt::IfCondInversion` canonicalisation pass: it eagerly rewrites
//! every `If(BoolNeg(C)){A}{B}` into `If(C){B}{A}` (and collapses double
//! negations via the existing `BoolNeg(BoolNeg(x)) → x` ConstantFold rule
//! that runs first).  By the time `Matcher` walks the graph, every `If`
//! is in canonical direct layout, and the symmetric two-layout matching
//! that lived here is unnecessary.  Use [`crate::pattern::pat::IntoPat::capture`]
//! to bind the matched If node id.

use std::sync::Arc;

use strider_ir::node::{NodeId, NodeKind, NodeOutputId};

use crate::pattern::matcher::Bindings;
use crate::pattern::pat::Pat;
use crate::pattern::pat::node_pat::KindSpec;
use crate::pattern::pat::traits::{MatchCtx, Pattern};

/// Builder for `If` node patterns.  Created by [`crate::pattern::pat::if_node`].
pub struct IfPat {
    cond: Option<Pat>,
    true_branch: Option<Pat>,
    false_branch: Option<Pat>,
}

impl IfPat {
    pub(crate) fn new() -> Self {
        Self { cond: None, true_branch: None, false_branch: None }
    }
    /// Constrain the branch condition.  Matched directly against the
    /// `If`'s cond input; the `opt::IfCondInversion` pass guarantees
    /// every `If` is in canonical direct layout before patterns run.
    pub fn cond(mut self, p: impl Into<Pat>) -> Self {
        self.cond = Some(p.into());
        self
    }
    /// Match `p` against the single consumer of the If's true-branch
    /// output (output 0).
    pub fn true_branch(mut self, p: impl Into<Pat>) -> Self {
        self.true_branch = Some(p.into());
        self
    }
    /// Match `p` against the single consumer of the If's false-branch
    /// output (output 1).
    pub fn false_branch(mut self, p: impl Into<Pat>) -> Self {
        self.false_branch = Some(p.into());
        self
    }
}

/// Direct-layout-only `Pattern` impl for `IfPat`.
struct IfPattern {
    cond: Option<Pat>,
    true_branch: Option<Pat>,
    false_branch: Option<Pat>,
}

impl Pattern for IfPattern {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        let node = ctx.function.node_for_output(target);
        self.try_match_at(ctx, node, b)
    }

    fn kind_spec(&self) -> KindSpec {
        KindSpec::Exact(NodeKind::If)
    }

    fn try_match_node(&self, ctx: &MatchCtx, node: NodeId, b: &mut Bindings) -> bool {
        // `If` has zero VALUE outputs (only Control outputs); the default
        // `try_match_node` (which iterates outputs and calls `try_match`)
        // would never reach this pattern via `find_all`'s candidate-output
        // enumeration.  Match against the node directly here.
        self.try_match_at(ctx, node, b)
    }
}

impl IfPattern {
    /// Verifies `node` is an `If` and applies the cond / true_branch /
    /// false_branch constraints in canonical direct layout.
    fn try_match_at(&self, ctx: &MatchCtx, node: NodeId, b: &mut Bindings) -> bool {
        if !matches!(ctx.function.node_kind(node), NodeKind::If) {
            return false;
        }
        let mark = b.mark();
        if self.try_layout(ctx, node, b) {
            return true;
        }
        b.restore(mark);
        false
    }

    fn try_layout(&self, ctx: &MatchCtx, if_node: NodeId, b: &mut Bindings) -> bool {
        // 1. Cond.  Input 1 of the If (input 0 is the Control predecessor).
        if let Some(cond_pat) = &self.cond {
            let inputs = ctx.function.node_inputs(if_node);
            let Some(cond_in) = inputs.into_iter().nth(1) else {
                return false;
            };
            if !ctx.matcher.match_output_with_walk_through(cond_in, cond_pat, b) {
                return false;
            }
        }

        // 2. True / false branch consumers.
        if let Some(tp) = self.true_branch.as_ref()
            && !match_branch_consumer(ctx, if_node, 0, tp, b)
        {
            return false;
        }
        if let Some(fp) = self.false_branch.as_ref()
            && !match_branch_consumer(ctx, if_node, 1, fp, b)
        {
            return false;
        }
        true
    }
}

/// Match `pat` against the single forward-step consumer of the If's
/// output at `output_index`.  Honors `ignore_regions` via
/// [`crate::pattern::pat::node_pat::match_consumer_node`]: the helper walks
/// through an immediate `Region` header when the flag is set.
fn match_branch_consumer(
    ctx: &MatchCtx,
    if_node: NodeId,
    output_index: usize,
    pat: &Pat,
    b: &mut Bindings,
) -> bool {
    let outputs = ctx.function.node_outputs(if_node);
    let Some(&out) = outputs.get(output_index) else {
        return false;
    };
    let Some(consumer) = crate::pattern::matcher::consumer::next_control_node(ctx.matcher, out) else {
        return false;
    };
    crate::pattern::pat::node_pat::match_consumer_node(ctx, consumer, pat, b)
}

impl From<IfPat> for Pat {
    fn from(b: IfPat) -> Pat {
        let IfPat { cond, true_branch, false_branch } = b;
        Pat::from_dyn(Arc::new(IfPattern { cond, true_branch, false_branch }))
    }
}
