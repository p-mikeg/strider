//! Control-chain traversal helpers used by the matcher.
//!
//! Hosts `match_contains` (forward walk along a ctrl chain looking for an
//! inner pattern), `preceded_by_search` (backward walk from a ctrl input to
//! find a matching Call node), and `first_ctrl_output` (fetch the Control
//! output of a node, if any).
//!
//! As of the Step 3 refactor (switch to one-step-direct semantics via
//! `matcher/walk.rs`), these helpers are no longer called by
//! `ControlNodePat::try_match`.  They remain live for `ContainsPat` which
//! will be deleted in the follow-up step, at which point this whole module
//! goes with it.  `#[allow(dead_code)]` silences the transitional warnings.
#![allow(dead_code)]

use std::collections::HashSet;

use ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind};

use super::Matcher;
use super::bindings::Bindings;
use crate::pat::Pat;

/// Forward walk along a ctrl chain.  Tries `inner_pat` against each node
/// encountered until a match is found or the chain ends.
///
/// Exposed as `pub(crate)` under the `_from_pat` alias so callers outside
/// this module (namely [`crate::pat::control_pat`] and
/// [`crate::pat::contains`]) can reuse the same walker without duplicating
/// the cycle-safe traversal.
pub(crate) fn match_contains_from_pat(
    matcher: &Matcher,
    ctrl_output: NodeOutputId,
    inner_pat: &Pat,
    bindings: &mut Bindings,
    visited: &mut HashSet<NodeId>,
) -> bool {
    match_contains(matcher, ctrl_output, inner_pat, bindings, visited)
}

pub(super) fn match_contains(
    matcher: &Matcher,
    ctrl_output: NodeOutputId,
    inner_pat: &Pat,
    bindings: &mut Bindings,
    visited: &mut HashSet<NodeId>,
) -> bool {
    // Peel a `Contains` shell — this function *is* the forward search, so
    // the wrapper adds no extra semantics here.  Handles nested
    // `contains(contains(...))` and any other case where a `Contains`
    // appears as an inner pattern.
    let peeled = inner_pat
        .as_ctrl()
        .and_then(|c| c.contains_inner())
        .cloned();
    let inner_pat = peeled.as_ref().unwrap_or(inner_pat);

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

/// Crate-visible re-export of [`preceded_by_search`] for callers outside
/// this module (used by [`crate::pat::control_pat`]).
pub(crate) fn preceded_by_search_from_pat(
    matcher: &Matcher,
    ctrl_output: NodeOutputId,
    call_pat: &Pat,
    bindings: &mut Bindings,
) -> bool {
    preceded_by_search(matcher, ctrl_output, call_pat, bindings)
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
