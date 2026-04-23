//! Wildcard + constant-shaped `PatKind` arms.
//!
//! Handles: `Any`, `Capture`, `IntConst`, `BoolConst`, `FloatConst`,
//! `AnyIntConst`, `AnyBoolConst`, `AnyFloatConst`,
//! `AnyIntConstTyped`, `AnyBoolConstTyped`, `AnyFloatConstTyped`.

use ir::node::{NodeKind, NodeOutputId};

use super::super::Matcher;
use super::super::bindings::Bindings;
use crate::pat::{Pat, PatKind};

pub(super) fn match_constants(
    matcher: &Matcher,
    output: NodeOutputId,
    pat: &Pat,
    bindings: &mut Bindings,
) -> Option<bool> {
    let node = matcher.fn_graph.graph.get_node_from_output(output);
    let kind = matcher.fn_graph.graph.node_kind(node);

    let result = match pat.as_legacy()? {
        PatKind::Any => true,

        PatKind::Capture(v) => bindings.bind_var(*v, output),

        PatKind::IntConst(c) => matches!(kind, NodeKind::IntConst(v) if *v == *c),

        PatKind::BoolConst(c) => matches!(kind, NodeKind::BoolConst(v) if *v == *c),

        PatKind::FloatConst(c) => matches!(kind, NodeKind::FloatConst(v) if *v == *c),

        PatKind::AnyIntConst(v) => {
            if !matches!(kind, NodeKind::IntConst(_)) {
                return Some(false);
            }
            bindings.bind_var(*v, output)
        }

        PatKind::AnyBoolConst(v) => {
            if !matches!(kind, NodeKind::BoolConst(_)) {
                return Some(false);
            }
            bindings.bind_var(*v, output)
        }

        PatKind::AnyFloatConst(v) => {
            if !matches!(kind, NodeKind::FloatConst(_)) {
                return Some(false);
            }
            bindings.bind_var(*v, output)
        }

        PatKind::AnyIntConstTyped(iv) => {
            let NodeKind::IntConst(val) = kind else {
                return Some(false);
            };
            bindings.bind_int(*iv, *val)
        }

        PatKind::AnyBoolConstTyped(bv) => {
            let NodeKind::BoolConst(val) = kind else {
                return Some(false);
            };
            bindings.bind_bool(*bv, *val)
        }

        PatKind::AnyFloatConstTyped(fv) => {
            let NodeKind::FloatConst(bits) = kind else {
                return Some(false);
            };
            bindings.bind_float(*fv, *bits)
        }

        _ => return None,
    };
    Some(result)
}
