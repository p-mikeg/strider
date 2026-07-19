//! When `CastMask` selects a cast `NodeKind`, the matcher unwraps the cast and
//! retries the sub-pattern against its value input. Region walk-through is
//! deliberately not supported: a pattern crossing a region boundary must spell
//! the `Region` node out.

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, ValueId};
use strider_ir::walk::{CastMask, cast_mask_of};

use crate::matcher::Matcher;

/// Unwrap cast producers per `mask`, returning the deepest `ValueId` reached and
/// pushing each skipped producer onto `skipped` in walk order.
///
/// The caller must record each skipped cast into the match footprint;
/// otherwise a rewrite culling a dead skipped cast drops its asm-fingerprint
/// and breaks the superset-only contract.
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
        let Some(input_value) = f.node_inputs(producer).into_iter().next() else {
            return value;
        };
        skipped.push(producer);
        value = input_value;
    }
}
