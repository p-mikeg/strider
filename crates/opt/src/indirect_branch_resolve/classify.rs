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

use ir::BuiltFunctionGraph;
use ir::node::{NodeKind, NodeOutputId};

use super::ResolvedTargets;
use super::jump_table::classify_jump_table;
use crate::ReadOnlyMemory;

/// Classify a placeholder anchor's producer node into a
/// [`ResolvedTargets`].  Returns `None` when the producer doesn't
/// match any of the known sound shapes — the orchestrator interprets
/// `None` as "still unresolved at this iteration; try again or
/// surface as `UnresolvedIndirectBranch` at fixed point."
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
#[must_use]
pub fn classify_anchor(
    fg: &BuiltFunctionGraph,
    anchor_output: NodeOutputId,
    link_register_vn: Option<rsleigh::Vn>,
) -> Option<ResolvedTargets> {
    classify_anchor_with_rom(fg, anchor_output, link_register_vn, None)
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
#[must_use]
pub fn classify_anchor_with_rom(
    fg: &BuiltFunctionGraph,
    anchor_output: NodeOutputId,
    link_register_vn: Option<rsleigh::Vn>,
    rom: Option<&dyn ReadOnlyMemory>,
) -> Option<ResolvedTargets> {
    classify_anchor_with_rom_and_sp(fg, anchor_output, link_register_vn, rom, None)
}

/// Classify a placeholder anchor with both an optional
/// [`ReadOnlyMemory`] (for the rodata jump-table arm) and an optional
/// stack-pointer varnode (for the BUG-30 stack-array-of-labels arm).
///
/// Same contract as [`classify_anchor`] for every shape unaffected by
/// either side-channel.  When `rom` is `None`, the rodata-jump-table
/// arm is short-circuited.  When `stack_ptr_vn` is `None`, the BUG-30
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
    fg: &BuiltFunctionGraph,
    anchor_output: NodeOutputId,
    link_register_vn: Option<rsleigh::Vn>,
    rom: Option<&dyn ReadOnlyMemory>,
    stack_ptr_vn: Option<rsleigh::Vn>,
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
        // F3 / BUG-30: when the rodata jump-table arm doesn't match
        // and an SP varnode is supplied, fall through to
        // `stack_array::classify_stack_array` which handles the
        // computed-goto-via-local-stack-array shape.  Both arms fail
        // closed (return None) on any partial proof.
        NodeKind::Load(_) => {
            if let Some(r) = classify_jump_table(fg, anchor_output, rom, link_register_vn)
            {
                return Some(r);
            }
            if let Some(sp) = stack_ptr_vn {
                return super::stack_array::classify_stack_array(fg, anchor_output, sp);
            }
            None
        }
        // F3 / BUG-30: ARM / arm-thumb / arm-be lifters wrap the
        // dispatch target in `IntBinaryOp(And)` with a constant mask
        // (`& 0xFFFFFFFE` for 32-bit ARM Thumb-interworking).  The
        // stack_array classifier transparently strips the mask, so
        // route And-anchors through the same arm — but only when the
        // SP varnode is supplied.
        NodeKind::IntBinaryOp(ir::IntBinaryOp::And) => {
            if let Some(sp) = stack_ptr_vn {
                return super::stack_array::classify_stack_array(fg, anchor_output, sp);
            }
            None
        }
        _ => None,
    }
}
