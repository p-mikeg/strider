//! Generic control-level pattern. Covers `Call`, `CallOther`, `Return`, and
//! `If` — the four control-level patterns whose target is a `NodeId` rather
//! than a data `NodeOutputId`.  A single [`ControlNodePat`] struct tagged by
//! [`CtrlKind`] dispatches all four.

use std::collections::HashSet;

use ir::node::{NodeId, NodeKind, NodeOutputId};

use crate::matcher::Bindings;
use crate::matcher::traversal;
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
                    if !preceded_by_search_ctrl(ctx, ctrl_in, call_pat, b) {
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
                    let mut visited = HashSet::new();
                    if !match_contains_ctrl(ctx, true_ctrl, tb_pat, b, &mut visited) {
                        *b = snap;
                        return false;
                    }
                }

                if let Some(fb_pat) = false_branch {
                    let Some(&false_ctrl) = outputs.get(1) else {
                        *b = snap;
                        return false;
                    };
                    let mut visited = HashSet::new();
                    if !match_contains_ctrl(ctx, false_ctrl, fb_pat, b, &mut visited) {
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

/// Forward walk along a ctrl chain.  Equivalent to
/// [`traversal::match_contains`] but dispatches the inner pattern as a
/// control-level [`DynCtrlPat`].
///
/// If `inner_pat` is a [`ContainsPat`](crate::pat::contains::ContainsPat),
/// its inner is peeled first — the walker itself *is* the forward search,
/// so an outer `Contains` shell would double-walk.
fn match_contains_ctrl(
    ctx: &MatchCtx,
    ctrl_output: NodeOutputId,
    inner_pat: &DynCtrlPat,
    b: &mut Bindings,
    visited: &mut HashSet<NodeId>,
) -> bool {
    let wrapped = match inner_pat.contains_inner() {
        Some(peeled) => peeled.clone(),
        None => Pat::from_ctrl(inner_pat.clone()),
    };
    traversal::match_contains_from_pat(ctx.matcher, ctrl_output, &wrapped, b, visited)
}

/// Backward walk from a ctrl input to find the preceding control-level node.
///
/// Peels any outer `Contains` shell, same rationale as
/// [`match_contains_ctrl`].
fn preceded_by_search_ctrl(
    ctx: &MatchCtx,
    ctrl_output: NodeOutputId,
    call_pat: &DynCtrlPat,
    b: &mut Bindings,
) -> bool {
    let wrapped = match call_pat.contains_inner() {
        Some(peeled) => peeled.clone(),
        None => Pat::from_ctrl(call_pat.clone()),
    };
    traversal::preceded_by_search_from_pat(ctx.matcher, ctrl_output, &wrapped, b)
}
