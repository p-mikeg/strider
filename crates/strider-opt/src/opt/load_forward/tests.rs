use super::*;
use crate::AssumptionOptions;
use crate::PhiCollapse;
use crate::Result;
use crate::test_support::{COLLAPSE, STANDARD, at, run_pass, run_passes};
use strider_ir::node::{NodeKind, ValueType};
use strider_ir::{Function, FunctionBuilder, IRBuilderExt, IRViewer, IntBinaryOp};
use strider_ir_test_utils::IrBuilderEx;
use strider_ir_test_utils::IrWalkerEx;
use strider_ir_test_utils::{
    SENTINEL_LIFT_ADDR, make_sp_fn, sp_frame, stack_args_at, stack_vn_aarch64 as sp64_vn,
    stack_vn_x86 as sp32_vn,
};
use strider_target::Endianness;

type Body = fn(&mut FunctionBuilder, ValueId) -> Result<ValueId>;

/// `body` over a single-region frame of `sp`, returning the value it loads,
/// run through [`STANDARD`] under `claims`.
fn forwarded(
    sp: rsleigh::Vn,
    claims: AssumptionOptions,
    body: impl FnOnce(&mut FunctionBuilder, ValueId) -> Result<ValueId>,
) -> Result<Function> {
    let mut fg = make_sp_fn(sp, |b, sp_val| {
        let loaded = body(b, sp_val)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;
    fg.assume(&claims);
    run_passes(&mut fg, STANDARD)?;
    Ok(fg)
}

fn loads(fg: &Function) -> usize {
    fg.count_kind(|k| matches!(k, NodeKind::Load(_)))
}

/// The constant `fg` returns, or `None` while it still returns a `Load`.
fn outcome(fg: &Function) -> Option<u128> {
    let ret = fg.returned();
    if matches!(fg.node_kind(fg.producer(ret)), NodeKind::Load(_)) {
        return None;
    }
    Some(
        fg.int_const_u128(ret)
            .expect("a forward folds to a constant"),
    )
}

/// The producer of the surviving load's memory edge.
fn narrowed_onto(fg: &Function) -> NodeKind {
    let load = fg.first_kind(|k| matches!(k, NodeKind::Load(_)));
    *fg.node_kind(fg.producer(fg.node_inputs(load)[0]))
}

fn claiming(stack_global_disjoint: bool, escape_analysis: bool) -> AssumptionOptions {
    AssumptionOptions {
        stack_global_disjoint,
        escape_analysis,
        ..AssumptionOptions::none()
    }
}

fn i32_slot(b: &mut FunctionBuilder, sp: ValueId, off: i64) -> Result<ValueId> {
    at(b, sp, off, ValueType::int(32))
}

fn store32(b: &mut FunctionBuilder, addr: ValueId, v: u64) -> Result<()> {
    let data = b.build_int_const(v, ValueType::int(32))?;
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    Ok(())
}

fn load32(b: &mut FunctionBuilder, addr: ValueId) -> Result<ValueId> {
    b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(32))
}

/// Pins the memory-SSA walk as iterative: a recursive walk would overflow
/// the default 8 MB stack at this chain depth.
#[test]
fn forward_through_long_chain_of_disjoint_stack_stores() -> Result<()> {
    const CHAIN_LEN: i64 = 10_000;
    let fg = forwarded(sp32_vn(), AssumptionOptions::none(), |b, sp| {
        store32(b, sp, 0x99)?;
        // Disjoint stores the walk must step past to reach the target.
        for i in 1..=CHAIN_LEN {
            let addr = i32_slot(b, sp, i * 4 + 8)?;
            store32(b, addr, i as u64)?;
        }
        load32(b, sp)
    })?;
    assert_eq!(
        outcome(&fg),
        Some(0x99),
        "Load at sp+0 must forward past all {CHAIN_LEN} disjoint stack stores"
    );
    Ok(())
}

/// A load with nothing to forward still gets its memory edge narrowed past
/// the proven-disjoint stores onto `InitialMemory`.
#[test]
fn non_forwardable_load_is_narrowed_to_initial_memory() -> Result<()> {
    let fg = forwarded(sp32_vn(), AssumptionOptions::none(), |b, sp| {
        for off in [8, 16] {
            let addr = i32_slot(b, sp, off)?;
            store32(b, addr, 0xA0 + off as u64)?;
        }
        load32(b, sp)
    })?;
    assert_eq!(narrowed_onto(&fg), NodeKind::InitialMemory);
    Ok(())
}

/// Same address, different `VnSpace`: distinct spaces never alias, so the RAM
/// load must not take the REGISTER store's value, and narrows past it.
#[test]
fn forward_does_not_cross_address_spaces() -> Result<()> {
    let fg = forwarded(sp32_vn(), AssumptionOptions::none(), |b, sp| {
        let addr = i32_slot(b, sp, 8)?;
        let v = b.build_int_const(0x11u64, ValueType::int(32))?;
        b.build_store(addr, v, rsleigh::VnSpace::REGISTER)?;
        load32(b, addr)
    })?;
    assert_eq!(narrowed_onto(&fg), NodeKind::InitialMemory);
    Ok(())
}

/// One store / load shape over one frame per row, under `stack_global_disjoint`
/// when the row claims it: `Some(v)` forwards `v`, `None` keeps the load.
#[test]
fn straight_line_forwarding() -> Result<()> {
    let rows: &[(&str, rsleigh::Vn, bool, Body, Option<u128>)] = &[
        (
            "a matching store forwards",
            sp32_vn(),
            false,
            |b, sp| {
                let addr = i32_slot(b, sp, 4)?;
                store32(b, addr, 0x11)?;
                load32(b, addr)
            },
            Some(0x11),
        ),
        (
            "the nearest of two same-offset stores forwards",
            sp32_vn(),
            false,
            |b, sp| {
                let addr = i32_slot(b, sp, 8)?;
                store32(b, addr, 0x11)?;
                store32(b, addr, 0x22)?;
                load32(b, addr)
            },
            Some(0x22),
        ),
        (
            "a non-aliasing store is stepped past",
            sp32_vn(),
            false,
            |b, sp| {
                let (addr4, addr12) = (i32_slot(b, sp, 4)?, i32_slot(b, sp, 12)?);
                store32(b, addr4, 0xBB)?;
                store32(b, addr12, 0xAA)?;
                load32(b, addr4)
            },
            Some(0xBB),
        ),
        (
            // The aligned base differs from the initial SP by `sp mod 16`,
            // which is caller-dependent, so these are different memory.
            "sp + 8 does not back (sp & -16) + 8",
            sp32_vn(),
            false,
            |b, sp| {
                let addr = i32_slot(b, sp, 8)?;
                store32(b, addr, 0x11)?;
                let aligned =
                    b.build_const_binop(0xFFFF_FFF0u64, sp, IntBinaryOp::And, ValueType::int(32))?;
                let load_addr = i32_slot(b, aligned, 8)?;
                load32(b, load_addr)
            },
            None,
        ),
        (
            "a store over [0, 8) does not back a load of [4, 8)",
            sp64_vn(),
            false,
            |b, sp| {
                let addr4 = at(b, sp, 4, ValueType::int(64))?;
                let wide = b.build_int_const(0xDEAD_BEEF_CAFE_BABEu64, ValueType::int(64))?;
                b.build_store(sp, wide, rsleigh::VnSpace::RAM)?;
                load32(b, addr4)
            },
            None,
        ),
        (
            "a store of [2, 6) does not back a load of [0, 8)",
            sp64_vn(),
            false,
            |b, sp| {
                let addr2 = at(b, sp, 2, ValueType::int(64))?;
                store32(b, addr2, 0x1122_3344)?;
                b.build_load(sp, rsleigh::VnSpace::RAM, ValueType::int(64))
            },
            None,
        ),
        (
            "a narrower same-address store between a wide store and load blocks both",
            sp64_vn(),
            false,
            |b, sp| {
                let wide = b.build_int_const(0xAAAA_BBBB_CCCC_DDDDu64, ValueType::int(64))?;
                b.build_store(sp, wide, rsleigh::VnSpace::RAM)?;
                store32(b, sp, 0x1234)?;
                b.build_load(sp, rsleigh::VnSpace::RAM, ValueType::int(64))
            },
            None,
        ),
        (
            "a narrower store at the load's offset does not back it",
            sp64_vn(),
            false,
            |b, sp| {
                store32(b, sp, 0x11)?;
                b.build_load(sp, rsleigh::VnSpace::RAM, ValueType::int(64))
            },
            None,
        ),
        (
            // Its constant address could equal `sp + K`, or be an escaped
            // SP-derived pointer.
            "at the floor a const-address store may alias a stack slot",
            sp32_vn(),
            false,
            const_store_between,
            None,
        ),
        (
            "under stack_global_disjoint a const-address store is stepped past",
            sp32_vn(),
            true,
            const_store_between,
            Some(0xAA),
        ),
        (
            // Proven disjoint on the ring, not by `stack_global_disjoint`.
            "a const-address load forwards past a disjoint const-address store",
            sp32_vn(),
            false,
            |b, _| {
                let (addr1, addr2) = (
                    b.build_int_const(0x1000u64, ValueType::int(32))?,
                    b.build_int_const(0x2000u64, ValueType::int(32))?,
                );
                store32(b, addr1, 0xAA)?;
                store32(b, addr2, 0xBB)?;
                load32(b, addr1)
            },
            Some(0xAA),
        ),
        (
            // Matched by `ValueId` equality on the address slot.
            "an Anchor load forwards from the store through the same value",
            sp32_vn(),
            false,
            |b, _| {
                let table_base = b.build_int_const(0x100u64, ValueType::int(32))?;
                let p = load32(b, table_base)?;
                store32(b, p, 0xCC)?;
                load32(b, p)
            },
            Some(0xCC),
        ),
        (
            // `q != p` is unprovable at runtime.
            "a store through a different Anchor blocks",
            sp32_vn(),
            false,
            |b, _| {
                let base1 = b.build_int_const(0x100u64, ValueType::int(32))?;
                let base2 = b.build_int_const(0x200u64, ValueType::int(32))?;
                let (p, q) = (load32(b, base1)?, load32(b, base2)?);
                store32(b, p, 0xCC)?;
                store32(b, q, 0xDD)?;
                load32(b, p)
            },
            None,
        ),
        (
            "a Call between blocks",
            sp64_vn(),
            false,
            |b, sp| {
                let addr4 = at(b, sp, 4, ValueType::int(64))?;
                store32(b, addr4, 0x11)?;
                let target = b.build_int_const(0x1000u64, ValueType::int(64))?;
                b.build_call_cc(target, None)?;
                load32(b, addr4)
            },
            None,
        ),
        (
            // Both encodings normalise to offset -4 once `ConstantFold` has
            // folded `Add(_, Neg(K))`; LoadForward does not peel the `Neg`.
            "Sub(sp, 4) and Add(sp, 0xFFFFFFFC) name one slot",
            sp32_vn(),
            false,
            |b, sp| {
                let four = b.build_int_const(4u64, ValueType::int(32))?;
                let store_addr = b.build_sub_as_add_neg(sp, four, ValueType::int(32))?;
                store32(b, store_addr, 0x4242)?;
                let load_addr = i32_slot(b, sp, -4)?;
                load32(b, load_addr)
            },
            Some(0x4242),
        ),
        (
            // The `-O0 -m32` spill read back one byte: forwarded through a
            // `Truncate`, so the I8 result is the low byte.
            "a narrower load takes the low byte of a wider store",
            sp32_vn(),
            false,
            |b, sp| {
                let eight = b.build_int_const(8u64, ValueType::int(32))?;
                let addr = b.build_sub_as_add_neg(sp, eight, ValueType::int(32))?;
                store32(b, addr, 0xDEAD_BEEF)?;
                b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(8))
            },
            Some(0xEF),
        ),
    ];
    for &(why, sp, stack_global_disjoint, body, want) in rows {
        let fg = forwarded(sp, claiming(stack_global_disjoint, false), body)?;
        assert_eq!(outcome(&fg), want, "{why}");
    }
    Ok(())
}

/// `[sp+4] = 0xAA; [0x1000] = 0xBB; return [sp+4]`.
fn const_store_between(b: &mut FunctionBuilder, sp: ValueId) -> Result<ValueId> {
    let addr4 = i32_slot(b, sp, 4)?;
    store32(b, addr4, 0xAA)?;
    let global = b.build_int_const(0x1000u64, ValueType::int(32))?;
    store32(b, global, 0xBB)?;
    load32(b, addr4)
}

/// A store through an opaque (`Anchor`) pointer sits between a stack spill and
/// its reload.  Over a private frame with the knob on, the pointer cannot name
/// the frame, so the reload forwards past it; without the knob, when a frame
/// address escapes, or without `stack_global_disjoint`, it is a may-alias
/// barrier.
#[test]
fn an_opaque_store_blocks_a_stack_reload_unless_the_frame_is_private() -> Result<()> {
    for (disjoint, knob, escape, want) in [
        (true, true, false, Some(0x99)),
        (true, false, false, None),
        (true, true, true, None),
        // The pointer may name a global the stack overlaps.
        (false, true, false, None),
    ] {
        let fg = forwarded(sp32_vn(), claiming(disjoint, knob), |b, sp| {
            // spill: [sp-8] = 0x99, a local of this frame
            let slot = i32_slot(b, sp, -8)?;
            store32(b, slot, 0x99)?;
            let pconst = b.build_int_const(0x9000u64, ValueType::int(32))?;
            let ptr = load32(b, pconst)?;
            // When `escape`, the stored value is a frame address (`sp+8`).
            let data = if escape {
                i32_slot(b, sp, 8)?
            } else {
                b.build_int_const(0x11u64, ValueType::int(32))?
            };
            b.build_store(ptr, data, rsleigh::VnSpace::RAM)?;
            load32(b, slot)
        })?;
        assert_eq!(
            outcome(&fg),
            want,
            "disjoint {disjoint}, knob {knob}, escape {escape}"
        );
    }
    Ok(())
}

/// `(offset, value)` stores off SP.
type Stores<'a> = &'a [(i64, u64)];

/// An `If(true)` diamond over `sp` (which `STANDARD` leaves standing, its
/// `MemPhi` with it): the `entry` stores, each arm's, then a load of `ty` at
/// `sp + load`.  Every store is at the address width.
fn sp_diamond(
    sp: rsleigh::Vn,
    entry: Stores,
    arms: [Stores; 2],
    load: i64,
    ty: ValueType,
) -> Result<Function> {
    let width = ValueType::int(u16::try_from(sp.size * 8)?);
    let mut b = sp_frame(sp).build_fn()?;
    let [entry_r, then_r, else_r, merge] = [
        b.create_region_all()?,
        b.create_region_all()?,
        b.create_region_all()?,
        b.create_region_all()?,
    ];
    b.set_entry_region_all(entry_r)?;
    let store_all = |b: &mut FunctionBuilder, stores: Stores| -> Result<()> {
        for &(off, v) in stores {
            let sp_val = b.read_variable(&sp)?;
            let addr = at(b, sp_val, off, width)?;
            let data = b.build_int_const(v, width)?;
            b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        }
        Ok(())
    };

    b.set_region(entry_r);
    store_all(&mut b, entry)?;
    let cond = b.build_boolean_const(true);
    b.build_if(cond, then_r, else_r)?;
    for (region, stores) in [then_r, else_r].into_iter().zip(arms) {
        b.set_region(region);
        store_all(&mut b, stores)?;
        b.build_branch(merge)?;
    }
    b.set_region(merge);
    let sp_val = b.read_variable(&sp)?;
    let addr = at(&mut b, sp_val, load, width)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ty)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;
    run_passes(&mut fg, STANDARD)?;
    Ok(fg)
}

/// A `MemPhi` is transparent only where its arms agree: each `(entry, arms)`
/// row stores, then the merge loads `sp+load`.
#[test]
fn a_memphi_forwards_only_what_its_arms_agree_on() -> Result<()> {
    type Row<'a> = (
        &'a str,
        rsleigh::Vn,
        Stores<'a>,
        [Stores<'a>; 2],
        i64,
        ValueType,
        Option<u128>,
    );
    let (i32_ty, none): (_, Stores) = (ValueType::int(32), &[]);
    let rows: &[Row] = &[
        (
            "each arm stores its own value",
            sp32_vn(),
            none,
            [&[(4, 0xAA)], &[(4, 0xBB)]],
            4,
            i32_ty,
            None,
        ),
        (
            "one arm stores nothing",
            sp32_vn(),
            none,
            [&[(4, 0xAA)], none],
            4,
            i32_ty,
            None,
        ),
        (
            "one arm stores a wider value",
            sp64_vn(),
            none,
            [&[(0, 0xDEAD_BEEF_CAFE_BABE)], none],
            0,
            i32_ty,
            None,
        ),
        // Both arms carry one memory token, so `PhiCollapse` removes the
        // trivial `MemPhi` and the walk reaches the dominating store.
        (
            "a dominating store over empty arms",
            sp32_vn(),
            &[(4, 0xAB)],
            [none, none],
            4,
            i32_ty,
            Some(0xAB),
        ),
        // Distinct per-arm inputs keep the `MemPhi`, yet both arms bottom out
        // on the entry store.
        (
            "arms storing elsewhere reach one store",
            sp32_vn(),
            &[(4, 0xAA)],
            [&[(8, 0xBB)], &[(12, 0xCC)]],
            4,
            i32_ty,
            Some(0xAA),
        ),
    ];
    for &(why, sp, entry, arms, load, ty, want) in rows {
        let fg = sp_diamond(sp, entry, arms, load, ty)?;
        assert_eq!(outcome(&fg), want, "{why}");
        // The module's contract, that no merge of two stored values forwards
        // through a synthesized value-`Phi`; no guard backs it, since the pass
        // has no code that builds one.
        assert!(
            !fg.has_kind(|k| matches!(k, NodeKind::Phi)),
            "{why}: phi-free"
        );
    }
    Ok(())
}

/// Same contract as the 2-way diamond, but a nested `If` gives the merge
/// three predecessors: the 3-input `MemPhi` is still an opaque boundary.
#[test]
fn three_predecessor_memphi_blocks_forwarding() -> Result<()> {
    let sp = sp32_vn();
    let mut b = sp_frame(sp).build_fn()?;
    let entry = b.create_region_all()?;
    let arm_a = b.create_region_all()?;
    let inner = b.create_region_all()?;
    let arm_b = b.create_region_all()?;
    let arm_c = b.create_region_all()?;
    let merge = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    // if { arm_a } else { if { arm_b } else { arm_c } }, all joining at merge.
    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, arm_a, inner)?;
    b.set_region(inner);
    let cond2 = b.build_boolean_const(false);
    b.build_if(cond2, arm_b, arm_c)?;

    for (region, val) in [(arm_a, 0xAAu64), (arm_b, 0xBB), (arm_c, 0xCC)] {
        b.set_region(region);
        let sp_v = b.read_variable(&sp)?;
        let addr = b.build_const_binop(4u64, sp_v, IntBinaryOp::Add, ValueType::int(32))?;
        let c = b.build_int_const(val, ValueType::int(32))?;
        b.build_store(addr, c, rsleigh::VnSpace::RAM)?;
        b.build_branch(merge)?;
    }

    b.set_region(merge);
    let sp_m = b.read_variable(&sp)?;
    let addr_m = b.build_const_binop(4u64, sp_m, IntBinaryOp::Add, ValueType::int(32))?;
    let loaded = b.build_load(addr_m, rsleigh::VnSpace::RAM, ValueType::int(32))?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    run_passes(&mut fg, STANDARD)?;

    assert_eq!(
        loads(&fg),
        1,
        "a 3-predecessor MemPhi is an opaque boundary; the load must NOT forward",
    );
    Ok(())
}

/// On big-endian the load takes the high bytes, so forwarding must shift
/// before truncating rather than emit the LE plain `Truncate`.
///
/// The pipeline here is the collapse passes plus `LoadForward`, which leaves
/// the shift-and-truncate chain standing for the structural assertions;
/// `ConstantFold` would fold it to a single `IntConst`.
#[test]
fn narrow_load_from_wider_store_be_shifts_high_bytes() -> Result<()> {
    let sp = sp32_vn();
    let mut b = sp_frame(sp)
        .byte_order(Endianness::Big)
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    let eight = b.build_int_const(8u64, ValueType::int(32))?;
    let addr = b.build_sub_as_add_neg(sp_val, eight, ValueType::int(32))?;
    let wide = b.build_int_const(0xDEAD_BEEFu64, ValueType::int(32))?;
    b.build_store(addr, wide, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(8))?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    run_passes(
        &mut fg,
        &[
            &crate::PhiCollapse,
            &crate::RegionCollapse,
            &crate::LoadForward,
        ],
    )?;

    assert_eq!(
        loads(&fg),
        0,
        "Load u8 at matching offset must be forwarded as the high byte of the u32 store on BE",
    );

    let val_value = fg.returned();
    let val_ty = fg.value_type_opt(val_value);
    assert_eq!(val_ty, Some(ValueType::int(8)));

    let outer = fg.producer(val_value);
    assert!(
        matches!(fg.node_kind(outer), NodeKind::Truncate),
        "BE narrow forward must wrap data in a Truncate; got {:?}",
        fg.node_kind(outer),
    );

    let outer_inputs = fg.node_inputs(outer);
    assert_eq!(outer_inputs.len(), 1, "Truncate has a single input");
    let inner = fg.producer(outer_inputs[0]);
    assert!(
        matches!(
            fg.node_kind(inner),
            NodeKind::IntBinaryOp(IntBinaryOp::ShiftRight),
        ),
        "BE narrow forward must shift before truncation; got {:?}",
        fg.node_kind(inner),
    );

    // ShiftRight inputs: [data, shift_const]; here (4 - 1) * 8 = 24.
    let shr_inputs = fg.node_inputs(inner);
    assert_eq!(shr_inputs.len(), 2, "ShiftRight has two inputs");
    let shift_val = shr_inputs[1];
    assert!(
        fg.int_const_u128(shift_val) == Some(24),
        "BE shift amount must be (store_size - load_size) * 8 = 24; got {:?}",
        fg.int_const_u128(shift_val),
    );
    Ok(())
}

/// A spill to `sp-8`, a local of this frame, then `barrier` with SP lowered by
/// 64 so the slot sits above any call's SP, then the reload, run under
/// `claims`.
fn spill_across(
    claims: AssumptionOptions,
    barrier: impl FnOnce(&mut FunctionBuilder, ValueId) -> Result<()>,
) -> Result<Function> {
    let sp = sp32_vn();
    forwarded(sp, claims, |b, sp_val| {
        let slot = i32_slot(b, sp_val, -8)?;
        store32(b, slot, 0x42)?;
        let call_sp = i32_slot(b, sp_val, -64)?;
        b.write_variable(&sp, call_sp)?;
        barrier(b, sp_val)?;
        load32(b, slot)
    })
}

fn call(b: &mut FunctionBuilder) -> Result<()> {
    let target = b.build_int_const(0x1000u64, ValueType::int(32))?;
    b.build_call(target, &[], &[], 0)?;
    Ok(())
}

/// A spill straddling a barrier forwards only under the relaxation that
/// covers it.  A `Call` steps through when the frame is provably private and
/// the opt-in knob is set; an escaping frame address, or an opaque
/// `CallOther` (which may write the stack without ever receiving a frame
/// address) keeps blocking.  An `Anchor` store needs `escape_analysis` too:
/// `stack_global_disjoint` alone does not buy it.
#[test]
fn a_spill_forwards_across_a_barrier_only_under_its_relaxation() -> Result<()> {
    type Barrier = fn(&mut FunctionBuilder, ValueId) -> Result<()>;
    let rows: &[(&str, bool, bool, Barrier, Option<u128>)] = &[
        (
            "a Call over a private frame",
            false,
            true,
            |b, _| call(b),
            Some(0x42),
        ),
        (
            "a Call with the knob off",
            false,
            false,
            |b, _| call(b),
            None,
        ),
        (
            // `stack_global_disjoint` steps the reload past the leaking store
            // itself.
            "a Call once a frame address escapes",
            true,
            true,
            |b, sp| {
                let leaked = i32_slot(b, sp, 16)?;
                let global = b.build_int_const(0x4000u64, ValueType::int(32))?;
                b.build_store(global, leaked, rsleigh::VnSpace::RAM)?;
                call(b)
            },
            None,
        ),
        (
            // The reload forwards and places the call's window, but the spill
            // of a frame address escapes the frame before it can matter.
            "a Call whose SP is a reloaded frame address",
            true,
            true,
            |b, sp| {
                let spilled = i32_slot(b, sp, -16)?;
                let call_sp = i32_slot(b, sp, -64)?;
                b.build_store(spilled, call_sp, rsleigh::VnSpace::RAM)?;
                let reloaded = load32(b, spilled)?;
                b.write_variable(&sp32_vn(), reloaded)?;
                call(b)
            },
            None,
        ),
        (
            // advance_memory = true puts the CallOther on the memory chain.
            "a CallOther over a private frame",
            false,
            true,
            |b, _| {
                b.build_call_other(0, &[], &[], true, false)?;
                Ok(())
            },
            None,
        ),
        (
            "an Anchor store under stack_global_disjoint alone",
            true,
            false,
            |b, _| {
                let global_addr = b.build_int_const(0x2000u64, ValueType::int(32))?;
                let p = load32(b, global_addr)?;
                store32(b, p, 0xBB)
            },
            None,
        ),
    ];
    for &(why, stack_global_disjoint, escape_analysis, barrier, want) in rows {
        let fg = spill_across(claiming(stack_global_disjoint, escape_analysis), barrier)?;
        assert_eq!(outcome(&fg), want, "{why}");
    }
    Ok(())
}

/// A spill whose address a register the call's convention neither passes nor
/// preserves still holds at the call, the way i386 `regparm` hands a local's
/// address over in EAX.  `held` is what that register holds.
fn spill_across_call_with_register(held_is_frame_address: bool) -> Result<strider_ir::Function> {
    let sp = sp32_vn();
    let eax = strider_ir_test_utils::reg_vn(0x0, 4);
    let mut b = strider_ir_test_utils::RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .tracked(eax)
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    let slot = b.build_const_binop((-8i64) as u64, sp_val, IntBinaryOp::Add, ValueType::int(32))?;
    let eleven = b.build_int_const(11u64, ValueType::int(32))?;
    b.build_store(slot, eleven, rsleigh::VnSpace::RAM)?;
    let held = if held_is_frame_address {
        slot
    } else {
        b.build_int_const(0x1234u64, ValueType::int(32))?
    };
    b.write_variable(&eax, held)?;
    let call_sp = b.build_const_binop(
        (-64i64) as u64,
        sp_val,
        IntBinaryOp::Add,
        ValueType::int(32),
    )?;
    b.write_variable(&sp, call_sp)?;
    let target = b.build_int_const(0x1000u64, ValueType::int(32))?;
    let (_, clobbers) = b.build_call(target, &[], &[eax], 0)?;
    b.write_variable(&eax, clobbers[0])?;
    let loaded = b.build_load(slot, rsleigh::VnSpace::RAM, ValueType::int(32))?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;
    fg.assume(&claiming(false, true));
    run_passes(&mut fg, STANDARD)?;
    Ok(fg)
}

#[test]
fn spill_not_forwarded_across_call_when_a_clobbered_register_holds_its_address() -> Result<()> {
    let fg = spill_across_call_with_register(true)?;
    assert_eq!(
        fg.count_kind(|k| matches!(k, NodeKind::Load(_))),
        1,
        "the callee can write the spill through the register, so the reload stays"
    );
    Ok(())
}

#[test]
fn spill_forwarded_across_call_when_the_clobbered_register_holds_no_address() -> Result<()> {
    let fg = spill_across_call_with_register(false)?;
    let val = fg.returned();
    assert_eq!(
        fg.int_const_u128(val),
        Some(11),
        "a register holding no frame address leaves the frame private"
    );
    Ok(())
}

/// The knob relaxes only SP-rooted probes; a global load still blocks.
#[test]
fn global_load_not_forwarded_across_call() -> Result<()> {
    let fg = forwarded(sp32_vn(), claiming(false, true), |b, _| {
        let global = b.build_int_const(0x4000u64, ValueType::int(32))?;
        store32(b, global, 0x42)?;
        call(b)?;
        load32(b, global)
    })?;
    assert_eq!(
        outcome(&fg),
        None,
        "the call still blocks a non-stack probe"
    );
    Ok(())
}

/// A private frame does NOT make the callee's area private.  The callee owns
/// everything below `call_sp + base_offset`: its own frame below the call's
/// SP, plus the ABI-reserved area between the call's SP and the first
/// stack-argument slot (x86's return-address slot, PPC64's linkage area, MIPS
/// o32's 16 bytes, Windows x64's home space).  It is also handed the
/// outgoing-argument slots, which the caller writes and it may overwrite.  A
/// spill anywhere there must not forward across the call, whose SP here is
/// the entry SP.
#[test]
fn a_spill_in_the_callees_area_does_not_forward_across_a_call() -> Result<()> {
    for (why, base_offset, slot) in [
        ("stack-arg slot 0", 0, 0),
        ("below the call's SP", 8, -4),
        ("the ABI-reserved area", 8, 4),
    ] {
        let mut b = sp_frame(sp32_vn())
            .stack_args(stack_args_at(base_offset, 4))
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp32_vn())?;
        let addr = i32_slot(&mut b, sp_val, slot)?;
        store32(&mut b, addr, 0x42)?;
        call(&mut b)?;
        let reload = load32(&mut b, addr)?;
        b.build_return(Some(reload), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        fg.assume(&claiming(false, true));
        run_passes(&mut fg, STANDARD)?;
        assert_eq!(outcome(&fg), None, "{why} is the callee's");
    }
    Ok(())
}

/// [`crate::test_support::memory_shapes::stack_top_spill`] with the reload
/// stored: the `Load`s left after the standard pipeline.
fn stack_top_spill_across_call(preserves_stack_args: bool) -> Result<u128> {
    let (mut b, reload) = crate::test_support::memory_shapes::stack_top_spill()?;
    let field = b.build_int_const(0x9000u64, ValueType::int(32))?;
    b.build_store(field, reload, rsleigh::VnSpace::RAM)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    fg.assume(&crate::AssumptionOptions {
        escape_analysis: true,
        callee_preserves_stack_args: preserves_stack_args,
        ..crate::AssumptionOptions::none()
    });
    run_passes(&mut fg, STANDARD)?;

    Ok(fg.count_kind(|k| matches!(k, NodeKind::Load(_))) as u128)
}

#[test]
fn stack_top_spill_forwards_across_call_only_under_the_relaxation() -> Result<()> {
    assert_eq!(
        stack_top_spill_across_call(false)?,
        1,
        "with the knob off the spill sits in the callee's argument window, so \
         the reload survives"
    );
    assert_eq!(
        stack_top_spill_across_call(true)?,
        0,
        "with the knob on the window is empty, so the reload forwards to the \
         spilled 0x42"
    );
    Ok(())
}

/// Two reloads of one call's frame in the same sweep, on opposite sides of the
/// argument window.  The window is scanned once for the call, so it has to
/// answer each reload on that reload's own offset.
///
/// ```text
/// store A0 -> sp-16    ; slot 0
/// store A1 -> sp-12    ; slot 1
/// store S  -> sp-4     ; a spill, one empty slot above the arguments
/// call f               ; sp = sp-16
/// load  sp-12          ; f's own argument slot, pinned
/// load  sp-4           ; above the window, forwards
/// ```
#[test]
fn one_window_answers_reloads_on_either_side_of_it() -> Result<()> {
    let sp = sp32_vn();
    let mut b = sp_frame(sp)
        .stack_args(stack_args_at(0, 4))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    let mut addrs = Vec::new();
    for (off, data) in [(-16i64, 0x11u64), (-12, 0x22), (-4, 0x55)] {
        let addr = b.build_const_binop(off as u64, sp_val, IntBinaryOp::Add, ValueType::int(32))?;
        let value = b.build_int_const(data, ValueType::int(32))?;
        b.build_store(addr, value, rsleigh::VnSpace::RAM)?;
        addrs.push(addr);
    }
    let call_sp = b.build_const_binop(
        (-16i64) as u64,
        sp_val,
        IntBinaryOp::Add,
        ValueType::int(32),
    )?;
    b.write_variable(&sp, call_sp)?;
    let target = b.build_int_const(0x1000u64, ValueType::int(32))?;
    b.build_call(target, &[], &[], 0)?;
    let pinned = b.build_load(addrs[1], rsleigh::VnSpace::RAM, ValueType::int(32))?;
    let forwarded = b.build_load(addrs[2], rsleigh::VnSpace::RAM, ValueType::int(32))?;
    let sum =
        b.build_int_binary_operation(pinned, forwarded, IntBinaryOp::Add, ValueType::int(32))?;
    b.build_return(Some(sum), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    fg.assume(&claiming(false, true));
    run_passes(&mut fg, STANDARD)?;

    assert_eq!(
        fg.count_kind(|k| matches!(k, NodeKind::Load(_))),
        1,
        "sp-4 forwards to the spill, sp-12 is the callee's argument slot"
    );
    let ret = fg.returned();
    let operands = fg.node_inputs(fg.producer(ret));
    assert!(
        operands.iter().any(|v| fg.int_const_u128(v) == Some(0x55)),
        "the sp-4 reload must carry the spilled 0x55"
    );
    Ok(())
}

/// Two nested loops with no memory def in either body, whose exits merge above
/// the load: `merge[inner_exit, exit_merge[store, outer_header]]`, with the
/// inner header on `inner_exit`'s only arm.
///
/// Resolving the `exit_merge` arm first walks the outer header, which reaches
/// the inner one; resolving `merge`'s other arm then reads the inner header
/// again from outside the outer loop.  The store reaches the load on one arm
/// and nothing does on the other, so `merge` is the load's boundary.
fn nested_loop_exit_merge_graph() -> Result<(
    strider_ir::Function,
    strider_ir::node::ValueId,
    strider_ir::node::ValueId,
)> {
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region_all()?;
    let store_r = b.create_region_all()?;
    let outer_header = b.create_region_all()?;
    let inner_header = b.create_region_all()?;
    let inner_latch = b.create_region_all()?;
    let inner_exit = b.create_region_all()?;
    let outer_latch = b.create_region_all()?;
    let exit_merge = b.create_region_all()?;
    let merge = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    b.set_region(entry);
    let addr = b.build_int_const(0x10u64, ValueType::int(64))?;
    let cond = b.build_boolean_const(true);
    b.build_if(cond, store_r, outer_header)?;

    b.set_region(store_r);
    let data = b.build_int_const(0x42u64, ValueType::int(64))?;
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    b.build_branch(exit_merge)?;

    b.set_region(outer_header);
    b.build_if(cond, inner_header, exit_merge)?;

    b.set_region(inner_header);
    b.build_if(cond, inner_latch, inner_exit)?;

    b.set_region(inner_latch);
    b.build_branch(inner_header)?;

    // Linked before `exit_merge`, so it is `merge`'s first arm.
    b.set_region(inner_exit);
    b.build_if(cond, outer_latch, merge)?;

    b.set_region(outer_latch);
    b.build_branch(outer_header)?;

    b.set_region(exit_merge);
    b.build_branch(merge)?;

    b.set_region(merge);
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(64))?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let fg = b.build()?;

    let merge_mem = fg.node_inputs(fg.producer(loaded))[0];
    Ok((fg, merge_mem, loaded))
}

/// The load's memory edge must stay on the exit merge: the inner header is
/// reached on only one of its arms, so narrowing onto it drops the store the
/// other arm carries.
#[test]
fn load_below_nested_loops_is_not_narrowed_past_the_exit_merge() -> Result<()> {
    let (mut fg, merge, _loaded) = nested_loop_exit_merge_graph()?;

    run_pass(&crate::LoadForward, &mut fg)?;

    let load = fg.first_kind(|k| matches!(k, NodeKind::Load(_)));
    let mem = fg.node_inputs(load)[0];
    assert_eq!(
        mem,
        merge,
        "the load must keep the exit merge as its memory edge, got a {:?}",
        fg.node_kind(fg.producer(mem)),
    );
    Ok(())
}

/// The narrowed edge outlives the pass, so a later `PhiCollapse` reads it: no
/// sequence of the two may end with the store forwarded into the load.
#[test]
fn nested_loop_exit_merge_never_forwards_the_store_after_phi_collapse() -> Result<()> {
    let (mut fg, _merge, loaded) = nested_loop_exit_merge_graph()?;

    for _ in 0..3 {
        run_pass(&crate::LoadForward, &mut fg)?;
        run_pass(&PhiCollapse, &mut fg)?;
    }

    assert!(
        matches!(fg.node_kind(fg.producer(loaded)), NodeKind::Load(_)),
        "the load must not take the store's value across the merge, got {:?}",
        fg.node_kind(fg.producer(loaded)),
    );
    Ok(())
}

/// The narrowed edge is read afterwards by walks that honour no call
/// relaxation, so it may only name a clobber that holds without one: under
/// `escape_analysis` the edge still stops at the `Call`.  Under
/// `stack_global_disjoint`, a claim every reader of the narrowed edge honours,
/// it steps past a global store.
///
/// Mutation: narrow with `stack_global_disjoint` off.
#[test]
fn narrowing_honours_only_the_claims_every_reader_does() -> Result<()> {
    let sp = sp32_vn();
    let across_call = forwarded(sp, claiming(false, true), |b, sp_val| {
        let call_sp = i32_slot(b, sp_val, -32)?;
        b.write_variable(&sp, call_sp)?;
        call(b)?;
        // Nothing is stored to the slot, so the load can only narrow.
        let addr = i32_slot(b, sp_val, -8)?;
        load32(b, addr)
    })?;
    assert!(
        matches!(narrowed_onto(&across_call), NodeKind::Call { .. }),
        "the Call must stay on the load's memory chain"
    );
    let past_global = forwarded(sp, claiming(true, false), |b, sp_val| {
        let global = b.build_int_const(0x4000u64, ValueType::int(32))?;
        store32(b, global, 0x22)?;
        let addr = i32_slot(b, sp_val, -8)?;
        load32(b, addr)
    })?;
    assert_eq!(narrowed_onto(&past_global), NodeKind::InitialMemory);
    Ok(())
}

/// `noalias_allocators` gives the narrowing heap identity, so a stack load's
/// edge steps past a store through the allocation; but a listed callee steps
/// it past itself only for forwarding, and the narrowed edge, which the
/// jump-table walk reads with no call relaxation, stops at the call.
///
/// Mutations: `classify_addr` names no heap class; narrow under
/// `Policy::forward`.
#[test]
fn narrowing_past_a_heap_store_stops_at_its_allocator() -> Result<()> {
    const ALLOCATOR: u64 = 0x1000;
    let sp = sp32_vn();
    // A return register, without which the call has no fresh heap base and
    // `noalias_allocators` never reaches it.
    let ret_reg = rsleigh::Vn {
        addr_off: 0x00,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let mut b = strider_ir_test_utils::RegisterSet::new()
        .tracked(sp)
        .tracked(ret_reg)
        .callee_saved(sp)
        .ret(ret_reg)
        .stack_vn(sp)
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    let call_sp = b.build_const_binop(
        (-32i64) as u64,
        sp_val,
        IntBinaryOp::Add,
        ValueType::int(32),
    )?;
    b.write_variable(&sp, call_sp)?;
    let target = b.build_int_const(ALLOCATOR, ValueType::int(32))?;
    let (_, rets) = b.build_call(target, &[], &[ret_reg], 0)?;
    let value = b.build_int_const(0x22u64, ValueType::int(32))?;
    b.build_store(rets[0], value, rsleigh::VnSpace::RAM)?;
    // Nothing is stored to the slot, so the load can only narrow.
    let addr = b.build_const_binop((-8i64) as u64, sp_val, IntBinaryOp::Add, ValueType::int(32))?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(32))?;
    // Keeps the allocator's pointer live, so the call keeps the return output
    // `is_allocator_return` reads.
    let sum =
        b.build_int_binary_operation(loaded, rets[0], IntBinaryOp::Add, ValueType::int(32))?;
    b.build_return(Some(sum), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    fg.assume(&crate::AssumptionOptions {
        noalias_allocators: [ALLOCATOR].into_iter().collect(),
        ..crate::AssumptionOptions::none()
    });
    run_passes(&mut fg, STANDARD)?;

    assert!(
        matches!(narrowed_onto(&fg), NodeKind::Call { .. }),
        "the Call must stay on the load's memory chain"
    );
    Ok(())
}

/// Each arm spills to its own stack slot and reloads a third, so narrowing
/// moves both loads past their arm's store onto one memory value and leaves
/// them structural twins under a phi.  Reporting the narrowing as no change let
/// the fixed point exit before that merge, leaving `Phi[x, x]` behind.
#[test]
fn narrowing_that_makes_twin_loads_reruns_the_fixed_point() -> Result<()> {
    let sp = sp32_vn();
    let var = strider_ir_test_utils::reg_vn(0x1000, 4);
    let mut b = sp_frame(sp).tracked(var).arg(var).build_fn()?;
    let entry = b.create_region_all()?;
    let then_r = b.create_region_all()?;
    let else_r = b.create_region_all()?;
    let merge = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    b.set_region(entry);
    let sp_val = b.read_variable(&sp)?;
    let slot = |b: &mut strider_ir::FunctionBuilder, off: i64| -> Result<_> {
        let k = b.build_int_const(off as u64, ValueType::int(32))?;
        b.build_int_binary_operation(sp_val, k, IntBinaryOp::Add, ValueType::int(32))
    };
    let reloaded = slot(&mut b, -12)?;
    let arg = b.read_variable(&var)?;
    let zero = b.build_int_const(0u64, ValueType::int(32))?;
    let cond =
        b.build_int_cmp_operation(arg, zero, strider_ir::IntCmpOp::Equal, ValueType::int(32))?;
    b.build_if(cond, then_r, else_r)?;

    for (region, off) in [(then_r, -4), (else_r, -8)] {
        b.set_region(region);
        let spill = slot(&mut b, off)?;
        let data = b.build_int_const(7u64, ValueType::int(32))?;
        b.build_store(spill, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(reloaded, rsleigh::VnSpace::RAM, ValueType::int(32))?;
        b.write_variable(&var, loaded)?;
        b.build_branch(merge)?;
    }

    b.set_region(merge);
    let merged = b.read_variable(&var)?;
    b.build_return(Some(merged), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    run_passes(&mut fg, &[&crate::PhiCollapse, &crate::LoadForward])?;

    let ret = fg.returned();
    assert!(
        matches!(fg.node_kind(fg.producer(ret)), NodeKind::Load(_)),
        "the merged twins make the phi trivial, got {:?}",
        fg.node_kind(fg.producer(ret))
    );
    Ok(())
}

mod scaling {
    use crate::mem_analysis::WALK_STEPS;
    use crate::mem_ssa::CLIMB_STEPS;
    use crate::test_support::memory_shapes as shapes;
    use crate::test_support::{run_pass, steps};
    use strider_ir::node::NodeKind;
    use strider_ir_test_utils::IrWalkerEx;

    /// The analysis work one `LoadForward` run takes over `fg`: every def
    /// verdict asked plus every step of the climb.
    fn work(mut fg: strider_ir::Function) -> crate::Result<u64> {
        steps(&[&WALK_STEPS, &CLIMB_STEPS], || {
            run_pass(&crate::LoadForward, &mut fg).map(drop)
        })
    }

    /// Doubling the shape may only double the work, give or take a logarithm.
    fn assert_near_linear(
        name: &str,
        build: fn(usize) -> crate::Result<strider_ir::Function>,
    ) -> crate::Result<()> {
        crate::test_support::assert_near_linear(name, 100, |n| work(build(n)?), 260)
    }

    #[test]
    fn a_store_chain_costs_near_linear_work() -> crate::Result<()> {
        assert_near_linear("store chain", shapes::store_chain)
    }

    #[test]
    fn store_diamonds_cost_near_linear_work() -> crate::Result<()> {
        assert_near_linear("store diamonds", shapes::store_diamonds)
    }

    #[test]
    fn call_diamonds_cost_near_linear_work() -> crate::Result<()> {
        assert_near_linear("call diamonds", shapes::call_diamonds)
    }

    #[test]
    fn an_accumulator_loop_costs_near_linear_work() -> crate::Result<()> {
        assert_near_linear("accumulator loop", shapes::accumulator_loop)
    }

    #[test]
    fn sequential_loops_cost_near_linear_work() -> crate::Result<()> {
        assert_near_linear("sequential loops", shapes::sequential_loops)
    }

    #[test]
    fn nested_loops_cost_near_linear_work() -> crate::Result<()> {
        assert_near_linear("nested loops", shapes::nested_loops)
    }

    /// Each reload steps past every call above it, so a call one reload's
    /// climb rejected must not be asked again by the next.
    #[test]
    fn a_spill_call_chain_costs_near_linear_work() -> crate::Result<()> {
        let mut fg = shapes::spill_call_chain(4)?;
        run_pass(&crate::LoadForward, &mut fg)?;
        assert!(
            !fg.has_kind(|k| matches!(k, NodeKind::Load(_))),
            "every reload must forward past the calls, or nothing is measured"
        );
        assert_near_linear("spill call chain", shapes::spill_call_chain)
    }

    /// The slow climb, not the fast one: an irreducible memory graph has no
    /// position index, so this is the layout-free climb's own scaling.
    #[test]
    fn an_irreducible_store_chain_costs_near_linear_work() -> crate::Result<()> {
        let fg = shapes::irreducible_store_chain(8)?;
        assert!(
            crate::mem_ssa::MemLayout::build(&fg).positions().is_none(),
            "the shape must stay irreducible, or it measures the fast climb"
        );
        work(fg)?;
        assert!(
            crate::mem_ssa::CLIMB_STEPS.with(std::cell::Cell::get) > 0,
            "a pass over the shape must reach the slow climb, not the walk"
        );
        assert_near_linear("irreducible store chain", shapes::irreducible_store_chain)
    }
}

/// The claims-free configuration still reads memory as RAM: a status register
/// polled after a write reads back as the value written.
#[test]
fn no_assumption_lifts_the_ram_premise() -> Result<()> {
    let mut fg = strider_ir_test_utils::make_empty_fn(|b| {
        let register = b.build_int_const(0x4000_1000u64, ValueType::int(32))?;
        let one = b.build_int_const(1u64, ValueType::int(32))?;
        b.build_store(register, one, rsleigh::VnSpace::RAM)?;
        b.build_load(register, rsleigh::VnSpace::RAM, ValueType::int(32))
    })?;
    run_passes(&mut fg, STANDARD)?;
    crate::test_support::assert_returns_const(&fg, 1);
    Ok(())
}

/// `slice_to_load` reshapes in the load's space's byte order. The REGISTER space has
/// its own, which differs from the data order on ARM BE8, where the sla's
/// register block is little-endian under big-endian data: there a byte load
/// of a register slot is the low byte, and of RAM the high one.
#[test]
fn a_narrowed_load_takes_its_spaces_byte_order() -> Result<()> {
    let build = |space: rsleigh::VnSpace| -> Result<strider_ir::Function> {
        let sp = sp32_vn();
        let mut b = sp_frame(sp)
            .byte_order(strider_target::ByteOrder {
                data: Endianness::Big,
                register: Endianness::Little,
            })
            .build_fn_single_region()?;
        let addr = b.build_int_const(0x40u64, ValueType::int(32))?;
        let wide = b.build_int_const(0xDEAD_BEEFu64, ValueType::int(32))?;
        b.build_store(addr, wide, space)?;
        let loaded = b.build_load(addr, space, ValueType::int(8))?;
        b.build_return(Some(loaded), &[])?;
        b.set_lift_addr(None);
        b.build()
    };
    let run = |fg: &mut strider_ir::Function| -> Result<()> {
        run_passes(
            fg,
            &[
                &crate::PhiCollapse,
                &crate::RegionCollapse,
                &crate::LoadForward,
                &crate::ConstantFold,
            ],
        )?;
        Ok(())
    };

    let mut ram = build(rsleigh::VnSpace::RAM)?;
    run(&mut ram)?;
    crate::test_support::assert_returns_const(&ram, 0xDE);

    let mut reg = build(rsleigh::VnSpace::REGISTER)?;
    run(&mut reg)?;
    crate::test_support::assert_returns_const(&reg, 0xEF);
    Ok(())
}

/// A forward whose value becomes a store's address re-classes that store,
/// so the answers memoised before it are stale.  `S` stores through `L1`,
/// an `Anchor` that may alias the global `[0x5000]`; forwarding `L1` makes
/// `S`'s address the constant it spilled, disjoint from that global, and the
/// spill's slot is disjoint from it under `stack_global_disjoint`.
///
/// Mutation: keep the answers across the forward.
#[test]
fn a_forward_into_a_store_address_drops_the_memoised_answers() -> Result<()> {
    let sp = sp32_vn();
    let mut a_value = None;
    let mut l1_value = None;
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let i32_ty = ValueType::int(32);
        let global = b.build_int_const(0x5000u64, i32_ty)?;
        store32(b, global, 0x77)?;
        let s8 = i32_slot(b, sp_val, -8)?;
        store32(b, s8, 0x9000)?;
        let l1 = load32(b, s8)?;
        store32(b, l1, 0x11)?;
        // A read wider than the store: its verdict is an exact match, but it
        // is not forwarded, so it stays to be asked again.
        let a = b.build_load(global, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        b.build_return(Some(a), &[])?;
        (a_value, l1_value) = (Some(a), Some(l1));
        Ok(())
    })?;
    run_passes(&mut fg, COLLAPSE)?;
    fg.assume(&claiming(true, false));
    let (a, l1) = (
        fg.producer(a_value.expect("built")),
        fg.producer(l1_value.expect("built")),
    );

    let query = MemQuery::default();
    let mut edit = crate::EditFunction::new(&mut fg);
    try_forward_load(&mut edit, a, &query)?;
    let before = edit.generation();
    try_forward_load(&mut edit, l1, &query)?;
    assert_ne!(edit.generation(), before, "the address load forwards");
    let f = edit.function();
    let policy = Policy::forward(f);
    let fresh = MemQuery::default().reaching(f, a, policy).node();
    assert!(
        matches!(f.node_kind(fresh), NodeKind::Store(_))
            && f.int_const_u128(f.store_addr(fresh)) == Some(0x5000),
        "the store through the forwarded address no longer aliases the global"
    );
    assert_eq!(
        query.reaching(f, a, policy).node(),
        fresh,
        "the query answers what a fresh one does"
    );
    Ok(())
}

/// A store wider than 16 bytes forwards to a narrower load like any other:
/// the reshape's shift constant is built at the store's width, which
/// `build_int_const` takes past 128 bits.
///
/// Mutation: refuse a reshape of a store wider than 16 bytes.
fn forward_from_a_wide_store(order: Endianness) -> Result<strider_ir::Function> {
    let sp = sp32_vn();
    let mut b = sp_frame(sp).byte_order(order).build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    let addr = b.build_const_binop(
        (-32i64) as u64,
        sp_val,
        IntBinaryOp::Add,
        ValueType::int(32),
    )?;
    let wide = (strider_ir::UBig::from(0x1122_3344u32) << 224usize)
        | strider_ir::UBig::from(0x5566_7788u32);
    let wide = b.build_int_const(wide, ValueType::int(256))?;
    b.build_store(addr, wide, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(32))?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;
    run_passes(&mut fg, STANDARD)?;
    Ok(fg)
}

#[test]
fn a_store_wider_than_16_bytes_forwards_its_low_bytes_on_little_endian() -> Result<()> {
    let fg = forward_from_a_wide_store(Endianness::Little)?;
    crate::test_support::assert_returns_const(&fg, 0x5566_7788);
    Ok(())
}

#[test]
fn a_store_wider_than_16_bytes_forwards_its_high_bytes_on_big_endian() -> Result<()> {
    let fg = forward_from_a_wide_store(Endianness::Big)?;
    crate::test_support::assert_returns_const(&fg, 0x1122_3344);
    Ok(())
}

/// The store and the load's address are the forward's proof, so the value
/// that replaces the load inherits the store's and the address producer's
/// fingerprints; without them that asm goes with the culled load.
///
/// Mutation: absorb only the load's own fingerprint.
#[test]
fn a_forward_absorbs_the_store_and_the_address() -> Result<()> {
    const STORE_ASM: u64 = 0xC0DE_0001;
    const ADDR_ASM: u64 = 0xC0DE_0002;
    let sp = sp32_vn();
    let mut b = sp_frame(sp).build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    let slot = b.build_const_binop((-8i64) as u64, sp_val, IntBinaryOp::Add, ValueType::int(32))?;
    let data = b.build_int_const(0x42u64, ValueType::int(32))?;
    b.set_lift_addr(Some(STORE_ASM));
    b.build_store(slot, data, rsleigh::VnSpace::RAM)?;
    b.set_lift_addr(Some(ADDR_ASM));
    let four = b.build_int_const((-4i64) as u64, ValueType::int(32))?;
    let half = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::int(32))?;
    let reread = b.build_int_binary_operation(half, four, IntBinaryOp::Add, ValueType::int(32))?;
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let loaded = b.build_load(reread, rsleigh::VnSpace::RAM, ValueType::int(32))?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    run_passes(&mut fg, &[&crate::PhiCollapse, &crate::LoadForward])?;

    crate::test_support::assert_returns_const(&fg, 0x42);
    let forwarded = fg.producer(fg.returned());
    let fingerprint = fg.side_tables().asm_fingerprint(forwarded);
    assert!(
        fingerprint.contains(&STORE_ASM) && fingerprint.contains(&ADDR_ASM),
        "the forwarded value must carry the store's and the address's asm; \
         got {fingerprint:?}"
    );
    Ok(())
}

/// `p = alloc(); [sp-24] = 0x6000; L2 = [sp-24]; [sp-16] = 0x77; [p] = 0x9000;
/// [j] = 0x55; L1 = [p]; [L1] = 0x11; return [sp-16]`, `j` being `L2 + 0x10`
/// or, when `spelled`, the constant it forwards to.
fn opaque_stores_that_forward_to_globals(spelled: bool) -> Result<Function> {
    const ALLOCATOR: u64 = 0x1000;
    let sp = sp32_vn();
    let ret_reg = rsleigh::Vn {
        addr_off: 0x00,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let mut b = strider_ir_test_utils::RegisterSet::new()
        .tracked(sp)
        .tracked(ret_reg)
        .callee_saved(sp)
        .ret(ret_reg)
        .stack_vn(sp)
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    let call_sp = i32_slot(&mut b, sp_val, -32)?;
    b.write_variable(&sp, call_sp)?;
    let target = b.build_int_const(ALLOCATOR, ValueType::int(32))?;
    let (_, rets) = b.build_call(target, &[], &[ret_reg], 0)?;
    let p = rets[0];
    let s24 = i32_slot(&mut b, sp_val, -24)?;
    store32(&mut b, s24, 0x6000)?;
    let l2 = load32(&mut b, s24)?;
    let s16 = i32_slot(&mut b, sp_val, -16)?;
    store32(&mut b, s16, 0x77)?;
    store32(&mut b, p, 0x9000)?;
    let j = if spelled {
        b.build_int_const(0x6010u64, ValueType::int(32))?
    } else {
        b.build_const_binop(0x10u64, l2, IntBinaryOp::Add, ValueType::int(32))?
    };
    store32(&mut b, j, 0x55)?;
    let l1 = load32(&mut b, p)?;
    store32(&mut b, l1, 0x11)?;
    let ret = load32(&mut b, s16)?;
    b.build_return(Some(ret), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;
    fg.assume(&AssumptionOptions {
        escape_analysis: true,
        noalias_allocators: [ALLOCATOR].into_iter().collect(),
        ..AssumptionOptions::none()
    });
    run_passes(&mut fg, STANDARD)?;
    Ok(fg)
}

/// A forward that turns an opaque store's address into a constant cannot
/// take back a reload already forwarded past that store, so without
/// `stack_global_disjoint` the opaque store blocks the reload from the start,
/// as the constant it becomes does.
///
/// Mutation: skip an opaque store over a private frame whatever
/// `stack_global_disjoint` says.
#[test]
fn an_opaque_store_blocks_as_the_global_it_forwards_to() -> Result<()> {
    let spelled = opaque_stores_that_forward_to_globals(true)?;
    assert_eq!(outcome(&spelled), None, "a global store may alias the slot");
    assert_eq!(
        outcome(&opaque_stores_that_forward_to_globals(false)?),
        outcome(&spelled),
        "the pass order decides nothing"
    );
    Ok(())
}
