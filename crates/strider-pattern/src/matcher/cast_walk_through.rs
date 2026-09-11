//! When `CastMask` selects a cast `NodeKind`, the matcher unwraps the cast and
//! retries the sub-pattern against its value input. Casts are the only kind
//! walked through: a pattern crossing a region boundary spells the `Region`
//! node out.

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, ValueId};
use strider_ir::walk::{CastMask, cast_mask_of};

use crate::matcher::Matcher;

/// Every level reachable by unwrapping cast producers per `mask`, outermost
/// first, as `(the cast unwrapped, the value below it)`.
///
/// EVERY level is yielded, not just the deepest: a pattern that names one cast
/// kind while `mask` also selects another must still be tried against the
/// intermediate value. Stopping at the bottom silently drops those matches.
///
/// The caller must record each unwrapped cast into the match footprint;
/// otherwise a rewrite culling a dead skipped cast drops its asm-fingerprint
/// and breaks the superset-only contract.
pub(crate) fn cast_levels(
    matcher: &Matcher,
    value: ValueId,
    mask: CastMask,
) -> Vec<(NodeId, ValueId)> {
    let mut levels = Vec::new();
    if mask.is_empty() {
        return levels;
    }
    let f = matcher.function();
    let mut value = value;
    loop {
        let producer = f.producer(value);
        let bit = cast_mask_of(f.node_kind(producer));
        if bit.is_empty() || !mask.contains(bit) {
            return levels;
        }
        let Some(input_value) = f.node_inputs(producer).into_iter().next() else {
            return levels;
        };
        levels.push((producer, input_value));
        value = input_value;
    }
}
