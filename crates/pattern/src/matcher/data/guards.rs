//! Post-match guard `PatKind` arms.
//!
//! Handles: `WithCapture`, `WithPredicate`, `WithMatchPredicate`.
//! Each guard first evaluates its inner pattern, then runs an extra
//! binding / predicate check on top.

use ir::node::NodeOutputId;

use super::super::Matcher;
use super::super::bindings::Bindings;
use crate::pat::{Pat, PatKind};

pub(super) fn match_guards(
    matcher: &Matcher,
    output: NodeOutputId,
    pat: &Pat,
    bindings: &mut Bindings,
) -> Option<bool> {
    let result = match pat.inner() {
        PatKind::WithCapture { inner, var } => {
            let snap = bindings.clone();
            if !matcher.match_output(output, inner, bindings) {
                return Some(false);
            }
            if bindings.bind_var(*var, output) {
                true
            } else {
                *bindings = snap;
                false
            }
        }

        PatKind::WithPredicate { inner, func } => {
            let snap = bindings.clone();
            if !matcher.match_output(output, inner, bindings) {
                return Some(false);
            }
            let Some(out_ty) = matcher.fn_graph.graph.output_kind(output).as_value() else {
                *bindings = snap;
                return Some(false);
            };
            if func(matcher.fn_graph, out_ty, output) {
                true
            } else {
                *bindings = snap;
                false
            }
        }

        PatKind::WithMatchPredicate { inner, func } => {
            let snap = bindings.clone();
            if !matcher.match_output(output, inner, bindings) {
                return Some(false);
            }
            let Some(out_ty) = matcher.fn_graph.graph.output_kind(output).as_value() else {
                *bindings = snap;
                return Some(false);
            };
            if func(matcher.fn_graph, out_ty, bindings) {
                true
            } else {
                *bindings = snap;
                false
            }
        }

        _ => return None,
    };
    Some(result)
}
