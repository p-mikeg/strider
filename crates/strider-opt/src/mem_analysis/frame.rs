//! Entry-SP geometry: the offsets a stack address can stand at, and whether
//! an access lies in the function's own frame.

use strider_ir::node::ValueId;
use strider_ir::{Function, IRViewer};

use super::decompose::{alignment_masked_operand, decompose};
use super::ring::wrap_to_addr_width;
use super::{MemExpr, MemKind};

/// Whether `[base + offset, base + offset + size)` lies wholly in this
/// function's own frame: below the entry SP plus the ABI's stack-argument
/// base (0 without one).  At and above it is the caller's outgoing-argument
/// block, which the caller may have leaked a pointer into.
///
/// The bound is in entry-SP coordinates, so the base's [`entry_sp_span`] must
/// top out at or below zero: `(sp + K) & !0xF` sits above the entry SP.
pub(super) fn in_own_frame(function: &Function, base: ValueId, offset: i128, size: i128) -> bool {
    // The access's end, not its start, is bounded.
    entry_sp_span(function, base).is_some_and(|(_, hi)| hi <= 0)
        && offset.saturating_add(size)
            <= function
                .default_cc()
                .stack_args
                .map_or(0, |args| args.base_offset)
}

/// The lowest and highest offset from the entry SP `value` can stand at, or
/// `None` when no bound follows from its spine.
///
/// Climbs the anchors [`decompose`] names: each alignment mask lowers the
/// value by at most one alignment less a byte, so the offsets give the top
/// and the masks the slack below it.  Offsets are read at the address width.
/// The sum is checked, since an under-read `hi` would call the caller's frame
/// ours; the slack only lowers `lo` and saturates.
pub(crate) fn entry_sp_span(function: &Function, value: ValueId) -> Option<(i128, i128)> {
    let (mut cur, mut acc, mut slack) = (value, 0i128, 0i128);
    loop {
        let MemExpr {
            base,
            offset,
            kind: MemKind::Stack,
        } = decompose(function, cur)?
        else {
            return None;
        };
        acc = acc.checked_add(offset)?;
        if function.is_entry_sp(base) {
            let top = wrap_to_addr_width(function, base, acc);
            return Some((top.checked_sub(slack)?, top));
        }
        let (operand, align) = alignment_masked_operand(function, function.producer(base))?;
        slack = slack.saturating_add(align - 1);
        cur = operand;
    }
}
