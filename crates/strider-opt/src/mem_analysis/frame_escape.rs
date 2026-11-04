//! Whole-function frame-escape analysis: does any stack address reach a
//! callee?  When none does, every spill slot is private, so a `Load` may
//! forward across a `Call`.
//!
//! `U` is the set of values [`decompose()`] reduces to `sp + k`.  The frame
//! escapes iff some member of `U` is used other than as a `Load` / `Store`
//! address, an input to an `Add` / `And` whose output is again in `U`, or a
//! `Call`'s SP anchor.  An address reaches callee-readable memory only through
//! such a use, so detection is at that boundary.  A stack-derived value held
//! in a register the convention clobbers at a call (i386 `regparm`, a static
//! chain, AArch64 X8) escapes too; the builder records it.
//!
//! `decompose` under-approximates `U` (`sp + (5 ^ 3)` before folding), which
//! is sound: the `Add` producing the unrecognised value is itself an exposing
//! use.  One leak poisons every call, since a leaked pointer can be stored to
//! a global and reloaded by a later callee.  A stored entry SP (PowerPC back
//! chain, ARM APCS frame record) is such a leak.
//!
//! [`incoming_area_escapes`] asks whether the incoming-argument memory may be
//! reached.  A use that hands the address over (call argument, stored value,
//! return) counts unless it is the entry SP plus a constant below the area;
//! every other exposing use counts whatever its offset.  A base below the
//! area that is spilled, reloaded and offset into it is missed.
//!
//! Sound modulo two claims: a callee writes the caller's frame outside the
//! argument area only through a pointer it was handed, and no pointer is
//! fabricated equal to a private slot.

use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{Function, IRViewer, IRWalker, IntBinaryOp};

use crate::mem_analysis::{MemKind, decompose};

/// Whether `v` is in `U`.
fn is_frame_addr(function: &Function, v: ValueId) -> bool {
    decompose(function, v).is_some_and(|e| e.kind == MemKind::Stack)
}

/// Memoised [`frame_address_escapes`].
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
/// register homes included.
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
        let sp_anchor_slot =
            matches!(kind, NodeKind::Call { .. }).then_some(NodeKind::CALL_SP_SLOT);
        for (idx, v) in function.node_inputs(node).into_iter().enumerate() {
            if sp_anchor_slot == Some(idx) {
                continue;
            }
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

/// Whether input `idx` keeps a member of `U` in `U`.  By slot, not value:
/// `Store(V, V)` exposes `V` as its data.
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
