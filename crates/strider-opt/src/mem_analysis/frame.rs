//! Entry-SP geometry: the offsets a stack address can stand at, and whether
//! an access lies in the function's own frame.

use strider_ir::node::{NodeKind, ValueId};
use strider_ir::{Function, IRViewer, IntBinaryOp};

use super::decompose::alignment_masked_operand;
use super::ring::wrap_to_addr_width;

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
/// Walks the shapes [`super::decompose::decompose`] does (`Add` of a constant
/// and an alignment `And`) and, unlike it, on through a `Phi`, whose arms
/// widen the span rather than end the walk: the join holds one of them, so
/// their union bounds it. A revisited value (a loop-carried base), an
/// exhausted budget or any other shape answers `None`, which every caller
/// reads conservatively.
///
/// The offsets are read at the ADDRESS width, so a chain that wraps is judged
/// on the address it really forms.
///
/// Overflow: `hi` is checked, because under-reading it would call a caller's
/// frame our own and would cut bytes off the top of an access range. The
/// alignment slack only ever lowers `lo` and cannot overflow (at most
/// `MAX_STEPS` masks of at most `MAX_ALIGN_BITS` each), so it saturates;
/// `lo` itself is checked, since clamping it at the carrier floor would RAISE
/// it and cut bytes off the bottom.
pub(crate) fn entry_sp_span(function: &Function, value: ValueId) -> Option<(i128, i128)> {
    /// A real SP spine is a handful of links; this only bounds a pathological one.
    const MAX_STEPS: usize = 256;
    let (mut lo, mut hi) = (i128::MAX, i128::MIN);
    // Per pending value: the offset accrued above it, and how far the masks
    // above it can lower that.
    let mut work = vec![(value, 0i128, 0i128)];
    let mut seen: rustc_hash::FxHashSet<ValueId> = rustc_hash::FxHashSet::default();
    let mut steps = 0usize;
    while let Some((cur, acc, slack)) = work.pop() {
        steps += 1;
        if steps > MAX_STEPS {
            return None;
        }
        let node = function.producer(cur);
        match *function.node_kind(node) {
            NodeKind::InitialVar(_) if function.is_entry_sp(cur) => {
                let top = wrap_to_addr_width(function, cur, acc);
                lo = lo.min(top.checked_sub(slack)?);
                hi = hi.max(top);
            }
            // Masking only clears bits, so it can only move the base DOWN, by
            // at most one alignment less a byte.
            NodeKind::IntBinaryOp(IntBinaryOp::And) => {
                let operand = alignment_masked_operand(function, node)?;
                let [l, r] = function.node_inputs_exact::<2>(node).ok()?;
                let mask = function.int_const_u128(if operand == l { r } else { l })?;
                let align = 1i128 << mask.trailing_zeros();
                work.push((operand, acc, slack.saturating_add(align - 1)));
            }
            NodeKind::IntBinaryOp(IntBinaryOp::Add) => {
                let (next, k) = crate::const_eval::const_addend(function, node)?;
                work.push((next, acc.checked_add(k)?, slack));
            }
            NodeKind::Phi => {
                // A revisited value is a loop-carried base: its displacement is
                // not a constant, so it bounds nothing.
                if !seen.insert(cur) {
                    return None;
                }
                let arms = work.len();
                work.extend(function.value_inputs(node).map(|arm| (arm, acc, slack)));
                // An arm the walk never reads is a displacement it never bounds.
                if work.len() == arms {
                    return None;
                }
            }
            _ => return None,
        }
    }
    debug_assert!(lo <= hi, "the walk ends only on an entry-SP terminal");
    Some((lo, hi))
}
