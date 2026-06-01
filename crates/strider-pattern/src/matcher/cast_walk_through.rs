//! Cast walk-through helper for `MatcherOptions::cast_mask`.
//!
//! When the active `CastMask` selects a cast `NodeKind`, the recursive
//! matcher transparently unwraps the cast and re-attempts the sub-pattern
//! against the cast's value input.  Region walk-through (the analyzer's
//! companion mode) is deliberately NOT ported — patterns that cross
//! region boundaries must include the `Region` node explicitly.
//!
//! The structural classification of which `NodeKind`s are
//! value-passthrough casts lives in `strider_ir::walk::cast_mask_of`;
//! this helper owns only the iterative "unwrap one cast, retry" tail-
//! loop.  The implementation mirrors the proven semantics of
//! `strider-analyze::pattern::matcher::Matcher::match_output_with_walk_through`.

use strider_ir::node::NodeOutputId;
use strider_ir::walk::{cast_mask_of, CastMask};

use crate::matcher::MatchCtx;

/// Tail-loop that unwraps value-passthrough cast producers per `mask`
/// and returns the deepest `NodeOutputId` reached.
///
/// Stops as soon as the producer either is not a registered cast (per
/// `mask`), does not have exactly one input, or `mask` is empty.  Used
/// by `try_match.rs` after a direct kind-mismatch to retry the
/// sub-pattern against the cast's value input.
#[must_use]
pub(crate) fn skip_casts(ctx: &MatchCtx, out: NodeOutputId, mask: CastMask) -> NodeOutputId {
    if mask.is_empty() {
        return out;
    }
    let mut out = out;
    loop {
        let producer = ctx.function.node_for_output(out);
        let bit = cast_mask_of(ctx.function.node_kind(producer));
        if bit.is_empty() || !mask.contains(bit) {
            return out;
        }
        // Cast producers (Truncate / Extend / *BitsTo*) have exactly one
        // value input.  Take the first input if present; otherwise stop.
        let Some(value_input) = ctx.function.node_inputs(producer).into_iter().next() else {
            return out;
        };
        out = value_input;
    }
}
