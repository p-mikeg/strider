//! Cast / bit-shape `PatKind` arms.
//!
//! Handles: `CastToBool`, `CastToInt`, `CastToFloat`, `Truncate`, `Extend`,
//! `Popcount`, `Lzcount`.  Each arm delegates to the shared `match_unary_op`
//! helper in `super` with a kind-predicate tailored to the variant.

use ir::node::{NodeKind, NodeOutputId};

use super::super::Matcher;
use super::super::bindings::Bindings;
use crate::pat::{Pat, PatKind};

pub(super) fn match_casts(
    matcher: &Matcher,
    output: NodeOutputId,
    pat: &Pat,
    bindings: &mut Bindings,
) -> Option<bool> {
    let node = matcher.fn_graph.graph.get_node_from_output(output);

    let result = match pat.inner() {
        PatKind::CastToBool { operand } => super::match_unary_op(
            matcher,
            node,
            operand,
            bindings,
            |k| matches!(k, NodeKind::CastToBool),
        ),

        PatKind::CastToInt { operand } => super::match_unary_op(
            matcher,
            node,
            operand,
            bindings,
            |k| matches!(k, NodeKind::CastToInt),
        ),

        PatKind::CastToFloat { operand } => super::match_unary_op(
            matcher,
            node,
            operand,
            bindings,
            |k| matches!(k, NodeKind::CastToFloat),
        ),

        PatKind::Truncate { operand } => super::match_unary_op(
            matcher,
            node,
            operand,
            bindings,
            |k| matches!(k, NodeKind::Truncate),
        ),

        PatKind::Popcount { operand } => super::match_unary_op(
            matcher,
            node,
            operand,
            bindings,
            |k| matches!(k, NodeKind::Popcount),
        ),

        PatKind::Lzcount { operand } => super::match_unary_op(
            matcher,
            node,
            operand,
            bindings,
            |k| matches!(k, NodeKind::Lzcount),
        ),

        PatKind::Extend { op, operand } => super::match_unary_op(
            matcher,
            node,
            operand,
            bindings,
            |k| matches!(k, NodeKind::Extend(actual) if actual == op),
        ),

        _ => return None,
    };
    Some(result)
}
