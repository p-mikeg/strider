//! Cast walk-through for the pattern's `CastMask`.
//!
//! When `CastMask` selects a cast `NodeKind`, the matcher unwraps the cast and
//! retries the sub-pattern against its value input. Region walk-through is
//! deliberately not supported: a pattern crossing a region boundary must spell
//! the `Region` node out.
//!
//! Which kinds count as value-passthrough casts is decided by
//! `strider_ir::walk::cast_mask_of`; this file owns only the unwrap loop.

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, ValueId};
use strider_ir::walk::{CastMask, cast_mask_of};

use crate::matcher::Matcher;

/// Unwrap cast producers per `mask`, returning the deepest `ValueId` reached and
/// pushing each skipped producer onto `skipped` in walk order.
///
/// Skipped casts are load-bearing: the walk engine records them into the match
/// footprint ([`crate::bindings::Bindings::record_matched`]), otherwise a
/// rewrite culling a dead skipped cast would drop its asm-fingerprint and break
/// the superset-only contract.
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
