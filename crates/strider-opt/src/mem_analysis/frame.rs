//! Entry-SP geometry: the offsets a stack address can stand at, and whether
//! an access lies in the function's own frame.

use strider_ir::node::ValueId;
use strider_ir::{Function, IRViewer};

use super::decompose::{alignment_masked_operand, decompose};
use super::ring::wrap_to_addr_width;
use super::{MemExpr, MemKind};

/// Does the access `[base + offset, base + offset + size)` lie wholly in THIS
/// function's own frame?
///
/// [`super::frame_escape`] proves only that no address of this frame escaped
/// from this function's BODY.  At the entry SP plus the ABI's stack-argument
/// base and above lies the CALLER's outgoing-argument block, which the caller
/// may hold or have leaked a pointer into; nothing here can reveal that, so a
/// callee must be assumed able to write it.  Below that bound is this frame
/// plus the reserved area the ABI gives the callee to scratch.
///
/// `base_offset` is the first incoming slot on every supported ABI and
/// `validate` rejects a negative one, so the stack always grows away from it:
/// x86-64 8, i386 4, ARM / AArch64 / MIPS n64 0, MIPS o32 16, PPC32 8,
/// PPC64 ELFv1 112 / ELFv2 96 (linkage area plus the 64-byte parameter
/// save area).  An ABI declaring none bounds at 0, since the
/// caller's frame still starts at the entry SP.
///
/// The comparison is in the entry SP's coordinates, so the base has to be AT OR
/// BELOW it. `InitialVar(sp)` is exactly it, and masking only clears bits, but
/// the alignment anchor is a mask of whatever SP-rooted expression the spine was
/// on, so `(sp + K) & !0xF` sits K bytes ABOVE the entry SP, inside the caller's
/// frame. The base's [`entry_sp_span`] therefore has to top out at or below
/// zero; how far below is this frame's own business.
pub(super) fn in_own_frame(function: &Function, base: ValueId, offset: i128, size: i128) -> bool {
    // The END of the access, not its start: one that begins below the bound and
    // reaches over it touches the caller's block, which a callee may write.
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
/// Climbs the anchors [`decompose`] names: each alignment mask below an
/// anchor can lower it by at most one alignment less a byte, so the offsets
/// read down to the entry SP give the top and the masks the slack below it.
/// A spine `decompose` does not bottom out on (a `Phi`, a heap base, any
/// other shape) answers `None`, which every caller reads conservatively.
///
/// The offsets are read at the ADDRESS width, so a chain that wraps is judged
/// on the address it really forms.
///
/// Overflow: the sum is checked, because under-reading `hi` would call a
/// caller's frame our own and clamping `lo` would cut bytes off the bottom.
/// The slack only lowers `lo` and saturates.
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
