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

use strider_ir::{
    IRViewer,
    node::{NodeId, ValueId},
    walk::{CastMask, cast_mask_of},
};

use crate::matcher::Matcher;

/// Tail-loop that unwraps value-passthrough cast producers per `mask`,
/// returning the deepest `ValueId` reached and pushing every skipped cast
/// producer `NodeId` onto `skipped` (in walk order).
///
/// Stops as soon as the producer either is not a registered cast (per
/// `mask`), does not have exactly one input, or `mask` is empty. Used by
/// the walk engine after a direct mismatch to retry the sub-pattern
/// against the cast's value input.
///
/// The `skipped` casts are part of the IR the match relies on, so the walk
/// engine records them into the match footprint
/// ([`crate::bindings::Bindings::record_matched`]) on a successful
/// cast-fallback — without them a rewrite that culls a dead skipped cast
/// would drop its asm-fingerprint, violating the superset-only contract.
pub(crate) fn skip_casts(
    matcher: &Matcher,
    value: ValueId,
    mask: CastMask,
    skipped: &mut Vec<NodeId>,
) -> ValueId {
    if mask.is_empty() {
        return value;
    }
    let f = matcher.function();
    let mut value = value;
    loop {
        let producer = f.producer(value);
        let bit = cast_mask_of(f.node_kind(producer));
        if bit.is_empty() || !mask.contains(bit) {
            return value;
        }
        // Cast producers (Truncate / Extend / *BitsTo*) have exactly one
        // value input. Take the first input if present; otherwise stop.
        let Some(input_value) = f.node_inputs(producer).into_iter().next() else {
            return value;
        };
        skipped.push(producer);
        value = input_value;
    }
}
