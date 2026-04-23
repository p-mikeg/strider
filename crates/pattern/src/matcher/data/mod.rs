//! Data-level pattern dispatch: `match_output`.
//!
//! All data-level `PatKind` families have migrated to the trait-based engine.
//! This dispatcher only exists as the Legacy fallthrough for `Pat`s that are
//! still on the `PatKind` path — today that's exclusively the control-level
//! variants (Call / CallOther / Return / If / Contains), which cannot match
//! in a data context and therefore return `false` here.
//!
//! Phase 2.1 migrated wildcards + constants + guards; Phase 2.2 the Int
//! family (binary/unary/cmp + *Any variants); Phase 2.3 the Bool family;
//! Phase 2.4 the Float family (incl. int↔float conversions); Phase 2.5 the
//! Casts family (CastToBool / CastToInt / CastToFloat / Truncate / Extend /
//! Popcount / Lzcount); Phase 2.6 the Memory family (Load / Store /
//! StackStore / StackStorePhi); Phase 2.7 the Phi / InitialVar / FunctionArg
//! trio.  Those pats never reach this dispatcher (they go through
//! [`crate::pat::traits::DataPattern`] via [`Matcher::match_output`]).
//!
//! Phase 3 will migrate the remaining control-level variants, after which
//! this file can be deleted entirely.

use ir::node::NodeOutputId;

use super::Matcher;
use super::bindings::Bindings;
use crate::pat::{Pat, PatKind};

/// Match a `NodeOutputId` (data edge) against a pattern.
///
/// Returns `true` and updates `bindings` on success.  On failure returns
/// `false`; the caller is responsible for restoring `bindings` if needed.
pub(super) fn match_output(
    _matcher: &Matcher,
    _output: NodeOutputId,
    pat: &Pat,
    _bindings: &mut Bindings,
) -> bool {
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
        // Every `PatKind` constructible today is covered by the control
        // fallthrough above — no further arms exist.  A future variant that
        // slipped through would match here and be treated as a non-match,
        // preserving the prior catch-all semantics.
        _ => false,
    }
}
