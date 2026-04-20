//! Control-chain traversal helpers used by the matcher.
//!
//! Hosts `match_contains` (forward walk along a ctrl chain looking for an
//! inner pattern), `preceded_by_search` (backward walk from a ctrl input to
//! find a matching Call node), and `first_ctrl_output` (fetch the Control
//! output of a node, if any).

use std::collections::HashSet;

use ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind};

use super::Matcher;
use super::bindings::Bindings;
use crate::pat::{Pat, PatKind};

/// Forward walk along a ctrl chain.  Tries `inner_pat` against each node
/// encountered until a match is found or the chain ends.
pub(super) fn match_contains(
    matcher: &Matcher,
    ctrl_output: NodeOutputId,
    inner_pat: &Pat,
    bindings: &mut Bindings,
    visited: &mut HashSet<NodeId>,
) -> bool {
    // Peel a `Contains` shell — this function *is* the forward search, so
    // the wrapper adds no extra semantics here.
    let inner_pat = match inner_pat.inner() {
        PatKind::Contains(p) => p,
        _ => inner_pat,
    };

    let consumers: Vec<(NodeId, u32)> =
        matcher.fn_graph.graph.output_uses(ctrl_output).collect();

    for (consumer, _) in consumers {
        if !visited.insert(consumer) {
            continue;
        }

        // Try to match here.
        let snap = bindings.clone();
        if matcher.match_node_id(consumer, inner_pat, bindings) {
            return true;
        }
        *bindings = snap;

        // Continue forward through transparent nodes.
        match matcher.fn_graph.graph.node_kind(consumer) {
            NodeKind::ControlState | NodeKind::IfCase(_) => {
                if let Some(next_ctrl) = first_ctrl_output(matcher, consumer)
                    && match_contains(matcher, next_ctrl, inner_pat, bindings, visited)
                {
                    return true;
                }
            }
            NodeKind::Call => {
                // Continue past the call.
                if let Some(next_ctrl) = first_ctrl_output(matcher, consumer)
                    && match_contains(matcher, next_ctrl, inner_pat, bindings, visited)
                {
                    return true;
                }
            }
            // If / Return are terminating — don't cross them.
            _ => {}
        }
    }
    false
}

/// Backward walk from a ctrl input to find the preceding Call node.
pub(super) fn preceded_by_search(
    matcher: &Matcher,
    ctrl_output: NodeOutputId,
    call_pat: &Pat,
    bindings: &mut Bindings,
) -> bool {
    let producing = matcher.fn_graph.graph.get_node_from_output(ctrl_output);

    match matcher.fn_graph.graph.node_kind(producing) {
        NodeKind::Call => {
            let snap = bindings.clone();
            if matcher.match_node_id(producing, call_pat, bindings) {
                true
            } else {
                // This call did not match — keep walking backwards through
                // its own ctrl input so earlier calls in a sequence can
                // still be found.
                *bindings = snap;
                let call_inputs: Vec<NodeOutputId> = matcher
                    .fn_graph
                    .graph
                    .node_inputs(producing)
                    .into_iter()
                    .collect();
                if let Some(&prev_ctrl) = call_inputs.first() {
                    preceded_by_search(matcher, prev_ctrl, call_pat, bindings)
                } else {
                    false
                }
            }
        }
        NodeKind::ControlState => {
            // Try each predecessor ctrl edge.
            let preds: Vec<NodeOutputId> = matcher
                .fn_graph
                .graph
                .node_inputs(producing)
                .into_iter()
                .collect();
            for pred_ctrl in preds {
                let snap = bindings.clone();
                if preceded_by_search(matcher, pred_ctrl, call_pat, bindings) {
                    return true;
                }
                *bindings = snap;
            }
            false
        }
        _ => false,
    }
}

/// Return the first Control-kind output of `node`, if any.
pub(super) fn first_ctrl_output(matcher: &Matcher, node: NodeId) -> Option<NodeOutputId> {
    matcher
        .fn_graph
        .graph
        .node_outputs(node)
        .into_iter()
        .find(|&o| matcher.fn_graph.graph.output_kind(o) == NodeOutputKind::Control)
}
