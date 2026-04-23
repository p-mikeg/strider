//! Boolean arithmetic / logical `PatKind` arms.
//!
//! Handles: `BoolBinaryOp`, `BoolUnaryOp`, and the `*Any` variants where the
//! operator itself is captured as a binding.

use ir::node::{NodeKind, NodeOutputId};

use super::super::Matcher;
use super::super::bindings::Bindings;
use super::super::commutativity::is_commutative_bool_op;
use crate::pat::{Pat, PatKind};

pub(super) fn match_bool(
    matcher: &Matcher,
    output: NodeOutputId,
    pat: &Pat,
    bindings: &mut Bindings,
) -> Option<bool> {
    let node = matcher.fn_graph.graph.get_node_from_output(output);
    let kind = matcher.fn_graph.graph.node_kind(node);

    let result = match pat.as_legacy()? {
        PatKind::BoolBinaryOp {
            op,
            lhs,
            rhs,
            ordered,
        } => {
            let NodeKind::BoolBinaryOp(actual) = kind else {
                return Some(false);
            };
            if actual != op {
                return Some(false);
            }
            let Ok([l, r]) = matcher.fn_graph.graph.node_inputs_exact::<2>(node) else {
                return Some(false);
            };
            let snap = bindings.clone();
            if matcher.match_output(l, lhs, bindings) && matcher.match_output(r, rhs, bindings) {
                return Some(true);
            }
            if !ordered && is_commutative_bool_op(*op) {
                *bindings = snap.clone();
                if matcher.match_output(r, lhs, bindings) && matcher.match_output(l, rhs, bindings)
                {
                    return Some(true);
                }
            }
            *bindings = snap;
            false
        }

        PatKind::BoolUnaryOp { op, operand } => {
            let NodeKind::BoolUnaryOp(actual) = kind else {
                return Some(false);
            };
            if actual != op {
                return Some(false);
            }
            let Ok([inp]) = matcher.fn_graph.graph.node_inputs_exact::<1>(node) else {
                return Some(false);
            };
            let snap = bindings.clone();
            if matcher.match_output(inp, operand, bindings) {
                true
            } else {
                *bindings = snap;
                false
            }
        }

        PatKind::BoolBinaryAny {
            op: op_var,
            lhs,
            rhs,
            ordered,
        } => {
            let NodeKind::BoolBinaryOp(actual_op) = kind else {
                return Some(false);
            };
            let Ok([l, r]) = matcher.fn_graph.graph.node_inputs_exact::<2>(node) else {
                return Some(false);
            };
            let snap = bindings.clone();
            if matcher.match_output(l, lhs, bindings)
                && matcher.match_output(r, rhs, bindings)
                && bindings.bind_bool_binary_op(*op_var, *actual_op)
            {
                return Some(true);
            }
            if !ordered && is_commutative_bool_op(*actual_op) {
                *bindings = snap.clone();
                if matcher.match_output(r, lhs, bindings)
                    && matcher.match_output(l, rhs, bindings)
                    && bindings.bind_bool_binary_op(*op_var, *actual_op)
                {
                    return Some(true);
                }
            }
            *bindings = snap;
            false
        }

        PatKind::BoolUnaryAny { op: op_var, operand } => {
            let NodeKind::BoolUnaryOp(actual_op) = kind else {
                return Some(false);
            };
            let Ok([inp]) = matcher.fn_graph.graph.node_inputs_exact::<1>(node) else {
                return Some(false);
            };
            let snap = bindings.clone();
            if matcher.match_output(inp, operand, bindings)
                && bindings.bind_bool_unary_op(*op_var, *actual_op)
            {
                return Some(true);
            }
            *bindings = snap;
            false
        }

        _ => return None,
    };
    Some(result)
}
