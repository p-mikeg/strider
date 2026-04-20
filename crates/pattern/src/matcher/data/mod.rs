//! Data-level pattern dispatch: `match_output`.
//!
//! `match_output` matches a `NodeOutputId` (data edge) against a `Pat`.  The
//! bulk of the logic is split into per-family submodules — each submodule
//! exposes a `match_<family>(matcher, output, pat, bindings) -> Option<bool>`
//! that returns `None` if the `PatKind` doesn't belong to that family, or
//! `Some(b)` with the match result otherwise.  The dispatcher below chains
//! every family in priority order.
//!
//! Families:
//! * `constants` — wildcards + constant-shaped patterns (Any, Capture, IntConst, …)
//! * `guards`    — post-match guards (WithCapture, WithPredicate, WithMatchPredicate)
//! * `int`       — integer binary / unary / cmp ops (both concrete and *Any variants)
//! * `bool_`     — boolean binary / unary ops
//! * `float`     — float binary / unary / cmp ops + int ↔ float conversions
//! * `casts`     — value-preserving casts + bit-level truncate / extend / popcount / lzcount
//! * `memory`    — Load / Store / StackStore / StackStorePhi
//! * `phi`       — Phi / InitialVar
//!
//! Control-level patterns (Call / CallOther / Return / If / Contains) cannot
//! match in a data context — the dispatcher returns `false` for them,
//! preserving the original behaviour of `match_output`.

use ir::node::{NodeId, NodeKind, NodeOutputId};

use super::Matcher;
use super::bindings::Bindings;
use crate::pat::{Pat, PatKind};

mod bool_;
mod casts;
mod constants;
mod float;
mod guards;
mod int;
mod memory;
mod phi;

/// Match a `NodeOutputId` (data edge) against a pattern.
///
/// Returns `true` and updates `bindings` on success.  On failure returns
/// `false`; the caller is responsible for restoring `bindings` if needed.
pub(super) fn match_output(
    matcher: &Matcher,
    output: NodeOutputId,
    pat: &Pat,
    bindings: &mut Bindings,
) -> bool {
    if let Some(r) = constants::match_constants(matcher, output, pat, bindings) {
        return r;
    }
    if let Some(r) = guards::match_guards(matcher, output, pat, bindings) {
        return r;
    }
    if let Some(r) = int::match_int(matcher, output, pat, bindings) {
        return r;
    }
    if let Some(r) = bool_::match_bool(matcher, output, pat, bindings) {
        return r;
    }
    if let Some(r) = float::match_float(matcher, output, pat, bindings) {
        return r;
    }
    if let Some(r) = casts::match_casts(matcher, output, pat, bindings) {
        return r;
    }
    if let Some(r) = memory::match_memory(matcher, output, pat, bindings) {
        return r;
    }
    if let Some(r) = phi::match_phi(matcher, output, pat, bindings) {
        return r;
    }

    // Control-level patterns in a data context → no match.  Preserves the
    // original behaviour of the single-file `match_output`.
    match pat.inner() {
        PatKind::Call { .. }
        | PatKind::CallOther { .. }
        | PatKind::Return { .. }
        | PatKind::If { .. }
        | PatKind::Contains(_) => false,
        // Every `PatKind` constructible today is covered by one of the
        // families above or by the control fallthrough — no further arms
        // exist.  A future variant that slipped through would match here
        // and be treated as a non-match, preserving the prior catch-all
        // semantics.
        _ => false,
    }
}

// ── shared helpers ────────────────────────────────────────────────────────

/// Check that `node` satisfies `kind_ok`, fetch its single input, and recurse
/// on `operand`.  Returns `false` (with bindings unchanged) if the kind check
/// or input-count check fails; otherwise propagates the result of
/// `match_output`.
pub(super) fn match_unary_op<F>(
    matcher: &Matcher,
    node: NodeId,
    operand: &Pat,
    bindings: &mut Bindings,
    kind_ok: F,
) -> bool
where
    F: FnOnce(&NodeKind) -> bool,
{
    if !kind_ok(matcher.fn_graph.graph.node_kind(node)) {
        return false;
    }
    let Ok([inp]) = matcher.fn_graph.graph.node_inputs_exact::<1>(node) else {
        return false;
    };
    let snap = bindings.clone();
    if matcher.match_output(inp, operand, bindings) {
        true
    } else {
        *bindings = snap;
        false
    }
}

/// Check that `node` satisfies `kind_ok`, fetch its two inputs, and try
/// matching `lhs`/`rhs` in order.  Backtracks on failure.
pub(super) fn match_binary_op<F>(
    matcher: &Matcher,
    node: NodeId,
    lhs: &Pat,
    rhs: &Pat,
    bindings: &mut Bindings,
    kind_ok: F,
) -> bool
where
    F: FnOnce(&NodeKind) -> bool,
{
    if !kind_ok(matcher.fn_graph.graph.node_kind(node)) {
        return false;
    }
    let Ok([l, r]) = matcher.fn_graph.graph.node_inputs_exact::<2>(node) else {
        return false;
    };
    let snap = bindings.clone();
    if matcher.match_output(l, lhs, bindings) && matcher.match_output(r, rhs, bindings) {
        true
    } else {
        *bindings = snap;
        false
    }
}
