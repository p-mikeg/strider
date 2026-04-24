//! Control-level patterns — one struct per node kind, each implementing the
//! unified [`Pattern`] trait directly. `Call` / `CallOther` / `Return` / `If`
//! share no enum or dispatch wrapper; each struct owns its own `try_match_node`
//! body.
//!
//! Every struct targets [`NodeOutputId`] via `Pattern::try_match`, which
//! recovers the producing node and forwards to `try_match_node`. Overriding
//! `try_match_node` lets zero-output terminators like `Return` still match.
//!
//! For `If.true_branch` / `If.false_branch` / `Return.preceded_by`:
//! direct-step semantics — the sub-pattern matches the direct ctrl
//! consumer/producer. No walking through transparent nodes.

use ir::node::{NodeId, NodeKind, NodeOutputId};

use crate::matcher::Bindings;
use crate::matcher::walk;
use crate::pat::traits::{CandidateKind, DynPat, MatchCtx, Pattern};
use crate::var::NodeVar;

// ── Call ─────────────────────────────────────────────────────────────────────

pub struct CallPattern {
    pub(crate) target: Option<DynPat>,
    pub(crate) args: Vec<(usize, DynPat)>,
    pub(crate) ret_outputs: Vec<(usize, DynPat)>,
    pub(crate) node_var: Option<NodeVar>,
}

impl Pattern for CallPattern {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        let node = ctx.graph.graph.get_node_from_output(target);
        self.try_match_node(ctx, node, b)
    }

    fn try_match_node(&self, ctx: &MatchCtx, node: NodeId, b: &mut Bindings) -> bool {
        let graph = &ctx.graph.graph;
        if !matches!(graph.node_kind(node), NodeKind::Call) {
            return false;
        }
        let inputs: Vec<NodeOutputId> = graph.node_inputs(node).into_iter().collect();
        let snap = b.clone();

        // Call inputs: [ctrl(0), mem(1), target(2), arg0(3), arg1(4), ...].
        if let Some(tgt_pat) = &self.target {
            let Some(&tgt_out) = inputs.get(2) else {
                *b = snap;
                return false;
            };
            if !ctx.matcher.match_output_dyn(tgt_out, tgt_pat, b) {
                *b = snap;
                return false;
            }
        }

        for (idx, arg_pat) in &self.args {
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
        if !self.ret_outputs.is_empty() {
            let outputs = graph.node_outputs(node);
            for (idx, out_pat) in &self.ret_outputs {
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

    fn candidate_kind(&self) -> Option<CandidateKind> {
        Some(CandidateKind::Call)
    }
}

// ── CallOther ────────────────────────────────────────────────────────────────

pub struct CallOtherPattern {
    pub(crate) user_op_id: Option<u64>,
    pub(crate) args: Vec<(usize, DynPat)>,
    pub(crate) node_var: Option<NodeVar>,
}

impl Pattern for CallOtherPattern {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        let node = ctx.graph.graph.get_node_from_output(target);
        self.try_match_node(ctx, node, b)
    }

    fn try_match_node(&self, ctx: &MatchCtx, node: NodeId, b: &mut Bindings) -> bool {
        let graph = &ctx.graph.graph;
        let NodeKind::CallOther { user_op_id: actual_id } = graph.node_kind(node) else {
            return false;
        };
        if let Some(id) = self.user_op_id
            && *actual_id != id
        {
            return false;
        }
        let inputs: Vec<NodeOutputId> = graph.node_inputs(node).into_iter().collect();
        let snap = b.clone();

        // CallOther inputs: [ctrl(0), mem(1), arg0(2), arg1(3), ...].
        for (idx, arg_pat) in &self.args {
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

    fn candidate_kind(&self) -> Option<CandidateKind> {
        Some(CandidateKind::CallOther)
    }
}

// ── Return ───────────────────────────────────────────────────────────────────

pub struct ReturnPattern {
    pub(crate) preceded_by: Option<DynPat>,
    pub(crate) ret_vals: Vec<(usize, DynPat)>,
    pub(crate) node_var: Option<NodeVar>,
}

impl Pattern for ReturnPattern {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        let node = ctx.graph.graph.get_node_from_output(target);
        self.try_match_node(ctx, node, b)
    }

    fn try_match_node(&self, ctx: &MatchCtx, node: NodeId, b: &mut Bindings) -> bool {
        let graph = &ctx.graph.graph;
        if !matches!(graph.node_kind(node), NodeKind::Return) {
            return false;
        }
        let inputs: Vec<NodeOutputId> = graph.node_inputs(node).into_iter().collect();
        let snap = b.clone();

        if let Some(prev_pat) = &self.preceded_by {
            let Some(&ctrl_in) = inputs.first() else {
                *b = snap;
                return false;
            };
            // Direct-step backward: match the inner pattern against the
            // node that directly produces Return's ctrl input.
            let producer = walk::prev_control_node(ctx.matcher, ctrl_in);
            if !prev_pat.try_match_node(ctx, producer, b) {
                *b = snap;
                return false;
            }
        }

        // Return inputs: [ctrl(0), mem(1), retval0(2), retval1(3), ...].
        for (idx, rv_pat) in &self.ret_vals {
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

    fn candidate_kind(&self) -> Option<CandidateKind> {
        Some(CandidateKind::Return)
    }
}

// ── If ───────────────────────────────────────────────────────────────────────

pub struct IfPattern {
    pub(crate) cond: Option<DynPat>,
    pub(crate) true_branch: Option<DynPat>,
    pub(crate) false_branch: Option<DynPat>,
    pub(crate) node_var: Option<NodeVar>,
}

impl Pattern for IfPattern {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        let node = ctx.graph.graph.get_node_from_output(target);
        self.try_match_node(ctx, node, b)
    }

    fn try_match_node(&self, ctx: &MatchCtx, node: NodeId, b: &mut Bindings) -> bool {
        let graph = &ctx.graph.graph;
        if !matches!(graph.node_kind(node), NodeKind::If) {
            return false;
        }
        let snap = b.clone();

        if let Some(cond_pat) = &self.cond {
            let inputs: Vec<NodeOutputId> = graph.node_inputs(node).into_iter().collect();
            // If inputs: [ctrl(0), cond(1)].
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

        if let Some(tb_pat) = &self.true_branch {
            let Some(&true_ctrl) = outputs.get(0) else {
                *b = snap;
                return false;
            };
            // Direct-step forward: match the inner pattern against the
            // direct consumer of If.output[0].
            let Some(successor) = walk::next_control_node(ctx.matcher, true_ctrl) else {
                *b = snap;
                return false;
            };
            if !tb_pat.try_match_node(ctx, successor, b) {
                *b = snap;
                return false;
            }
        }

        if let Some(fb_pat) = &self.false_branch {
            let Some(&false_ctrl) = outputs.get(1) else {
                *b = snap;
                return false;
            };
            let Some(successor) = walk::next_control_node(ctx.matcher, false_ctrl) else {
                *b = snap;
                return false;
            };
            if !fb_pat.try_match_node(ctx, successor, b) {
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

    fn candidate_kind(&self) -> Option<CandidateKind> {
        Some(CandidateKind::If)
    }
}
