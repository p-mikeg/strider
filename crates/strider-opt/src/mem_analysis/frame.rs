//! Whether a stack address lies in the function's own frame.

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
/// frame. The operand is therefore walked down to `InitialVar(sp)` and its
/// accumulated displacement required to be non-positive.
pub(super) fn in_own_frame(function: &Function, base: ValueId, offset: i128, size: i128) -> bool {
    let base_node = function.producer(base);
    let rooted_at_entry_sp = match *function.node_kind(base_node) {
        NodeKind::InitialVar(id) => function.initial_vn(id) == function.default_cc().stack_vn,
        _ => alignment_masked_operand(function, base_node)
            .is_some_and(|operand| at_or_below_entry_sp(function, operand, 0)),
    };
    // The END of the access, not its start: one that begins below the bound and
    // reaches over it touches the caller's block, which a callee may write.
    rooted_at_entry_sp
        && offset.saturating_add(size)
            <= function
                .default_cc()
                .stack_args
                .map_or(0, |args| args.base_offset)
}

/// Whether `value` is the entry SP displaced by a non-positive amount.
///
/// Follows the spine shapes [`super::decompose::decompose`] does
/// (`Add(x, const)` and an alignment `And`), accumulating the constants an
/// anchor would otherwise discard, and through a `Phi`, where EVERY arm must
/// qualify because the join could take any of them. The accumulated
/// displacement is compared at the address width, so a chain that wraps is
/// judged on the address it really forms. Anything else, a positive
/// displacement, a revisited value (a loop-carried base) or an exhausted budget
/// answers `false`, which only ever costs precision.
fn at_or_below_entry_sp(function: &Function, value: ValueId, acc: i128) -> bool {
    /// A real SP spine is a handful of links; this only bounds a pathological one.
    const MAX_STEPS: usize = 256;
    let sp = function.default_cc().stack_vn;
    let mut work = vec![(value, acc)];
    let mut seen: rustc_hash::FxHashSet<ValueId> = rustc_hash::FxHashSet::default();
    let mut steps = 0usize;
    while let Some((cur, acc)) = work.pop() {
        steps += 1;
        if steps > MAX_STEPS {
            return false;
        }
        let node = function.producer(cur);
        match *function.node_kind(node) {
            NodeKind::InitialVar(id) => {
                // Compare at the ADDRESS width, as `decompose` does: a raw
                // `i128` sum can be non-positive while the address it stands
                // for wrapped back above the entry SP, into the caller's frame.
                if function.initial_vn(id) != sp || wrap_to_addr_width(function, cur, acc) > 0 {
                    return false;
                }
            }
            // Masking only clears bits, so it can only move the base DOWN.
            NodeKind::IntBinaryOp(IntBinaryOp::And) => {
                let Some(operand) = alignment_masked_operand(function, node) else {
                    return false;
                };
                work.push((operand, acc));
            }
            NodeKind::IntBinaryOp(IntBinaryOp::Add) => {
                let Some((next, k)) = crate::const_eval::var_and_const(function, node) else {
                    return false;
                };
                let Some(sum) = function.int_const_i128(k).and_then(|k| acc.checked_add(k)) else {
                    return false;
                };
                work.push((next, sum));
            }
            NodeKind::Phi => {
                // A revisited value is a loop-carried base: its displacement is
                // not a constant, so it cannot be shown non-positive.
                if !seen.insert(cur) {
                    return false;
                }
                let arms = work.len();
                work.extend(function.value_inputs(node).map(|arm| (arm, acc)));
                // An arm the walk never reads is a displacement it never bounds.
                if work.len() == arms {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}
