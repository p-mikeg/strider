//! Control-level pattern dispatch: `match_node_id`.
//!
//! Dispatches on `Call` / `CallOther` / `Return` / `If` patterns against a
//! `NodeId`.  For every other `PatKind`, falls back to trying each output of
//! the node via `match_output`.

use std::collections::HashSet;

use ir::node::{NodeId, NodeKind, NodeOutputId};

use super::Matcher;
use super::bindings::Bindings;
use super::traversal;
use crate::pat::{Pat, PatKind};

/// Match a `NodeId` (control-level node) against a pattern.
pub(super) fn match_node_id(
    matcher: &Matcher,
    node: NodeId,
    pat: &Pat,
    bindings: &mut Bindings,
) -> bool {
    let kind = matcher.fn_graph.graph.node_kind(node);
    let inputs: Vec<NodeOutputId> =
        matcher.fn_graph.graph.node_inputs(node).into_iter().collect();

    match pat.inner() {
        PatKind::Call {
            target,
            args,
            node_var,
        } => {
            if !matches!(kind, NodeKind::Call) {
                return false;
            }
            let snap = bindings.clone();

            if let Some(tgt_pat) = target {
                let Some(&tgt_out) = inputs.get(2) else {
                    *bindings = snap;
                    return false;
                };
                if !matcher.match_output(tgt_out, tgt_pat, bindings) {
                    *bindings = snap;
                    return false;
                }
            }

            for (idx, arg_pat) in args {
                let Some(&arg_out) = inputs.get(3 + idx) else {
                    *bindings = snap;
                    return false;
                };
                if !matcher.match_output(arg_out, arg_pat, bindings) {
                    *bindings = snap;
                    return false;
                }
            }

            if let Some(nv) = node_var
                && !bindings.bind_node_var(*nv, node)
            {
                *bindings = snap;
                return false;
            }
            true
        }

        PatKind::CallOther {
            user_op_id,
            args,
            node_var,
        } => {
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
            let snap = bindings.clone();

            for (idx, arg_pat) in args {
                // CallOther inputs: [ctrl(0), mem(1), arg0(2), arg1(3), …]
                let Some(&arg_out) = inputs.get(2 + idx) else {
                    *bindings = snap;
                    return false;
                };
                if !matcher.match_output(arg_out, arg_pat, bindings) {
                    *bindings = snap;
                    return false;
                }
            }

            if let Some(nv) = node_var
                && !bindings.bind_node_var(*nv, node)
            {
                *bindings = snap;
                return false;
            }
            true
        }

        PatKind::Return {
            preceded_by,
            ret_vals,
            node_var,
        } => {
            if !matches!(kind, NodeKind::Return) {
                return false;
            }
            let snap = bindings.clone();

            if let Some(call_pat) = preceded_by {
                let Some(&ctrl_in) = inputs.first() else {
                    *bindings = snap;
                    return false;
                };
                if !traversal::preceded_by_search(matcher, ctrl_in, call_pat, bindings) {
                    *bindings = snap;
                    return false;
                }
            }

            // Return inputs: [ctrl(0), retval0(1), retval1(2), …]
            // There is no memory edge on Return — only ctrl then the return values.
            for (idx, rv_pat) in ret_vals {
                let Some(&rv_out) = inputs.get(1 + idx) else {
                    *bindings = snap;
                    return false;
                };
                if !matcher.match_output(rv_out, rv_pat, bindings) {
                    *bindings = snap;
                    return false;
                }
            }

            if let Some(nv) = node_var
                && !bindings.bind_node_var(*nv, node)
            {
                *bindings = snap;
                return false;
            }
            true
        }

        PatKind::If {
            cond,
            true_branch,
            false_branch,
            node_var,
        } => {
            if !matches!(kind, NodeKind::If) {
                return false;
            }
            let snap = bindings.clone();

            if let Some(cond_pat) = cond {
                let Some(&cond_out) = inputs.get(1) else {
                    *bindings = snap;
                    return false;
                };
                if !matcher.match_output(cond_out, cond_pat, bindings) {
                    *bindings = snap;
                    return false;
                }
            }

            let outputs = matcher.fn_graph.graph.node_outputs(node);

            if let Some(tb_pat) = true_branch {
                let Some(&true_ctrl) = outputs.get(0) else {
                    *bindings = snap;
                    return false;
                };
                let mut visited = HashSet::new();
                if !traversal::match_contains(matcher, true_ctrl, tb_pat, bindings, &mut visited) {
                    *bindings = snap;
                    return false;
                }
            }

            if let Some(fb_pat) = false_branch {
                let Some(&false_ctrl) = outputs.get(1) else {
                    *bindings = snap;
                    return false;
                };
                let mut visited = HashSet::new();
                if !traversal::match_contains(matcher, false_ctrl, fb_pat, bindings, &mut visited) {
                    *bindings = snap;
                    return false;
                }
            }

            if let Some(nv) = node_var
                && !bindings.bind_node_var(*nv, node)
            {
                *bindings = snap;
                return false;
            }
            true
        }

        // For all other patterns try every output of the node.
        //
        // We must try *all* output kinds, not just value outputs, so that
        // nodes like `Store` (whose only output is a Memory edge) or
        // `ControlPhi` (whose output is a ControlPhi edge) can
        // be matched from top-level `find_all` queries.
        //
        // `match_output` does its own kind-check, so trying a wrong output
        // (e.g. a Control edge against an IntBinaryOp pattern) just
        // returns `false` cleanly.
        _ => {
            for out in matcher.fn_graph.graph.node_outputs(node).into_iter() {
                let snap = bindings.clone();
                if matcher.match_output(out, pat, bindings) {
                    return true;
                }
                *bindings = snap;
            }
            false
        }
    }
}
