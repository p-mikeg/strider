//! Data-level pattern dispatch: `match_output`.
//!
//! `match_output` matches a `NodeOutputId` (data edge) against a `Pat`.  The
//! bulk of the logic is split into per-family submodules — each submodule
//! exposes a `match_<family>(matcher, output, pat, bindings) -> Option<bool>`
//! that returns `None` if the `PatKind` doesn't belong to that family, or
//! `Some(b)` with the match result otherwise.  The dispatcher below chains
//! every family in priority order.
//!
//! Families (remaining on the Legacy path):
//! * `memory`    — Load / Store / StackStore / StackStorePhi
//! * `phi`       — Phi / InitialVar
//!
//! Wildcards + constants + guards migrated to the trait-based engine in
//! Phase 2.1; the Int family (binary/unary/cmp + *Any variants) migrated in
//! Phase 2.2; the Bool family (binary/unary + *Any variants) migrated in
//! Phase 2.3; the Float family (binary/unary/cmp + *Any variants + int↔float
//! conversions) migrated in Phase 2.4; the Casts family (CastToBool /
//! CastToInt / CastToFloat / Truncate / Extend / Popcount / Lzcount)
//! migrated in Phase 2.5.  Those pats never reach this dispatcher (they go
//! through [`crate::pat::traits::DataPattern`] via
//! [`Matcher::match_output`]).
//!
//! Control-level patterns (Call / CallOther / Return / If / Contains) cannot
//! match in a data context — the dispatcher returns `false` for them,
//! preserving the original behaviour of `match_output`.

use ir::node::NodeOutputId;

use super::Matcher;
use super::bindings::Bindings;
use crate::pat::{Pat, PatKind};

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
    if let Some(r) = memory::match_memory(matcher, output, pat, bindings) {
        return r;
    }
    if let Some(r) = phi::match_phi(matcher, output, pat, bindings) {
        return r;
    }

    // Control-level patterns in a data context → no match.  Preserves the
    // original behaviour of the single-file `match_output`.  A `Pat` that
    // has migrated off the legacy path (i.e. `as_legacy()` returns `None`)
    // is never routed here by `Matcher::match_output`, so the `None` arm
    // only appears for defensive completeness.
    match pat.as_legacy() {
        Some(PatKind::Call { .. })
        | Some(PatKind::CallOther { .. })
        | Some(PatKind::Return { .. })
        | Some(PatKind::If { .. })
        | Some(PatKind::Contains(_)) => false,
        // Every `PatKind` constructible today is covered by one of the
        // families above or by the control fallthrough — no further arms
        // exist.  A future variant that slipped through would match here
        // and be treated as a non-match, preserving the prior catch-all
        // semantics.
        _ => false,
    }
}
