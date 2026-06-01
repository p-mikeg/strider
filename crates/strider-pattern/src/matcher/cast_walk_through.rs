//! Cast walk-through helper for the pattern's `CastMask`.
//!
//! When the active [`CastMask`] selects a cast `NodeKind`, the recursive
//! matcher transparently unwraps the cast and re-attempts the sub-pattern
//! against the cast's value input. Region walk-through is deliberately
//! NOT supported — patterns that cross region boundaries must include the
//! `Region` node explicitly.
//!
//! The structural classification of which `NodeKind`s are
//! value-passthrough casts lives in `strider_ir::walk::cast_mask_of`;
//! this helper owns only the iterative "unwrap one cast, retry" tail-loop.

use strider_ir::node::NodeOutputId;
use strider_ir::walk::{CastMask, cast_mask_of};

use crate::matcher::Matcher;

/// Tail-loop that unwraps value-passthrough cast producers per `mask`
/// and returns the deepest `NodeOutputId` reached.
///
/// Stops as soon as the producer either is not a registered cast (per
/// `mask`), does not have exactly one input, or `mask` is empty. Used by
/// the walk engine after a direct mismatch to retry the sub-pattern
/// against the cast's value input.
#[must_use]
pub(crate) fn skip_casts(matcher: &Matcher, out: NodeOutputId, mask: CastMask) -> NodeOutputId {
    if mask.is_empty() {
        return out;
    }
    let f = matcher.function();
    let mut out = out;
    loop {
        let producer = f.node_for_output(out);
        let bit = cast_mask_of(f.node_kind(producer));
        if bit.is_empty() || !mask.contains(bit) {
            return out;
        }
        // Cast producers (Truncate / Extend / *BitsTo*) have exactly one
        // value input. Take the first input if present; otherwise stop.
        let Some(value_input) = f.node_inputs(producer).into_iter().next() else {
            return out;
        };
        out = value_input;
    }
}
