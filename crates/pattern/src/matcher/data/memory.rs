//! Memory-access `PatKind` arms.
//!
//! Handles: `Load`, `Store`, `StackStore`, `StackStorePhi`.  Each arm
//! performs the kind / space check, matches the addr / data sub-patterns,
//! then binds the optional `output_var` / `node_var` captures.

use ir::node::{NodeKind, NodeOutputId};

use super::super::Matcher;
use super::super::bindings::Bindings;
use crate::pat::{Pat, PatKind};

pub(super) fn match_memory(
    matcher: &Matcher,
    output: NodeOutputId,
    pat: &Pat,
    bindings: &mut Bindings,
) -> Option<bool> {
    let node = matcher.fn_graph.graph.get_node_from_output(output);
    let kind = matcher.fn_graph.graph.node_kind(node);

    let result = match pat.inner() {
        PatKind::Load {
            space,
            addr,
            output_var,
            node_var,
        } => {
            let NodeKind::Load(actual_space) = kind else {
                return Some(false);
            };
            if let Some(s) = space
                && actual_space != s
            {
                return Some(false);
            }
            let snap = bindings.clone();
            let inputs = matcher.fn_graph.graph.node_inputs(node);
            if let Some(addr_pat) = addr {
                let Some(&addr_out) = inputs.get(1) else {
                    *bindings = snap;
                    return Some(false);
                };
                if !matcher.match_output(addr_out, addr_pat, bindings) {
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

        PatKind::Store {
            space,
            addr,
            data,
            output_var,
            node_var,
        } => {
            let NodeKind::Store(actual_space) = kind else {
                return Some(false);
            };
            if let Some(s) = space
                && actual_space != s
            {
                return Some(false);
            }
            let inputs = matcher.fn_graph.graph.node_inputs(node);
            let snap = bindings.clone();
            if let Some(addr_pat) = addr {
                let Some(&addr_out) = inputs.get(1) else {
                    *bindings = snap;
                    return Some(false);
                };
                if !matcher.match_output(addr_out, addr_pat, bindings) {
                    *bindings = snap;
                    return Some(false);
                }
            }
            if let Some(data_pat) = data {
                let Some(&data_out) = inputs.get(2) else {
                    *bindings = snap;
                    return Some(false);
                };
                if !matcher.match_output(data_out, data_pat, bindings) {
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

        PatKind::StackStore {
            space,
            offset,
            data,
            output_var,
            node_var,
        } => {
            let NodeKind::StackStore {
                space: actual_space,
                offset: actual_offset,
            } = *kind
            else {
                return Some(false);
            };
            if let Some(s) = space
                && actual_space != *s
            {
                return Some(false);
            }
            if let Some(o) = offset
                && actual_offset != *o
            {
                return Some(false);
            }
            let inputs = matcher.fn_graph.graph.node_inputs(node);
            let snap = bindings.clone();
            if let Some(data_pat) = data {
                // StackStore inputs = [memory(0), base(1), data(2)].
                let Some(&data_out) = inputs.get(2) else {
                    *bindings = snap;
                    return Some(false);
                };
                if !matcher.match_output(data_out, data_pat, bindings) {
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

        PatKind::StackStorePhi {
            space,
            offsets,
            data,
            output_var,
            node_var,
        } => {
            let NodeKind::StackStorePhi {
                space: actual_space,
            } = *kind
            else {
                return Some(false);
            };
            if let Some(s) = space
                && actual_space != *s
            {
                return Some(false);
            }
            if let Some(expected) = offsets {
                let mut actual: Vec<i64> =
                    matcher.fn_graph.graph.stack_phi_offsets(node).to_vec();
                actual.sort();
                if &actual != expected {
                    return Some(false);
                }
            }
            let inputs = matcher.fn_graph.graph.node_inputs(node);
            let snap = bindings.clone();
            if let Some(data_pat) = data {
                // StackStorePhi inputs = [phi_token(0), memory(1), data(2)].
                let Some(&data_out) = inputs.get(2) else {
                    *bindings = snap;
                    return Some(false);
                };
                if !matcher.match_output(data_out, data_pat, bindings) {
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

        _ => return None,
    };
    Some(result)
}
