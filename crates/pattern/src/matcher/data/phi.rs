//! Phi + InitialVar `PatKind` arms.

use ir::node::{NodeKind, NodeOutputId};

use super::super::Matcher;
use super::super::bindings::Bindings;
use crate::pat::{Pat, PatKind};

pub(super) fn match_phi(
    matcher: &Matcher,
    output: NodeOutputId,
    pat: &Pat,
    bindings: &mut Bindings,
) -> Option<bool> {
    let node = matcher.fn_graph.graph.get_node_from_output(output);
    let kind = matcher.fn_graph.graph.node_kind(node);

    let result = match pat.inner() {
        PatKind::Phi {
            vn,
            inputs: slot_pats,
            output_var,
            node_var,
        } => {
            let NodeKind::ControlPhi(actual_vn) = kind else {
                return Some(false);
            };
            if let Some(v) = vn
                && actual_vn != v
            {
                return Some(false);
            }
            let inputs = matcher.fn_graph.graph.node_inputs(node);
            let snap = bindings.clone();
            for (idx, slot_pat) in slot_pats {
                let Some(&slot_out) = inputs.get(*idx) else {
                    *bindings = snap;
                    return Some(false);
                };
                if !matcher.match_output(slot_out, slot_pat, bindings) {
                    *bindings = snap;
                    return Some(false);
                }
            }
            if let Some(v) = output_var
                && !bindings.bind_var(*v, output)
            {
                *bindings = snap;
                return Some(false);
            }
            if let Some(nv) = node_var
                && !bindings.bind_node_var(*nv, node)
            {
                *bindings = snap;
                return Some(false);
            }
            true
        }

        PatKind::InitialVar { vn } => {
            let NodeKind::InitialVar(actual_vn) = kind else {
                return Some(false);
            };
            if let Some(v) = vn
                && actual_vn != v
            {
                return Some(false);
            }
            true
        }

        PatKind::FunctionArg {
            source,
            index,
            output_var,
            node_var,
        } => {
            let NodeKind::FunctionArg {
                source: actual_source,
                index: actual_index,
            } = kind
            else {
                return Some(false);
            };
            if let Some(s) = source
                && actual_source != s
            {
                return Some(false);
            }
            if let Some(i) = index
                && actual_index != i
            {
                return Some(false);
            }
            let snap = bindings.clone();
            if let Some(v) = output_var
                && !bindings.bind_var(*v, output)
            {
                *bindings = snap;
                return Some(false);
            }
            if let Some(nv) = node_var
                && !bindings.bind_node_var(*nv, node)
            {
                *bindings = snap;
                return Some(false);
            }
            true
        }

        _ => return None,
    };
    Some(result)
}
