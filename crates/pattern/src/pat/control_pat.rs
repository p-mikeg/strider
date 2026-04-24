//! Generic control-level pattern. Covers `Call`, `CallOther`, `Return`, and
//! `If` — the four control-level node kinds — under the single unified
//! [`Pattern`] trait.  A single [`ControlNodePat`] struct tagged by
//! [`CtrlKind`] dispatches all four.
//!
//! Target type is [`NodeOutputId`] (matching the rest of the engine); the
//! `try_match` impl recovers the producing node via
//! `ctx.graph.graph.get_node_from_output(target)` and forwards to
//! [`Pattern::try_match_node`], which is the real workhorse.  Overriding
//! `try_match_node` (rather than only `try_match`) also lets control
//! patterns match zero-output semantic nodes like `Return`.

use ir::node::{NodeId, NodeKind, NodeOutputId};

use crate::matcher::Bindings;
use crate::matcher::walk;
use crate::pat::traits::{CandidateKind, DynPat, MatchCtx, Pattern};
use crate::var::NodeVar;

pub struct ControlNodePat {
    pub(crate) kind: CtrlKind,
    pub(crate) node_var: Option<NodeVar>,
}

pub enum CtrlKind {
    Call {
        target: Option<DynPat>,
        args: Vec<(usize, DynPat)>,
        ret_outputs: Vec<(usize, DynPat)>,
    },
    CallOther {
        user_op_id: Option<u64>,
        args: Vec<(usize, DynPat)>,
    },
    Return {
        preceded_by: Option<DynPat>,
        ret_vals: Vec<(usize, DynPat)>,
    },
    If {
        cond: Option<DynPat>,
        true_branch: Option<DynPat>,
        false_branch: Option<DynPat>,
    },
}

impl Pattern for ControlNodePat {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        let node = ctx.graph.graph.get_node_from_output(target);
        self.try_match_node(ctx, node, b)
    }

    fn try_match_node(&self, ctx: &MatchCtx, node: NodeId, b: &mut Bindings) -> bool {
        let graph = &ctx.graph.graph;
        let kind = graph.node_kind(node);
        let inputs: Vec<NodeOutputId> = graph.node_inputs(node).into_iter().collect();

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
                    if !ctx.matcher.match_output_dyn(tgt_out, tgt_pat, b) {
                        *b = snap;
                        return false;
                    }
                }

                for (idx, arg_pat) in args {
                    let Some(&arg_out) = inputs.get(3 + idx) else {
                        *b = snap;
                        return false;
                    };
                    if !ctx.matcher.match_output_dyn(arg_out, arg_pat, b) {
                        *b = snap;
                        return false;
                    }
                }

                // Call outputs: [ctrl(0), mem(1), retval0(2), retval1(3), ...].
                if !ret_outputs.is_empty() {
                    let outputs = graph.node_outputs(node);
                    for (idx, out_pat) in ret_outputs {
                        let Some(&ret_out) = outputs.get(2 + idx) else {
                            *b = snap;
                            return false;
                        };
                        if !ctx.matcher.match_output_dyn(ret_out, out_pat, b) {
                            *b = snap;
                            return false;
                        }
                    }
                }

                if let Some(nv) = self.node_var
                    && !b.bind_node_var(nv, node)
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
                    if !ctx.matcher.match_output_dyn(arg_out, arg_pat, b) {
                        *b = snap;
                        return false;
                    }
                }

                if let Some(nv) = self.node_var
                    && !b.bind_node_var(nv, node)
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
                    // semantic node, then match the inner pattern against
                    // that node's output.  `skip_backward_transparent`
                    // returns `ctrl_in` unchanged if the immediate producer
                    // is already semantic.
                    let producer_out = walk::skip_backward_transparent(ctx.matcher, ctrl_in);
                    if !ctx.matcher.match_output_dyn(producer_out, call_pat, b) {
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
                    if !ctx.matcher.match_output_dyn(rv_out, rv_pat, b) {
                        *b = snap;
                        return false;
                    }
                }

                if let Some(nv) = self.node_var
                    && !b.bind_node_var(nv, node)
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
                    if !ctx.matcher.match_output_dyn(cond_out, cond_pat, b) {
                        *b = snap;
                        return false;
                    }
                }

                let outputs = graph.node_outputs(node);

                if let Some(tb_pat) = true_branch {
                    let Some(&true_ctrl) = outputs.get(0) else {
                        *b = snap;
                        return false;
                    };
                    if !try_match_forward_branch(ctx, true_ctrl, tb_pat, b) {
                        *b = snap;
                        return false;
                    }
                }

                if let Some(fb_pat) = false_branch {
                    let Some(&false_ctrl) = outputs.get(1) else {
                        *b = snap;
                        return false;
                    };
                    if !try_match_forward_branch(ctx, false_ctrl, fb_pat, b) {
                        *b = snap;
                        return false;
                    }
                }

                if let Some(nv) = self.node_var
                    && !b.bind_node_var(nv, node)
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

/// Advance `ctrl_out` past transparent nodes (`ControlState` / `IfCase`)
/// and match `pat` against the first semantic node reached.
///
/// Uses [`walk::skip_forward_transparent`] first — that returns the first
/// output of the reached semantic node, which is the most direct path and
/// handles the common case cleanly.  When the semantic node has no
/// outputs (e.g. a terminating `Return`) [`walk::skip_forward_transparent`]
/// returns `None`; the fallback locates the semantic node itself via
/// [`locate_forward_semantic_node`] and dispatches through
/// [`Pattern::try_match_node`], which `ControlNodePat` overrides so
/// patterns like `ret()` match zero-output nodes correctly.
fn try_match_forward_branch(
    ctx: &MatchCtx,
    ctrl_out: NodeOutputId,
    pat: &DynPat,
    b: &mut Bindings,
) -> bool {
    if let Some(out) = walk::skip_forward_transparent(ctx.matcher, ctrl_out) {
        return ctx.matcher.match_output_dyn(out, pat, b);
    }
    let Some(semantic_node) = locate_forward_semantic_node(ctx, ctrl_out) else {
        return false;
    };
    pat.try_match_node(ctx, semantic_node, b)
}

/// Mirror of [`walk::skip_forward_transparent`] that returns the semantic
/// node's [`NodeId`] instead of its first output.  Used when the semantic
/// node has no outputs — the caller ([`try_match_forward_branch`]) needs
/// the node.
fn locate_forward_semantic_node(ctx: &MatchCtx, ctrl_out: NodeOutputId) -> Option<NodeId> {
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
