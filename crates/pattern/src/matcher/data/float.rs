//! Floating-point arithmetic / comparison / conversion `PatKind` arms.
//!
//! Handles:
//! * `FloatBinaryOp`, `FloatUnaryOp`, `FloatCmpOp`
//! * `FloatBinaryAny`, `FloatUnaryAny`, `FloatCmpAny`
//! * int ↔ float conversions: `IntToFloat`, `FloatToInt`, `FloatToFloat`,
//!   `IntBitsToFloat`, `FloatBitsToInt`
//!
//! `FloatConst` / `AnyFloatConst` / `AnyFloatConstTyped` live in
//! `constants.rs`; `CastToFloat` lives in `casts.rs`.

use ir::node::{NodeKind, NodeOutputId};

use super::super::Matcher;
use super::super::bindings::Bindings;
use super::super::commutativity::is_commutative_float_op;
use crate::pat::{Pat, PatKind};

pub(super) fn match_float(
    matcher: &Matcher,
    output: NodeOutputId,
    pat: &Pat,
    bindings: &mut Bindings,
) -> Option<bool> {
    let node = matcher.fn_graph.graph.get_node_from_output(output);
    let kind = matcher.fn_graph.graph.node_kind(node);

    let result = match pat.as_legacy()? {
        PatKind::FloatBinaryOp {
            op,
            lhs,
            rhs,
            ordered,
        } => {
            let NodeKind::FloatBinaryOp(actual) = kind else {
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
            if !ordered && is_commutative_float_op(*op) {
                *bindings = snap.clone();
                if matcher.match_output(r, lhs, bindings) && matcher.match_output(l, rhs, bindings)
                {
                    return Some(true);
                }
            }
            *bindings = snap;
            false
        }

        PatKind::FloatUnaryOp { op, operand } => {
            let NodeKind::FloatUnaryOp(actual) = kind else {
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

        PatKind::FloatCmpOp { op, lhs, rhs } => super::match_binary_op(
            matcher,
            node,
            lhs,
            rhs,
            bindings,
            |k| matches!(k, NodeKind::FloatCmpOp(actual) if actual == op),
        ),

        PatKind::FloatBinaryAny {
            op: op_var,
            lhs,
            rhs,
            ordered,
        } => {
            let NodeKind::FloatBinaryOp(actual_op) = kind else {
                return Some(false);
            };
            let Ok([l, r]) = matcher.fn_graph.graph.node_inputs_exact::<2>(node) else {
                return Some(false);
            };
            let snap = bindings.clone();
            if matcher.match_output(l, lhs, bindings)
                && matcher.match_output(r, rhs, bindings)
                && bindings.bind_float_binary_op(*op_var, *actual_op)
            {
                return Some(true);
            }
            if !ordered && is_commutative_float_op(*actual_op) {
                *bindings = snap.clone();
                if matcher.match_output(r, lhs, bindings)
                    && matcher.match_output(l, rhs, bindings)
                    && bindings.bind_float_binary_op(*op_var, *actual_op)
                {
                    return Some(true);
                }
            }
            *bindings = snap;
            false
        }

        PatKind::FloatUnaryAny { op: op_var, operand } => {
            let NodeKind::FloatUnaryOp(actual_op) = kind else {
                return Some(false);
            };
            let Ok([inp]) = matcher.fn_graph.graph.node_inputs_exact::<1>(node) else {
                return Some(false);
            };
            let snap = bindings.clone();
            if matcher.match_output(inp, operand, bindings)
                && bindings.bind_float_unary_op(*op_var, *actual_op)
            {
                return Some(true);
            }
            *bindings = snap;
            false
        }

        PatKind::FloatCmpAny {
            op: op_var,
            lhs,
            rhs,
            ordered,
        } => {
            // The `ordered` flag is read here only to keep the field "live"
            // from rustc's dead-code perspective; no float comparison
            // operator is commutative in the existing helpers, so the swap
            // path below is never actually exercised.
            let _ = ordered;
            // No float comparison operators are commutative in the existing
            // helpers, so the `ordered` flag has no effect here — the swap
            // path is never taken.  The field is retained for API symmetry
            // with the other binary-any variants.
            let NodeKind::FloatCmpOp(actual_op) = kind else {
                return Some(false);
            };
            let Ok([l, r]) = matcher.fn_graph.graph.node_inputs_exact::<2>(node) else {
                return Some(false);
            };
            let snap = bindings.clone();
            if matcher.match_output(l, lhs, bindings)
                && matcher.match_output(r, rhs, bindings)
                && bindings.bind_float_cmp_op(*op_var, *actual_op)
            {
                return Some(true);
            }
            *bindings = snap;
            false
        }

        PatKind::IntToFloat { operand } => super::match_unary_op(
            matcher,
            node,
            operand,
            bindings,
            |k| matches!(k, NodeKind::IntToFloat),
        ),

        PatKind::FloatToInt { operand } => super::match_unary_op(
            matcher,
            node,
            operand,
            bindings,
            |k| matches!(k, NodeKind::FloatToInt),
        ),

        PatKind::FloatToFloat { operand } => super::match_unary_op(
            matcher,
            node,
            operand,
            bindings,
            |k| matches!(k, NodeKind::FloatToFloat),
        ),

        PatKind::IntBitsToFloat { operand } => super::match_unary_op(
            matcher,
            node,
            operand,
            bindings,
            |k| matches!(k, NodeKind::IntBitsToFloat),
        ),

        PatKind::FloatBitsToInt { operand } => super::match_unary_op(
            matcher,
            node,
            operand,
            bindings,
            |k| matches!(k, NodeKind::FloatBitsToInt),
        ),

        _ => return None,
    };
    Some(result)
}
