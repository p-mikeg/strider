//! Producer-shape classifier for indirect-branch resolution.
//!
//! Walks the producer node of a placeholder anchor's value-input and
//! classifies it into a [`ResolvedTargets`].  Each arm is a
//! soundness-checked shape (`IntConst`, `InitialVar(lr)`, `ValuePhi`
//! of constants, jump-table load, stack-array load) — the comments on
//! each arm spell out why the runtime target set is constrained.
//!
//! [`ResolvedTargets`] is re-exported from `cfg`, so callers can pass
//! results from the classifier directly into
//! `cfg::Builder::with_known_targets`.

use ir::node::{NodeKind, NodeOutputId};

use super::ResolvedTargets;
use super::jump_table::classify_jump_table;
use crate::ReadOnlyMemory;

/// Classify a placeholder anchor's producer node into a
/// [`ResolvedTargets`].
///
/// Returns:
/// - `Ok(Some(_))` — successful classification.
/// - `Ok(None)` — producer doesn't match any of the known sound
///   shapes; the orchestrator interprets this as "still unresolved at
///   this iteration; try again or surface as
///   `UnresolvedIndirectBranch` at fixed point."
/// - `Err(_)` — `analyze_known_bits` returned a `Kb::merge`
///   contradiction (incompatible constants reaching the same
///   output).  Round 9 H-6: surface the diagnostic instead of
///   masking it as `None`; KB contradiction is a real IR-level bug
///   the caller should see explicitly.
///
/// Equivalent to [`classify_anchor_with_rom`] with `rom == None`.
/// The jump-table arm becomes a no-op in that case because
/// reading table entries requires rodata access; for callers that
/// have a rom, prefer the rom-aware form.
///
/// `link_register_vn` is the calling convention's link register
/// varnode (`None` on stack-push ABIs like x86 / x86_64 where there
/// is no architectural link register).  When `None`, the
/// `InitialVar(lr) → LinkRegister` arm is short-circuited — there
/// can be no LR match without a known LR varnode.
///
/// # Soundness
///
/// Every arm in this match must be a producer shape that, on the
/// optimised IR, **unambiguously** identifies the indirect branch's
/// runtime target.  Shapes the prior in-place heuristic tried
/// (`Load(InitialVar(sp))` for `pop pc`-style returns) are
/// deliberately NOT included here: a `push X; pop pc` tail call
/// has the same Load-shape and would be misclassified as a return.
/// We rely on `StackLoadForward` having already simplified
/// properly-popped return addresses to `InitialVar(lr_vn)` directly
/// — that's the shape the LinkRegister arm matches.
///
/// # Errors
///
/// Returns `Err` when `analyze_known_bits` fails (KB-merge contradiction).
pub fn classify_anchor(
    fg: pattern::RewriteCtxView<'_>,
    anchor_output: NodeOutputId,
    link_register_vn: Option<rsleigh::Vn>,
) -> anyhow::Result<Option<ResolvedTargets>> {
    let known = crate::analyze_known_bits(fg)?;
    Ok(classify_anchor_with_rom_and_sp(
        fg,
        anchor_output,
        link_register_vn,
        None,
        None,
        &known,
    ))
}

/// Classify a placeholder anchor with an optional [`ReadOnlyMemory`]
/// for the jump-table arm.
///
/// Same contract as [`classify_anchor`] for every shape that doesn't
/// require rom access; the only difference is the
/// `NodeKind::Load(_)` arm that pattern-matches the canonical
/// jump-table dispatch shape and reads its table entries via `rom`.
///
/// Pass `rom = None` to disable the jump-table arm; otherwise pass
/// the same rom the cfg builder + optimiser pipeline use (almost
/// always the ELF's `.rodata` + `.text` view), so the entries the
/// classifier reads agree with the entries downstream consumers
/// see.
///
/// # Errors
///
/// Returns `Err` when `analyze_known_bits` fails (KB-merge contradiction).
/// See [`classify_anchor`] for full Result-shape semantics.
pub fn classify_anchor_with_rom(
    fg: pattern::RewriteCtxView<'_>,
    anchor_output: NodeOutputId,
    link_register_vn: Option<rsleigh::Vn>,
    rom: Option<&dyn ReadOnlyMemory>,
) -> anyhow::Result<Option<ResolvedTargets>> {
    let known = crate::analyze_known_bits(fg)?;
    Ok(classify_anchor_with_rom_and_sp(
        fg,
        anchor_output,
        link_register_vn,
        rom,
        None,
        &known,
    ))
}

/// Classify a placeholder anchor with both an optional
/// [`ReadOnlyMemory`] (for the rodata jump-table arm) and an optional
/// stack-pointer varnode (for the stack-array-of-labels arm).
///
/// Same contract as [`classify_anchor`] for every shape unaffected by
/// either side-channel.  When `rom` is `None`, the rodata-jump-table
/// arm is short-circuited.  When `stack_ptr_vn` is `None`, the 
/// stack-array arm is short-circuited.
///
/// The orchestrator passes both: the rom for the binary-image rodata,
/// and the calling convention's stack-pointer varnode for the
/// stack-array shape.
///
/// # Soundness
///
/// Both new arms preserve the classifier's overall contract: the
/// resulting `ResolvedTargets::Multiple` enumerates the *full* set of
/// possible runtime targets.  Failing closed (returning `None`) on
/// any partial proof defers the branch to a later iteration or to
/// `UnresolvedIndirectBranch` at fixed point — never under-
/// approximating.
#[must_use]
pub fn classify_anchor_with_rom_and_sp(
    fg: pattern::RewriteCtxView<'_>,
    anchor_output: NodeOutputId,
    link_register_vn: Option<rsleigh::Vn>,
    rom: Option<&dyn ReadOnlyMemory>,
    stack_ptr_vn: Option<rsleigh::Vn>,
    known: &crate::KnownBitsMap,
) -> Option<ResolvedTargets> {
    let graph = &fg.graph;
    let producer_id = graph.get_node_from_output(anchor_output);
    let kind = *graph.node_kind(producer_id);
    match kind {
        // SOUND: a literal constant in the IR comes from one of:
        //   - a tracked IntConst pcode insn in the source region,
        //   - constant folding (`ConstantFold`),
        //   - a `LoadReadOnly` resolution against the binary's rodata.
        // All three are deterministic functions of the function's
        // pcode, so the same address is the only possible runtime
        // target of this BranchIndirect.
        NodeKind::IntConst(k) => {
            #[allow(clippy::cast_possible_truncation)]
            let truncated = k as u64;
            Some(ResolvedTargets::Single(truncated))
        }
        // SOUND: `InitialVar(vn)` is the function-entry value of
        // varnode `vn`.  When `vn == lr_vn`, the indirect branch
        // dispatches to the caller-provided return address — i.e. a
        // standard return.  This is the shape `StackLoadForward`
        // produces for properly-popped return addresses.
        NodeKind::InitialVar(vn) if Some(vn) == link_register_vn => {
            Some(ResolvedTargets::LinkRegister)
        }
        // SOUND: `ValuePhi`'s output is the merge of one
        // per-predecessor value input (slot 0 is the phi token,
        // slots 1.. are the values).  When *every* value input folds
        // to `IntConst(k_i)`, the runtime target set is exactly
        // `{k_i}` for the predecessors that ever reach this branch.
        NodeKind::ValuePhi => {
            let inputs: Vec<NodeOutputId> =
                graph.node_inputs(producer_id).into_iter().collect();
            let mut targets = Vec::with_capacity(inputs.len().saturating_sub(1));
            for &val in inputs.iter().skip(1) {
                match graph.kind_of_output(val) {
                    NodeKind::IntConst(k) => {
                        #[allow(clippy::cast_possible_truncation)]
                        targets.push(*k as u64);
                    }
                    _ => return None,
                }
            }
            targets.sort_unstable();
            targets.dedup();
            // SOUND: an empty `Multiple` would silently advertise zero
            // runtime targets, making the dispatch site appear
            // unreachable.  Defer instead.
            if targets.is_empty() {
                None
            } else {
                Some(ResolvedTargets::Multiple(targets))
            }
        }
        // Jump-table arm.  Producer is a Load — a candidate for
        // the canonical `Load(IntAdd(IntConst(base), IntMul(idx,
        // IntConst(stride))))` jump-table dispatch shape.
        //
        // when the rodata jump-table arm doesn't match and
        // an SP varnode is supplied, fall through to
        // `stack_array::classify_stack_array` which handles the
        // computed-goto-via-local-stack-array shape.  Both arms fail
        // closed (return None) on any partial proof.
        NodeKind::Load(_) => {
            if let Some(r) =
                classify_jump_table(fg, anchor_output, rom, link_register_vn, known)
            {
                return Some(r);
            }
            if let Some(sp) = stack_ptr_vn {
                return super::stack_array::classify_stack_array(fg, anchor_output, sp, known);
            }
            None
        }
        // ARM / arm-thumb / arm-be lifters wrap the
        // dispatch target in `IntBinaryOp(And)` with a constant mask
        // (`& 0xFFFFFFFE` for 32-bit ARM Thumb-interworking).  The
        // stack_array classifier transparently strips the mask, so
        // route And-anchors through the same arm — but only when the
        // SP varnode is supplied.
        NodeKind::IntBinaryOp(ir::IntBinaryOp::And) => {
            if let Some(sp) = stack_ptr_vn {
                return super::stack_array::classify_stack_array(fg, anchor_output, sp, known);
            }
            None
        }
        // Width-conversion peeling: `Extend(IntConst(K))` and
        // `Truncate(IntConst(K))` produce a value that's a deterministic
        // function of `K`, so the dispatch is `Single(K & out_mask)` for
        // truncation and `Single(K)` (zero/sign-extended) for extension.
        // SOUND because:
        //   - `Truncate` masks the input to the declared output width;
        //   - `Extend(Zero)` zero-fills upper bits;
        //   - `Extend(Sign)` sign-fills based on the input's high bit.
        // All three are deterministic with the original `K`.
        //
        // This shape arises when a compiler stores a target into a
        // narrower register (e.g. 32-bit `MOV r4, #target; BX r4` on
        // 32-bit ARM, where the `BX r4` lift may zero-extend r4's
        // value to 64-bit — even though the architectural pointer is
        // 32-bit, the IR's NodeOutputType for the dispatch slot may be
        // U64 to match the target's address width).
        //
        // Truncate's output mask is the truncated width; the lower bits
        // of K are the dispatch target.  Extend preserves K's value
        // when the input fits the output width.
        NodeKind::Truncate => {
            let inputs: Vec<NodeOutputId> =
                graph.node_inputs(producer_id).into_iter().collect();
            if let Some(&inner) = inputs.first()
                && let NodeKind::IntConst(k) = graph.kind_of_output(inner)
            {
                // Output type narrower than input — mask K to the
                // output width.  `output_kind(anchor_output).as_value()`
                // gives the declared output type, whose bit_mask_u128
                // covers exactly the kept bits.
                if let Some(out_ty) = graph.output_kind(anchor_output).as_value() {
                    let masked = (*k) & out_ty.bit_mask_u128();
                    #[allow(clippy::cast_possible_truncation)]
                    let truncated = masked as u64;
                    return Some(ResolvedTargets::Single(truncated));
                }
            }
            None
        }
        NodeKind::Extend(op) => {
            let inputs: Vec<NodeOutputId> =
                graph.node_inputs(producer_id).into_iter().collect();
            if let Some(&inner) = inputs.first()
                && let NodeKind::IntConst(k) = graph.kind_of_output(inner)
            {
                // Round 9 IMPORTANT (R9-1C Issue 1): correctly handle
                // both extension flavours.  Pre-fix the arm used
                // `(*k) as u64` for both, which is wrong for
                // `SignExtend(IntConst(neg_value, narrow_ty))` — the
                // u128 storage holds the narrow value with zero high
                // bits, so a sign-negative narrow constant would be
                // truncated to u64 with the high bits cleared instead
                // of sign-filled.
                //
                // In production this arm is normally dead because
                // `ConstantFold` rules 5/6 fold `Zero/SignExtend(IntConst)`
                // before the classifier runs and `extend_if_needed`
                // folds at build time.  But the unit-test path can
                // bypass both, and a future caller that constructs the
                // shape directly should still get the correct answer.
                let truncated = match op {
                    ir::ExtendOp::ZeroExtend => {
                        // Zero-extension is the identity in u128
                        // storage (the inner IntConst already has
                        // zeros in the high bits).
                        #[allow(clippy::cast_possible_truncation)]
                        let v = (*k) as u64;
                        v
                    }
                    ir::ExtendOp::SignExtend => {
                        // Sign-extend: read the inner constant as a
                        // signed value at its declared input width,
                        // then mask to the dispatch slot's output
                        // width (typically 64-bit).
                        let in_ty = graph.output_kind(inner).as_value()?;
                        let signed = in_ty.get_signed_int(*k)?;
                        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                        let v = signed as u64;
                        v
                    }
                };
                return Some(ResolvedTargets::Single(truncated));
            }
            None
        }
        _ => None,
    }
}
