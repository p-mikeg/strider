//! Whole-function frame-escape analysis: does any stack address reach a callee?
//! When none does, every spill slot is private, so a `Load` may forward across
//! a `Call`.
//!
//! `U` is the set of frame addresses: the values [`decompose()`] reduces to
//! `sp + k`.
//!
//! The frame is contaminated iff some member of `U` is consumed at any position
//! other than one that keeps it in the address world:
//!
//! - a node whose own output is in `U` (a constant-offset `Add` or alignment
//!   `And` extending the SP chain),
//! - the address operand of a `Load` / `Store`, or
//! - the SP-anchor slot of a `Call` (input 3; every callee receives `sp` by the
//!   ABI, so the anchor is not an argument).
//!
//! Any other use exposes the address: a `Store` data operand (it enters memory,
//! which is also how a stack argument is passed before `CallStackArgCollect`
//! runs), a `Call` / `CallOther` argument, a `Return`, or an op whose result
//! leaves `U` (pointer arithmetic). Detection is at that boundary, never at the
//! callee: an address only reaches a callee's readable memory by first crossing
//! one of these uses, so a pointer into already-written memory is caught at the
//! store that wrote it.
//!
//! A register reaches the callee without any use: a `Call` carries only the
//! argument registers its convention names, yet a callee may read one the
//! convention clobbers (i386 `regparm` in EAX, clang's `fastcc` in ECX, the
//! GNU C static chain, AArch64's indirect-result register X8).  The builder
//! records a stack-derived value held in such a register at any call, and that
//! record is an escape too.
//!
//! `decompose` under-approximates `U`: an unfolded offset such as `sp + (5 ^ 3)`
//! reads as outside `U` until `ConstantFold` collapses it. That stays sound,
//! because the `Add` consuming `sp` to produce the unrecognised result is itself
//! an exposing use, so contamination is flagged one node earlier; the memo
//! recomputes once folding converges.
//!
//! The verdict is one whole-function bit: a single leak poisons every call,
//! since a leaked pointer can be stored to a global and reloaded by a later
//! callee.
//!
//! A frame that stores its entry SP is exposed by that store: the PowerPC
//! `stwu`/`stdu` back chain and the ARM APCS frame record (`mov ip, sp; push
//! {.., ip, ..}`), so the whole-function verdict is `true` on both.  The
//! APCS frame also counts for [`incoming_area_escapes`], since its offset 0
//! lies inside ARM's register-argument homes, and it holds the entry SP in
//! `ip`, a clobbered register, at its first call.  The entry SP the PowerPC
//! back chain stores lies below every PowerPC area start, so it does not.
//!
//! [`incoming_area_escapes`] asks the narrower question of whether the
//! incoming-argument memory may be reached.  It places an exposure only where
//! the use hands over the address itself (a call argument, a stored value, a
//! return) and counts it unless it is the entry SP plus a constant below the
//! area, which names a frame object of its own; pointer arithmetic leaving the
//! object to a callee is outside the C object model.  Every other exposing use
//! (arithmetic leaving `U`, a `Phi`, a width change) counts whatever its
//! input's offset, as above: its result is a pointer `decompose` cannot place.
//! A stack address stored and reloaded is not followed, so a base below the
//! area that is spilled and then offset into it is missed; GCC and LLVM
//! rematerialise such a base rather than spill it.
//!
//! Sound modulo two claims the IR cannot check: a callee does not write the
//! caller's frame outside the argument area except through a pointer it was
//! handed in an argument, a clobbered register or memory, and no pointer is
//! fabricated numerically equal to a private slot.

use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{Function, IRViewer, IRWalker, IntBinaryOp};

use crate::mem_analysis::{MemKind, decompose};

/// A value is a frame address iff it decomposes to a *stack* base; a heap base
/// (an allocator's pointer) is not part of the frame `U`.
fn is_frame_addr(function: &Function, v: ValueId) -> bool {
    decompose(function, v).is_some_and(|e| e.kind == MemKind::Stack)
}

/// Cached [`frame_address_escapes`]. The verdict is a pure function of the
/// current graph; the optimizer clears the memo after every mutating pass.
pub(crate) fn frame_address_escapes_cached(function: &Function) -> bool {
    if let Some(escapes) = function.frame_escape() {
        return escapes;
    }
    let escapes = frame_address_escapes(function);
    function.set_frame_escape(escapes);
    escapes
}

fn frame_address_escapes(function: &Function) -> bool {
    function.frame_address_in_call_register() || any_exposed(function, |_| true)
}

/// Whether a callee may hold a pointer into the incoming arguments' memory,
/// register homes included: a stack address that may point at or above the
/// convention's incoming-argument start past the entry SP escapes.  See the
/// module doc.
pub(crate) fn incoming_area_escapes(function: &Function) -> bool {
    let Some(area_start) = function.default_cc().incoming_args_start() else {
        return false;
    };
    if function
        .call_register_frame_reach()
        .is_some_and(|reach| reach >= area_start)
    {
        return true;
    }
    any_exposed(function, |v| {
        !decompose(function, v)
            .is_some_and(|e| function.is_entry_sp(e.base) && e.offset < area_start)
    })
}

/// Whether some member of `U` is consumed by a use that exposes it: one that
/// hands it over (a call argument, a stored value, a return) when `counts`
/// holds for it, any other always.
fn any_exposed(function: &Function, mut counts: impl FnMut(ValueId) -> bool) -> bool {
    for node in function.walk() {
        let kind = *function.node_kind(node);
        // The structural SP anchor every callee has, not an argument.
        let sp_anchor_slot =
            matches!(kind, NodeKind::Call { .. }).then_some(NodeKind::CALL_SP_SLOT);
        for (idx, v) in function.node_inputs(node).into_iter().enumerate() {
            if sp_anchor_slot == Some(idx) {
                continue;
            }
            // Skip control / memory / phi-token edges.
            if function.value_type_opt(v).is_none() {
                continue;
            }
            if !is_frame_addr(function, v) {
                continue;
            }
            if !use_is_address_only(function, node, kind, idx)
                && (!hands_over(kind, idx) || counts(v))
            {
                return true;
            }
        }
    }
    false
}

/// Whether input `idx` of a `kind` node passes the value itself on: a call's
/// argument, a store's data, a returned value.
fn hands_over(kind: NodeKind, idx: usize) -> bool {
    match kind {
        NodeKind::Call { .. } => idx > NodeKind::CALL_SP_SLOT,
        NodeKind::CallOther { .. } | NodeKind::Return => idx > NodeKind::CTRL_MEM_SLOT,
        NodeKind::Store(_) => idx == NodeKind::STORE_DATA_SLOT,
        _ => false,
    }
}

/// A member of `U` is benign here only as the address operand
/// (`NodeKind::ACCESS_ADDR_SLOT`) of a `Load`/`Store`, or as an input to an
/// `Add`/`And` whose own output is again in `U` (a chain extension).
///
/// The `Load`/`Store` test is by slot index, not value equality: a self-store
/// `Store(V, V)` puts the same `ValueId` in both the address and data slots, so
/// comparing against the address operand would mask the escaping data operand.
fn use_is_address_only(function: &Function, node: NodeId, kind: NodeKind, idx: usize) -> bool {
    match kind {
        NodeKind::Load(_) | NodeKind::Store(_) => idx == NodeKind::ACCESS_ADDR_SLOT,
        NodeKind::IntBinaryOp(IntBinaryOp::Add | IntBinaryOp::And) => function
            .single_value_output(node)
            .ok()
            .is_some_and(|(out, _)| is_frame_addr(function, out)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::frame_address_escapes;
    use strider_ir::node::{ValueId, ValueType};
    use strider_ir::{Function, FunctionBuilder, IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::{sp_arg_frame, stack_vn_x86 as sp};

    use super::super::collapse_phis;

    type Body = fn(&mut FunctionBuilder, ValueId) -> crate::Result<()>;

    /// `body` over a one-region frame, handed `&local` (`sp - 4`) and building
    /// its own `Return`, then collapsed.
    fn frame_fn(body: Body) -> crate::Result<Function> {
        let mut b = sp_arg_frame(sp()).build_fn_single_region()?;
        let sp_v = b.read_variable(&sp())?;
        let local = crate::test_support::at(&mut b, sp_v, -4, ValueType::int(32))?;
        body(&mut b, local)?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        Ok(fg)
    }

    /// `store [local] = 0x42; return load [local]`.
    fn clean_spill(b: &mut FunctionBuilder, local: ValueId) -> crate::Result<()> {
        let data = b.build_int_const(0x42u64, ValueType::int(32))?;
        b.build_store(local, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(local, rsleigh::VnSpace::RAM, ValueType::int(32))?;
        b.build_return(Some(loaded), &[])
    }

    /// Whether `&local` escapes each row's body.
    #[test]
    fn a_frame_address_escapes_only_as_a_value() -> crate::Result<()> {
        let rows: &[(&str, Body, bool)] = &[
            ("used only as a Load/Store address", clean_spill, false),
            (
                // A Call's structural SP anchor (input slot 3) is not an
                // argument, so a spill straddling a call is still private.
                "a spill across a call",
                |b, local| {
                    let data = b.build_int_const(0x42u64, ValueType::int(32))?;
                    b.build_store(local, data, rsleigh::VnSpace::RAM)?;
                    let target = b.build_int_const(0x1000u64, ValueType::int(32))?;
                    b.build_call(target, &[], &[], 0)?;
                    let loaded = b.build_load(local, rsleigh::VnSpace::RAM, ValueType::int(32))?;
                    b.build_return(Some(loaded), &[])
                },
                false,
            ),
            (
                "a store's data operand (`*global = &local`)",
                |b, local| {
                    let global = b.build_int_const(0x4000u64, ValueType::int(32))?;
                    b.build_store(global, local, rsleigh::VnSpace::RAM)?;
                    b.build_return(None, &[])
                },
                true,
            ),
            (
                // The value can be loaded back opaquely and handed to a callee.
                "Store(V, V), the data operand equal to the address",
                |b, local| {
                    b.build_store(local, local, rsleigh::VnSpace::RAM)?;
                    b.build_return(None, &[])
                },
                true,
            ),
            (
                "a call argument",
                |b, local| {
                    let target = b.build_int_const(0x1000u64, ValueType::int(32))?;
                    b.build_call(target, &[local], &[], 0)?;
                    b.build_return(None, &[])
                },
                true,
            ),
            (
                // `CallOther` has no structural SP anchor (its inputs are
                // [ctrl, mem, ..args]), so every arg is a real argument.
                "a CallOther argument",
                |b, local| {
                    b.build_call_other(0, &[local], &[], true, false)?;
                    b.build_return(None, &[])
                },
                true,
            ),
            (
                "the returned value",
                |b, local| b.build_return(Some(local), &[]),
                true,
            ),
            (
                // `xor(&local, mask)` leaves the SP chain before the store.
                "non-address arithmetic",
                |b, local| {
                    let laundered =
                        b.build_const_binop(0xFFu64, local, IntBinaryOp::Xor, ValueType::int(32))?;
                    let global = b.build_int_const(0x4000u64, ValueType::int(32))?;
                    b.build_store(global, laundered, rsleigh::VnSpace::RAM)?;
                    b.build_return(None, &[])
                },
                true,
            ),
        ];
        for &(why, body, escapes) in rows {
            assert_eq!(frame_address_escapes(&frame_fn(body)?), escapes, "{why}");
        }
        Ok(())
    }

    /// A register the call clobbers still holds `&local` at the call, which
    /// the call node does not carry.
    #[test]
    fn frame_address_in_a_clobbered_register_escapes() -> crate::Result<()> {
        let sp = sp();
        let eax = strider_ir_test_utils::reg_vn(0x0, 4);
        let mut b = sp_arg_frame(sp).tracked(eax).build_fn_single_region()?;
        let sp_v = b.read_variable(&sp)?;
        let frame_addr = crate::test_support::at(&mut b, sp_v, -4, ValueType::int(32))?;
        b.write_variable(&eax, frame_addr)?;
        let target = b.build_int_const(0x1000u64, ValueType::int(32))?;
        b.build_call(target, &[], &[eax], 0)?;
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        assert!(frame_address_escapes(&fg));
        Ok(())
    }

    #[test]
    fn cached_warms_is_consulted_and_clears() -> crate::Result<()> {
        let fg = frame_fn(clean_spill)?;

        assert_eq!(fg.frame_escape(), None, "cold");
        assert!(!super::frame_address_escapes_cached(&fg));
        assert_eq!(fg.frame_escape(), Some(false), "warmed");

        // Poisoning proves the cached bit is consulted, not recomputed.
        fg.set_frame_escape(true);
        assert!(
            super::frame_address_escapes_cached(&fg),
            "returns the cached bit"
        );

        // Clearing forces a fresh, correct recompute.
        fg.clear_frame_escape();
        assert!(!super::frame_address_escapes_cached(&fg));
        Ok(())
    }
}
