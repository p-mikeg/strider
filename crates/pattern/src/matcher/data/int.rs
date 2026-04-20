//! Integer arithmetic / comparison `PatKind` arms.
//!
//! Handles: `IntBinaryOp`, `IntUnaryOp`, `IntCmpOp`, and the `*Any` variants
//! where the operator itself is captured as a binding.

use ir::node::{NodeKind, NodeOutputId};

use super::super::Matcher;
use super::super::bindings::Bindings;
use super::super::commutativity::{is_commutative_int_cmp_op, is_commutative_int_op};
use crate::pat::{Pat, PatKind};

pub(super) fn match_int(
    matcher: &Matcher,
    output: NodeOutputId,
    pat: &Pat,
    bindings: &mut Bindings,
) -> Option<bool> {
    let node = matcher.fn_graph.graph.get_node_from_output(output);
    let kind = matcher.fn_graph.graph.node_kind(node);

    let result = match pat.inner() {
        PatKind::IntBinaryOp {
            op,
            lhs,
            rhs,
            ordered,
        } => {
            let NodeKind::IntBinaryOp(actual) = kind else {
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
            if !ordered && is_commutative_int_op(*op) {
                *bindings = snap.clone();
                if matcher.match_output(r, lhs, bindings) && matcher.match_output(l, rhs, bindings)
                {
                    return Some(true);
                }
            }
            *bindings = snap;
            false
        }

        PatKind::IntUnaryOp { op, operand } => {
            let NodeKind::IntUnaryOp(actual) = kind else {
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

        PatKind::IntCmpOp {
            op,
            lhs,
            rhs,
            ordered,
        } => {
            let NodeKind::IntCmpOp(actual) = kind else {
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
            if !ordered && is_commutative_int_cmp_op(*op) {
                *bindings = snap.clone();
                if matcher.match_output(r, lhs, bindings) && matcher.match_output(l, rhs, bindings)
                {
                    return Some(true);
                }
            }
            *bindings = snap;
            false
        }

        PatKind::IntBinaryAny {
            op: op_var,
            lhs,
            rhs,
            ordered,
        } => {
            let NodeKind::IntBinaryOp(actual_op) = kind else {
                return Some(false);
            };
            let Ok([l, r]) = matcher.fn_graph.graph.node_inputs_exact::<2>(node) else {
                return Some(false);
            };
            let snap = bindings.clone();
            if matcher.match_output(l, lhs, bindings)
                && matcher.match_output(r, rhs, bindings)
                && bindings.bind_int_binary_op(*op_var, *actual_op)
            {
                return Some(true);
            }
            if !ordered && is_commutative_int_op(*actual_op) {
                *bindings = snap.clone();
                if matcher.match_output(r, lhs, bindings)
                    && matcher.match_output(l, rhs, bindings)
                    && bindings.bind_int_binary_op(*op_var, *actual_op)
                {
                    return Some(true);
                }
            }
            *bindings = snap;
            false
        }

        PatKind::IntUnaryAny { op: op_var, operand } => {
            let NodeKind::IntUnaryOp(actual_op) = kind else {
                return Some(false);
            };
            let Ok([inp]) = matcher.fn_graph.graph.node_inputs_exact::<1>(node) else {
                return Some(false);
            };
            let snap = bindings.clone();
            if matcher.match_output(inp, operand, bindings)
                && bindings.bind_int_unary_op(*op_var, *actual_op)
            {
                return Some(true);
            }
            *bindings = snap;
            false
        }

        PatKind::IntCmpAny {
            op: op_var,
            lhs,
            rhs,
            ordered,
        } => {
            let NodeKind::IntCmpOp(actual_op) = kind else {
                return Some(false);
            };
            let Ok([l, r]) = matcher.fn_graph.graph.node_inputs_exact::<2>(node) else {
                return Some(false);
            };
            let snap = bindings.clone();
            if matcher.match_output(l, lhs, bindings)
                && matcher.match_output(r, rhs, bindings)
                && bindings.bind_int_cmp_op(*op_var, *actual_op)
            {
                return Some(true);
            }
            if !ordered && is_commutative_int_cmp_op(*actual_op) {
                *bindings = snap.clone();
                if matcher.match_output(r, lhs, bindings)
                    && matcher.match_output(l, rhs, bindings)
                    && bindings.bind_int_cmp_op(*op_var, *actual_op)
                {
                    return Some(true);
                }
            }
            *bindings = snap;
            false
        }

        _ => return None,
    };
    Some(result)
}
