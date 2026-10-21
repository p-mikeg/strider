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
//! [`incoming_area_escapes`] asks the narrower question of whether the
//! incoming-argument memory may be reached.  It places an exposure only where
//! the use hands over the address itself (a call argument, a stored value, a
//! return) and counts it unless it is the entry SP plus a constant below the
//! area, which names a frame object of its own; pointer arithmetic leaving the
//! object to a callee is outside the C object model.  Every other exposing use
//! (arithmetic leaving `U`, a `Phi`, a width change) counts whatever its
//! input's offset, as above: its result is a pointer `decompose` cannot place.
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
    if let Some(escapes) = function.side_tables().frame_escape() {
        return escapes;
    }
    let escapes = frame_address_escapes(function);
    function.side_tables().set_frame_escape(escapes);
    escapes
}

fn frame_address_escapes(function: &Function) -> bool {
    function.side_tables().frame_address_in_call_register() || any_exposed(function, |_| true)
}

/// Whether a stack address that may point at or above `area_start` bytes past
/// the entry SP (the incoming-argument memory) escapes; see the module doc.
pub(crate) fn incoming_area_escapes(function: &Function, area_start: i128) -> bool {
    if function
        .side_tables()
        .call_register_frame_reach()
        .is_some_and(|reach| reach >= area_start)
    {
        return true;
    }
    let entry_sp = function
        .initial_sp()
        .and_then(|node| function.node_outputs(node).first().copied());
    any_exposed(function, |v| {
        !decompose(function, v).is_some_and(|e| Some(e.base) == entry_sp && e.offset < area_start)
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
    use strider_ir::{IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::{sp_arg_frame, stack_vn_x86 as sp};

    use super::super::collapse_phis;

    /// `Add(sp_v, off)` as an `I32` frame address.
    fn frame_off(
        b: &mut strider_ir::FunctionBuilder,
        sp_v: ValueId,
        off: i64,
    ) -> crate::Result<ValueId> {
        let k = b.build_int_const(off as u64, ValueType::int(32))?;
        b.build_int_binary_operation(sp_v, k, IntBinaryOp::Add, ValueType::int(32))
    }

    fn new_builder() -> crate::Result<strider_ir::FunctionBuilder> {
        let sp = sp();
        sp_arg_frame(sp).build_fn_single_region()
    }

    #[test]
    fn clean_spill_is_private() -> crate::Result<()> {
        let mut b = new_builder()?;
        let sp_v = b.read_variable(&sp())?;
        let addr = frame_off(&mut b, sp_v, -4)?;
        let data = b.build_int_const(0x42u64, ValueType::int(32))?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(32))?;
        b.build_return(Some(loaded), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        assert!(
            !frame_address_escapes(&fg),
            "a spill address used only as a Load/Store address does not escape"
        );
        Ok(())
    }

    #[test]
    fn spill_across_call_stays_private() -> crate::Result<()> {
        // A Call's structural SP anchor (input slot 3) is not an argument, so a
        // spill straddling a call is still private.
        let mut b = new_builder()?;
        let sp_v = b.read_variable(&sp())?;
        let addr = frame_off(&mut b, sp_v, -4)?;
        let data = b.build_int_const(0x42u64, ValueType::int(32))?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        let target = b.build_int_const(0x1000u64, ValueType::int(32))?;
        b.build_call(target, &[], &[], 0)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(32))?;
        b.build_return(Some(loaded), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        assert!(
            !frame_address_escapes(&fg),
            "the Call's SP anchor must not count as an escape"
        );
        Ok(())
    }

    #[test]
    fn store_of_frame_address_escapes() -> crate::Result<()> {
        // `*global = &local`: the frame address is the store's DATA operand.
        let mut b = new_builder()?;
        let sp_v = b.read_variable(&sp())?;
        let frame_addr = frame_off(&mut b, sp_v, -4)?;
        let global = b.build_int_const(0x4000u64, ValueType::int(32))?;
        b.build_store(global, frame_addr, rsleigh::VnSpace::RAM)?;
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        assert!(
            frame_address_escapes(&fg),
            "a frame address in a store's data operand escapes"
        );
        Ok(())
    }

    #[test]
    fn frame_address_as_call_arg_escapes() -> crate::Result<()> {
        let mut b = new_builder()?;
        let sp_v = b.read_variable(&sp())?;
        let frame_addr = frame_off(&mut b, sp_v, -4)?;
        let target = b.build_int_const(0x1000u64, ValueType::int(32))?;
        b.build_call(target, &[frame_addr], &[], 0)?;
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        assert!(
            frame_address_escapes(&fg),
            "a frame address passed as a call argument escapes"
        );
        Ok(())
    }

    /// A register the call clobbers still holds `&local` at the call, which
    /// the call node does not carry.
    #[test]
    fn frame_address_in_a_clobbered_register_escapes() -> crate::Result<()> {
        let sp = sp();
        let eax = strider_ir_test_utils::reg_vn(0x0, 4);
        let mut b = strider_ir_test_utils::RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .stack_vn(sp)
            .tracked(eax)
            .build_fn_single_region()?;
        let sp_v = b.read_variable(&sp)?;
        let frame_addr = frame_off(&mut b, sp_v, -4)?;
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
    fn frame_address_as_call_other_arg_escapes() -> crate::Result<()> {
        // `CallOther` has no structural SP anchor (its inputs are [ctrl, mem,
        // ..args]), so every arg is a real argument.  A frame address there
        // exposes the frame just as a `Call` arg does.
        let mut b = new_builder()?;
        let sp_v = b.read_variable(&sp())?;
        let frame_addr = frame_off(&mut b, sp_v, -4)?;
        b.build_call_other(0, &[frame_addr], &[], true, false)?;
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        assert!(
            frame_address_escapes(&fg),
            "a frame address passed as a CallOther argument escapes"
        );
        Ok(())
    }

    #[test]
    fn returning_frame_address_escapes() -> crate::Result<()> {
        let mut b = new_builder()?;
        let sp_v = b.read_variable(&sp())?;
        let frame_addr = frame_off(&mut b, sp_v, -4)?;
        b.build_return(Some(frame_addr), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        assert!(
            frame_address_escapes(&fg),
            "returning a frame address escapes"
        );
        Ok(())
    }

    #[test]
    fn cached_warms_is_consulted_and_clears() -> crate::Result<()> {
        // Clean spill: escapes = false.
        let mut b = new_builder()?;
        let sp_v = b.read_variable(&sp())?;
        let addr = frame_off(&mut b, sp_v, -4)?;
        let data = b.build_int_const(0x42u64, ValueType::int(32))?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(32))?;
        b.build_return(Some(loaded), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);

        assert_eq!(fg.side_tables().frame_escape(), None, "cold");
        assert!(!super::frame_address_escapes_cached(&fg));
        assert_eq!(fg.side_tables().frame_escape(), Some(false), "warmed");

        // Poisoning proves the cached bit is consulted, not recomputed.
        fg.side_tables().set_frame_escape(true);
        assert!(
            super::frame_address_escapes_cached(&fg),
            "returns the cached bit"
        );

        // Clearing forces a fresh, correct recompute.
        fg.side_tables().clear_frame_escape();
        assert!(!super::frame_address_escapes_cached(&fg));
        Ok(())
    }

    #[test]
    fn self_store_of_frame_address_escapes() -> crate::Result<()> {
        // `Store(V, V)` with `V` a frame address: the data operand is a frame
        // address even though it equals the address operand. It must flag; the
        // value can be loaded back opaquely and handed to a callee.
        let mut b = new_builder()?;
        let sp_v = b.read_variable(&sp())?;
        let frame_addr = frame_off(&mut b, sp_v, -4)?;
        b.build_store(frame_addr, frame_addr, rsleigh::VnSpace::RAM)?;
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        assert!(
            frame_address_escapes(&fg),
            "Store(V, V) exposes V through the data operand"
        );
        Ok(())
    }

    #[test]
    fn laundered_frame_address_escapes() -> crate::Result<()> {
        // `xor(&local, mask)` leaves the SP chain at the xor, before the store.
        let mut b = new_builder()?;
        let sp_v = b.read_variable(&sp())?;
        let frame_addr = frame_off(&mut b, sp_v, -4)?;
        let mask = b.build_int_const(0xFFu64, ValueType::int(32))?;
        let laundered =
            b.build_int_binary_operation(frame_addr, mask, IntBinaryOp::Xor, ValueType::int(32))?;
        let global = b.build_int_const(0x4000u64, ValueType::int(32))?;
        b.build_store(global, laundered, rsleigh::VnSpace::RAM)?;
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        assert!(
            frame_address_escapes(&fg),
            "a frame address feeding non-address arithmetic escapes"
        );
        Ok(())
    }
}
