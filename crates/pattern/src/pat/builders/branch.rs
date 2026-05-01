//! `IfPat` — matches `If` nodes with optional constraints on the condition
//! input and the single consumers of the true/false control outputs.
//!
//! When the pattern has a `cond` constraint, the matcher tries TWO layouts:
//! 1. **Direct**: cond matches input 1; true_branch matches consumer of output 0;
//!    false_branch matches consumer of output 1.
//! 2. **Swapped**: input 1 is `BoolUnaryOp::Neg(inner)`, inner matches cond;
//!    true_branch matches consumer of output 1; false_branch matches consumer
//!    of output 0.
//!
//! This handles compiler-inverted if-then-else: `if (c) A else B` and
//! `if (!c) B else A` are logically equivalent and must both match the
//! source-level pattern `if_node().cond(c).true_branch(A).false_branch(B)`.
//!
//! Without a `cond` constraint, no swap is attempted — there is no
//! condition to negate.
//!
//! Use [`crate::pat::IntoPat::capture`] to bind the matched If node id.

use std::sync::Arc;

use ir::BoolUnaryOp;
use ir::node::{NodeId, NodeKind, NodeOutputId};

use crate::matcher::Bindings;
use crate::pat::Pat;
use crate::pat::node_pat::KindSpec;
use crate::pat::traits::{MatchCtx, Pattern};

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
    /// Constrain the branch condition.  When set, the matcher also tries
    /// the compiler-inverted layout — see module-level docs.
    pub fn cond(mut self, p: impl Into<Pat>) -> Self {
        self.cond = Some(p.into());
        self
    }
    /// Match `p` against the single consumer of the If's true-branch
    /// output.  When `cond` is also set, also matches the consumer of
    /// the false-branch output if cond is found wrapped in `Not(...)`.
    pub fn true_branch(mut self, p: impl Into<Pat>) -> Self {
        self.true_branch = Some(p.into());
        self
    }
    /// Match `p` against the single consumer of the If's false-branch
    /// output.  Symmetric to `true_branch`.
    pub fn false_branch(mut self, p: impl Into<Pat>) -> Self {
        self.false_branch = Some(p.into());
        self
    }
}

/// Custom `Pattern` impl for `IfPat`: tries direct and (if cond is set)
/// swapped layouts.
struct IfPattern {
    cond: Option<Pat>,
    true_branch: Option<Pat>,
    false_branch: Option<Pat>,
}

impl Pattern for IfPattern {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        let node = ctx.graph.graph.get_node_from_output(target);
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
    /// Common entry: verifies `node` is an `If`, then tries the direct
    /// layout and (if a cond is set) the swapped layout.  Restores
    /// bindings between attempts so a failed sibling can't leak partial
    /// state into the surviving match.
    fn try_match_at(&self, ctx: &MatchCtx, node: NodeId, b: &mut Bindings) -> bool {
        if !matches!(ctx.graph.graph.node_kind(node), NodeKind::If) {
            return false;
        }
        let mark = b.mark();
        if self.try_layout(ctx, node, b, /*swapped=*/ false) {
            return true;
        }
        b.restore(mark);
        // Swap only when cond is constrained — without a cond, "swap" has
        // no semantic basis and the conservative direct-only match wins.
        if self.cond.is_none() {
            return false;
        }
        if self.try_layout(ctx, node, b, /*swapped=*/ true) {
            return true;
        }
        b.restore(mark);
        false
    }

    fn try_layout(
        &self,
        ctx: &MatchCtx,
        if_node: NodeId,
        b: &mut Bindings,
        swapped: bool,
    ) -> bool {
        // 1. Cond.  Input 1 of the If (input 0 is the Control predecessor).
        if let Some(cond_pat) = &self.cond {
            let inputs = ctx.graph.graph.node_inputs(if_node);
            let Some(cond_in) = inputs.into_iter().nth(1) else {
                return false;
            };
            if swapped {
                // Require cond_in to be Neg(<x>); match cond_pat against <x>.
                let cond_node = ctx.graph.graph.get_node_from_output(cond_in);
                if !matches!(
                    ctx.graph.graph.node_kind(cond_node),
                    NodeKind::BoolUnaryOp(BoolUnaryOp::Neg)
                ) {
                    return false;
                }
                let inner_inputs = ctx.graph.graph.node_inputs(cond_node);
                let Some(inner) = inner_inputs.into_iter().next() else {
                    return false;
                };
                if !ctx.matcher.match_output_with_walk_through(inner, cond_pat, b) {
                    return false;
                }
            } else if !ctx.matcher.match_output_with_walk_through(cond_in, cond_pat, b) {
                return false;
            }
        }

        // 2. True / false branch consumers.  Under swap, output 1 carries
        //    the source-level "true" semantics and output 0 the "false".
        let (true_out_idx, false_out_idx) = if swapped { (1, 0) } else { (0, 1) };

        if let Some(tp) = self.true_branch.as_ref()
            && !match_branch_consumer(ctx, if_node, true_out_idx, tp, b)
        {
            return false;
        }
        if let Some(fp) = self.false_branch.as_ref()
            && !match_branch_consumer(ctx, if_node, false_out_idx, fp, b)
        {
            return false;
        }
        true
    }
}

/// Match `pat` against the single forward-step consumer of the If's
/// output at `output_index`.  Honors `ignore_control_states` via
/// [`crate::pat::node_pat::match_consumer_node`]: the helper walks
/// through an immediate `ControlState` header when the flag is set.
fn match_branch_consumer(
    ctx: &MatchCtx,
    if_node: NodeId,
    output_index: usize,
    pat: &Pat,
    b: &mut Bindings,
) -> bool {
    let outputs = ctx.graph.graph.node_outputs(if_node);
    let Some(out) = outputs.into_iter().nth(output_index) else {
        return false;
    };
    let Some(consumer) = crate::matcher::walk::next_control_node(ctx.matcher, out) else {
        return false;
    };
    crate::pat::node_pat::match_consumer_node(ctx, consumer, pat, b)
}

impl From<IfPat> for Pat {
    fn from(b: IfPat) -> Pat {
        let IfPat { cond, true_branch, false_branch } = b;
        Pat::from_dyn(Arc::new(IfPattern { cond, true_branch, false_branch }))
    }
}
