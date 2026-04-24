//! Generic control-level pattern. Covers `Call`, `CallOther`, `Return`, and
//! `If` — the four control-level patterns whose target is a `NodeId` rather
//! than a data `NodeOutputId`.  A single [`ControlNodePat`] struct tagged by
//! [`CtrlKind`] dispatches all four.

use ir::node::{NodeId, NodeKind, NodeOutputId};

use crate::matcher::Bindings;
use crate::matcher::walk;
use crate::pat::Pat;
use crate::pat::traits::{
    CandidateKind, ControlPattern, DynCtrlPat, DynDataPat, MatchCtx,
};
use crate::var::NodeVar;

pub struct ControlNodePat {
    pub(crate) kind: CtrlKind,
    pub(crate) node_var: Option<NodeVar>,
}

pub enum CtrlKind {
    Call {
        target: Option<DynDataPat>,
        args: Vec<(usize, DynDataPat)>,
        ret_outputs: Vec<(usize, DynDataPat)>,
    },
    CallOther {
        user_op_id: Option<u64>,
        args: Vec<(usize, DynDataPat)>,
    },
    Return {
        preceded_by: Option<DynCtrlPat>,
        ret_vals: Vec<(usize, DynDataPat)>,
    },
    If {
        cond: Option<DynDataPat>,
        true_branch: Option<DynCtrlPat>,
        false_branch: Option<DynCtrlPat>,
    },
}

impl ControlPattern for ControlNodePat {
    fn try_match(&self, ctx: &MatchCtx, target: NodeId, b: &mut Bindings) -> bool {
        let graph = &ctx.graph.graph;
        let kind = graph.node_kind(target);
        let inputs: Vec<NodeOutputId> = graph.node_inputs(target).into_iter().collect();

        match &self.kind {
            CtrlKind::Call {
                target: tgt,
                args,
                ret_outputs,
            } => {
                if !matches!(kind, NodeKind::Call) {
                    return false;
                }
                let snap = b.clone();

                if let Some(tgt_pat) = tgt {
                    let Some(&tgt_out) = inputs.get(2) else {
                        *b = snap;
                        return false;
                    };
                    if !match_data(ctx, tgt_out, tgt_pat, b) {
                        *b = snap;
                        return false;
                    }
                }

                for (idx, arg_pat) in args {
                    let Some(&arg_out) = inputs.get(3 + idx) else {
                        *b = snap;
                        return false;
                    };
                    if !match_data(ctx, arg_out, arg_pat, b) {
                        *b = snap;
                        return false;
                    }
                }

                // Call outputs: [ctrl(0), mem(1), retval0(2), retval1(3), ...].
                if !ret_outputs.is_empty() {
                    let outputs = graph.node_outputs(target);
                    for (idx, out_pat) in ret_outputs {
                        let Some(&ret_out) = outputs.get(2 + idx) else {
                            *b = snap;
                            return false;
                        };
                        if !match_data(ctx, ret_out, out_pat, b) {
                            *b = snap;
                            return false;
                        }
                    }
                }

                if let Some(nv) = self.node_var
                    && !b.bind_node_var(nv, target)
                {
                    *b = snap;
                    return false;
                }
                true
            }

            CtrlKind::CallOther { user_op_id, args } => {
                let NodeKind::CallOther {
                    user_op_id: actual_id,
                } = kind
                else {
                    return false;
                };
                if let Some(id) = user_op_id
                    && actual_id != id
                {
                    return false;
                }
                let snap = b.clone();

                for (idx, arg_pat) in args {
                    // CallOther inputs: [ctrl(0), mem(1), arg0(2), ...].
                    let Some(&arg_out) = inputs.get(2 + idx) else {
                        *b = snap;
                        return false;
                    };
                    if !match_data(ctx, arg_out, arg_pat, b) {
                        *b = snap;
                        return false;
                    }
                }

                if let Some(nv) = self.node_var
                    && !b.bind_node_var(nv, target)
                {
                    *b = snap;
                    return false;
                }
                true
            }

            CtrlKind::Return {
                preceded_by,
                ret_vals,
            } => {
                if !matches!(kind, NodeKind::Return) {
                    return false;
                }
                let snap = b.clone();

                if let Some(call_pat) = preceded_by {
                    let Some(&ctrl_in) = inputs.first() else {
                        *b = snap;
                        return false;
                    };
                    // One-step backward skip: walk through any transparent
                    // `ControlState` / `IfCase` producers until we reach a
                    // semantic node, then match the inner ctrl pattern
                    // against that node.  `skip_backward_transparent` returns
                    // `ctrl_in` unchanged if the immediate producer is
                    // already semantic.
                    let producer_out = walk::skip_backward_transparent(ctx.matcher, ctrl_in);
                    let producer_node = ctx.graph.graph.get_node_from_output(producer_out);
                    let wrapped = Pat::from_ctrl(call_pat.clone());
                    if !ctx.matcher.match_node_id(producer_node, &wrapped, b) {
                        *b = snap;
                        return false;
                    }
                }

                // Return inputs: [ctrl(0), mem(1), retval0(2), ...].
                for (idx, rv_pat) in ret_vals {
                    let Some(&rv_out) = inputs.get(2 + idx) else {
                        *b = snap;
                        return false;
                    };
                    if !match_data(ctx, rv_out, rv_pat, b) {
                        *b = snap;
                        return false;
                    }
                }

                if let Some(nv) = self.node_var
                    && !b.bind_node_var(nv, target)
                {
                    *b = snap;
                    return false;
                }
                true
            }

            CtrlKind::If {
                cond,
                true_branch,
                false_branch,
            } => {
                if !matches!(kind, NodeKind::If) {
                    return false;
                }
                let snap = b.clone();

                if let Some(cond_pat) = cond {
                    let Some(&cond_out) = inputs.get(1) else {
                        *b = snap;
                        return false;
                    };
                    if !match_data(ctx, cond_out, cond_pat, b) {
                        *b = snap;
                        return false;
                    }
                }

                let outputs = graph.node_outputs(target);

                if let Some(tb_pat) = true_branch {
                    let Some(&true_ctrl) = outputs.get(0) else {
                        *b = snap;
                        return false;
                    };
                    // One-step forward skip: advance past transparent
                    // `ControlState` / `IfCase` consumers to the first
                    // semantic node on the true branch.  `None` means a
                    // dead-end (no consumer) or ambiguous fork (multiple
                    // consumers) — treat as no match.
                    let Some(successor_node) = skip_forward_to_semantic_node(ctx, true_ctrl)
                    else {
                        *b = snap;
                        return false;
                    };
                    let wrapped = Pat::from_ctrl(tb_pat.clone());
                    if !ctx.matcher.match_node_id(successor_node, &wrapped, b) {
                        *b = snap;
                        return false;
                    }
                }

                if let Some(fb_pat) = false_branch {
                    let Some(&false_ctrl) = outputs.get(1) else {
                        *b = snap;
                        return false;
                    };
                    let Some(successor_node) = skip_forward_to_semantic_node(ctx, false_ctrl)
                    else {
                        *b = snap;
                        return false;
                    };
                    let wrapped = Pat::from_ctrl(fb_pat.clone());
                    if !ctx.matcher.match_node_id(successor_node, &wrapped, b) {
                        *b = snap;
                        return false;
                    }
                }

                if let Some(nv) = self.node_var
                    && !b.bind_node_var(nv, target)
                {
                    *b = snap;
                    return false;
                }
                true
            }
        }
    }

    fn candidate_kind(&self) -> Option<CandidateKind> {
        Some(match self.kind {
            CtrlKind::Call { .. } => CandidateKind::Call,
            CtrlKind::CallOther { .. } => CandidateKind::CallOther,
            CtrlKind::Return { .. } => CandidateKind::Return,
            CtrlKind::If { .. } => CandidateKind::If,
        })
    }
}

/// Dispatch a data-level sub-pattern against a `NodeOutputId`.  Wraps the
/// `DynDataPat` in a [`Pat`] so the existing
/// [`crate::matcher::Matcher::match_output`] routing handles it uniformly.
fn match_data(
    ctx: &MatchCtx,
    out: NodeOutputId,
    pat: &DynDataPat,
    b: &mut Bindings,
) -> bool {
    let wrapped = Pat::from_dyn(pat.clone());
    ctx.matcher.match_output(out, &wrapped, b)
}

/// Starting from `ctrl_out` (a Control-kind output on a branch edge), walk
/// forward through transparent consumers (`ControlState` / `IfCase`) until
/// reaching a semantic node, and return that semantic node's `NodeId`.
///
/// This is a node-returning counterpart to
/// [`walk::skip_forward_transparent`], which returns the semantic node's
/// first output.  The output-returning variant fails when the semantic node
/// is a terminator with no outputs (e.g. `Return`); this variant succeeds in
/// that case because callers (the `If.true_branch` / `If.false_branch` arms
/// above) only need the node.
///
/// Returns `None` in the same dead-end / ambiguous-fork cases as
/// [`walk::skip_forward_transparent`].
fn skip_forward_to_semantic_node(ctx: &MatchCtx, ctrl_out: NodeOutputId) -> Option<NodeId> {
    // Advance one transparent hop at a time by reusing the existing
    // one-output-returning walker.  If it succeeds, resolve the output back
    // to a node.  If it fails, the landed-on node may still be semantic but
    // have no outputs — in that case we locate the semantic node directly
    // via the consumers of the last-known transparent chain.
    if let Some(out) = walk::skip_forward_transparent(ctx.matcher, ctrl_out) {
        return Some(ctx.graph.graph.get_node_from_output(out));
    }
    // Fallback: manually walk transparent hops, returning the semantic node
    // itself (whose `first_output` may be `None`).  Mirrors the transparency
    // predicate used by `walk.rs`.
    let mut out = ctrl_out;
    for _ in 0..64 {
        let consumers: Vec<_> = ctx.graph.graph.output_uses(out).collect();
        if consumers.len() != 1 {
            return None;
        }
        let (consumer_node, _) = consumers[0];
        let kind = ctx.graph.graph.node_kind(consumer_node);
        if !matches!(kind, NodeKind::ControlState | NodeKind::IfCase(_)) {
            return Some(consumer_node);
        }
        let next = ctx.graph.graph.node_outputs(consumer_node).into_iter().next()?;
        out = next;
    }
    None
}
