use super::*;
use crate::Result;
use crate::test_support::{CF_RP, run_passes};
use strider_ir::node::{NodeKind, ValueId, ValueType};
use strider_ir::{FunctionBuilder, IRBuilderExt, IRViewer, IntBinaryOp};
use strider_ir_test_utils::IrBuilderEx;
use strider_ir_test_utils::IrWalkerEx;
use strider_ir_test_utils::{
    RegisterSet, SENTINEL_LIFT_ADDR, reg_vn, sp_frame, stack_args_at, stack_vn_aarch64,
    stack_vn_x86 as sp32_vn, stack_vn_x86_64 as stack_vn,
};

/// [`CF_RP`] with `FunctionArgDetect` as its post pass.
fn detect(fg: &mut strider_ir::Function) -> Result<()> {
    let mut p = crate::test_support::pipeline(CF_RP);
    p.add_post_pass(&FunctionArgDetect);
    p.run(fg, &crate::OptCtx::new(None))
}

fn rdi_like_vn() -> rsleigh::Vn {
    // Stands in for x86_64 RDI.
    reg_vn(0x38, 8)
}

/// x86 cdecl reads its first stack arg at `[sp + 4]`, and with no register
/// args that `Load` is arg 0.  It must also remain reachable.
#[test]
fn reads_stack_arg_0_on_x86_cdecl() -> Result<()> {
    let sp = sp32_vn();
    let mut b = sp_frame(sp)
        .stack_args(stack_args_at(4, 8))
        .build_fn_single_region()?;
    {
        let loaded = build_sp_load(&mut b, &sp, 4)?;
        b.build_return(Some(loaded), &[])?;
    }
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // ConstantFold normalises the address before detection runs.
    detect(&mut fg)?;

    let arg0_nodes = fg.side_tables().arg_index_to_values(0);
    assert!(!arg0_nodes.is_empty(), "arg 0 should be registered (stack)");
    assert_eq!(arg0_nodes.len(), 1, "one Load at sp+4, so one carrier");
    assert!(
        matches!(fg.node_kind(fg.producer(arg0_nodes[0])), NodeKind::Load(_)),
        "carrier for stack arg 0 must be a Load node"
    );

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 1,
        "Load[sp+4] must remain reachable after the pass"
    );
    Ok(())
}

/// A `Load` rooted at an alignment-masked SP addresses a frame local, not an
/// incoming arg, so only an `InitialVar(sp)` base qualifies.
#[test]
fn aligned_sp_load_is_not_a_stack_arg() -> Result<()> {
    let sp = sp32_vn();
    let mut b = sp_frame(sp)
        .stack_args(stack_args_at(4, 8))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    let aligned =
        b.build_const_binop(0xFFFF_FFF8u64, sp_val, IntBinaryOp::And, ValueType::int(32))?;
    let addr = b.build_const_binop(4u64, aligned, IntBinaryOp::Add, ValueType::int(32))?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(32))?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    detect(&mut fg)?;

    assert!(
        fg.side_tables().arg_index_to_values(0).is_empty(),
        "a load rooted at an alignment-masked SP (not the entry SP) must not \
         register as stack arg 0"
    );
    Ok(())
}

fn build_sp_load(
    b: &mut FunctionBuilder,
    sp: &rsleigh::Vn,
    offset: u32,
) -> Result<strider_ir::node::ValueId> {
    let sp_val = b.read_variable(sp)?;
    let addr = b.build_const_binop(offset as u64, sp_val, IntBinaryOp::Add, ValueType::int(32))?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(32))?;
    Ok(loaded)
}

/// Ten incoming stack args, proving the `StackArgs` formula has no upper bound.
#[test]
fn detects_ten_contiguous_stack_args() -> Result<()> {
    const N: usize = 10;
    let sp = sp32_vn();
    let mut b = sp_frame(sp)
        .stack_args(stack_args_at(0, 8))
        .build_fn_single_region()?;
    // No intervening stores, so each load reads InitialMemory.
    let mut acc = None;
    for i in 0..N {
        let loaded = build_sp_load(&mut b, &sp, (i * 8) as u32)?;
        acc = Some(match acc {
            None => loaded,
            Some(prev) => {
                b.build_int_binary_operation(prev, loaded, IntBinaryOp::Add, ValueType::int(32))?
            }
        });
    }
    b.build_return(acc, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    detect(&mut fg)?;

    for i in 0..N {
        assert!(
            !fg.side_tables().arg_index_to_values(i as u32).is_empty(),
            "arg {i} (sp + {}) must be registered",
            i * 8
        );
    }
    Ok(())
}

/// Loads at sp+4 and sp+12 but not sp+8: only the contiguous prefix is
/// labelled, so the sp+12 load is left unregistered.  No gap-spanning.
#[test]
fn stack_arg_gap_truncates() -> Result<()> {
    let sp = sp32_vn();
    let mut b = sp_frame(sp)
        .stack_args(stack_args_at(4, 4))
        .build_fn_single_region()?;
    let a = build_sp_load(&mut b, &sp, 4)?;
    let c = build_sp_load(&mut b, &sp, 12)?;
    // Combined so neither load is dead.
    let sum = b.build_int_binary_operation(a, c, IntBinaryOp::Add, ValueType::int(32))?;
    b.build_return(Some(sum), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    detect(&mut fg)?;

    let arg0_nodes = fg.side_tables().arg_index_to_values(0);
    assert!(!arg0_nodes.is_empty(), "arg 0 (sp+4) should be registered");

    let arg1_nodes = fg.side_tables().arg_index_to_values(1);
    assert!(
        arg1_nodes.is_empty(),
        "arg 1 (sp+8) is absent: nothing at that offset"
    );

    let arg2_nodes = fg.side_tables().arg_index_to_values(2);
    assert!(
        arg2_nodes.is_empty(),
        "arg 2 (sp+12) must be truncated by the gap"
    );

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 2,
        "both Load nodes (sp+4 and sp+12) must remain reachable"
    );
    Ok(())
}

/// A `Load[sp+4]` reached through disjoint stores at +8 and +12 is still arg 0,
/// and the walker also narrows its memory edge onto `InitialMemory`.  Narrowing
/// never changes which args are detected.
#[test]
fn stack_arg_load_chain_is_narrowed_without_changing_detection() -> Result<()> {
    let sp = sp32_vn();
    let mut b = sp_frame(sp)
        .stack_args(stack_args_at(4, 8))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    for off in [8u64, 12u64] {
        let addr = b.build_const_binop(off, sp_val, IntBinaryOp::Add, ValueType::int(32))?;
        let v = b.build_int_const(off, ValueType::int(32))?;
        b.build_store(addr, v, rsleigh::VnSpace::RAM)?;
    }
    let loaded = build_sp_load(&mut b, &sp, 4)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    detect(&mut fg)?;

    let arg0 = fg.side_tables().arg_index_to_values(0).to_vec();
    assert_eq!(arg0.len(), 1, "Load[sp+4] registered as arg 0");

    let load = fg.producer(arg0[0]);
    let mem = fg.node_inputs(load)[0];
    assert!(
        matches!(fg.node_kind(fg.producer(mem)), NodeKind::InitialMemory),
        "stack-arg load narrowed past disjoint stores onto InitialMemory, got {:?}",
        fg.node_kind(fg.producer(mem)),
    );
    Ok(())
}

/// `Load[sp+4]` below a `MemPhi` whose arms each store 4 bytes at their
/// offset (`None`: no store), detected as stack arg 0 or not.
fn memphi_arms_detect(arms: [Option<u64>; 2]) -> Result<bool> {
    let sp = sp32_vn();
    let mut b = sp_frame(sp).stack_args(stack_args_at(4, 8)).build_fn()?;
    let entry = b.create_region_all()?;
    let arm_regions = [b.create_region_all()?, b.create_region_all()?];
    let join = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    // A boolean const keeps the MemPhi at two predecessors, and
    // `CF_RP` leaves that branch standing; DeadBranchElimination
    // would collapse it.
    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, arm_regions[0], arm_regions[1])?;

    for (region, offset) in arm_regions.into_iter().zip(arms) {
        b.set_region(region);
        if let Some(offset) = offset {
            let sp_a = b.read_variable(&sp)?;
            let addr = b.build_const_binop(offset, sp_a, IntBinaryOp::Add, ValueType::int(32))?;
            let data = b.build_int_const(0x22u64, ValueType::int(32))?;
            b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        }
        b.build_branch(join)?;
    }

    b.set_region(join);
    let loaded = build_sp_load(&mut b, &sp, 4)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    detect(&mut fg)?;
    Ok(!fg.side_tables().arg_index_to_values(0).is_empty())
}

/// Every predecessor of a `MemPhi` has to be clean: one arm storing at the
/// load's slot, or overlapping its range, disqualifies the load.
#[test]
fn memphi_shadow_disqualifies() -> Result<()> {
    for (arms, detected, why) in [
        ([Some(4), None], false, "one arm stores the slot"),
        // [2,6) overlaps [4,8); [8,12) does not.
        ([Some(2), Some(8)], false, "one arm overlaps the slot"),
        ([Some(8), Some(12)], true, "both arms are clean"),
    ] {
        assert_eq!(memphi_arms_detect(arms)?, detected, "{why}");
    }
    Ok(())
}

/// One slot read at two widths, as aarch64 does with `x0` and `w0`, must
/// register BOTH `Load` nodes under index 0.
#[test]
fn narrower_load_at_arg_slot_uses_truncate() -> Result<()> {
    let sp = stack_vn_aarch64();
    let mut b = sp_frame(sp)
        .stack_args(stack_args_at(0, 8))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    // Combined so neither load is dead.
    let narrow = b.build_load(sp_val, rsleigh::VnSpace::RAM, ValueType::int(32))?;
    let wide = b.build_load(sp_val, rsleigh::VnSpace::RAM, ValueType::int(64))?;
    let narrow_ext =
        b.extend_if_needed(narrow, ValueType::int(64), strider_ir::ExtendOp::ZeroExtend)?;
    let sum =
        b.build_int_binary_operation(narrow_ext, wide, IntBinaryOp::Add, ValueType::int(64))?;
    b.build_return(Some(sum), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    detect(&mut fg)?;

    let arg0_nodes = fg.side_tables().arg_index_to_values(0);
    assert_eq!(
        arg0_nodes.len(),
        2,
        "both Loads at sp+0 (I32 and I64) must be registered for arg 0"
    );
    assert!(
        arg0_nodes
            .iter()
            .all(|&v| matches!(fg.node_kind(fg.producer(v)), NodeKind::Load(_))),
        "all registered carriers for stack arg 0 must be Load nodes"
    );

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 2,
        "both Loads must remain reachable after the pass"
    );
    Ok(())
}

/// 32-bit cdecl `f(double a, int b)`: `a` spans two slots at `sp+4`, `b` sits
/// at `sp+12`, and they must come out as ordinals 0 and 1.
#[test]
fn wide_arg_then_narrow_arg_indexed_by_ordinal() -> Result<()> {
    let sp = sp32_vn();
    let mut b = sp_frame(sp)
        .stack_args(stack_args_at(4, 4))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    // The 8-byte `double`, spanning slots 0 and 1.
    let addr_a = b.build_const_binop(4u64, sp_val, IntBinaryOp::Add, ValueType::int(32))?;
    let a = b.build_load(addr_a, rsleigh::VnSpace::RAM, ValueType::int(64))?;
    // The `int`, at slot 2.
    let bv = build_sp_load(&mut b, &sp, 12)?;
    let b_ext = b.extend_if_needed(bv, ValueType::int(64), strider_ir::ExtendOp::ZeroExtend)?;
    let sum = b.build_int_binary_operation(a, b_ext, IntBinaryOp::Add, ValueType::int(64))?;
    b.build_return(Some(sum), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    detect(&mut fg)?;

    let arg0 = fg.side_tables().arg_index_to_values(0);
    assert_eq!(arg0.len(), 1, "wide arg (double) at sp+4 is ordinal 0");
    assert!(
        matches!(fg.node_kind(fg.producer(arg0[0])), NodeKind::Load(_)),
        "arg 0 carrier must be a Load node"
    );

    let arg1 = fg.side_tables().arg_index_to_values(1);
    assert_eq!(
        arg1.len(),
        1,
        "narrow arg (int) at sp+12 is ordinal 1, not lost to the slot-1 gap \
         the wide arg leaves behind"
    );
    assert!(
        matches!(fg.node_kind(fg.producer(arg1[0])), NodeKind::Load(_)),
        "arg 1 carrier must be a Load node"
    );
    Ok(())
}

/// 8-byte loads at `sp+4`, `sp+8` and `sp+12` overlap: the one starting
/// inside the first run reaches slot 2, so all three are one argument.
#[test]
fn a_load_starting_inside_a_run_widens_it() -> Result<()> {
    let sp = sp32_vn();
    let mut b = sp_frame(sp)
        .stack_args(stack_args_at(4, 4))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    let mut acc = None;
    for offset in [4u64, 8, 12] {
        let addr = b.build_const_binop(offset, sp_val, IntBinaryOp::Add, ValueType::int(32))?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        acc = Some(match acc {
            None => loaded,
            Some(prev) => {
                b.build_int_binary_operation(prev, loaded, IntBinaryOp::Add, ValueType::int(64))?
            }
        });
    }
    b.build_return(acc, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    detect(&mut fg)?;

    assert_eq!(fg.side_tables().arg_index_to_values(0).len(), 3);
    assert!(fg.side_tables().arg_index_to_values(1).is_empty());
    Ok(())
}

/// Span 4: an `I128` at `sp+0` covers slots 0..3, so the ordinal must advance
/// by exactly one across all of them and the following `I32` at `sp+16` must
/// not be lost to the three covered slots.
#[test]
fn span_four_wide_arg_then_narrow_arg_indexed_by_ordinal() -> Result<()> {
    let sp = sp32_vn();
    let mut b = sp_frame(sp)
        .stack_args(stack_args_at(0, 4))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    // 16 bytes, spanning slots 0..3.
    let a = b.build_load(sp_val, rsleigh::VnSpace::RAM, ValueType::int(128))?;
    // Slot 4.
    let bv = build_sp_load(&mut b, &sp, 16)?;
    // Truncate the I128 to I32 so the sum is well-typed and neither load dies.
    let a_trunc = {
        let trunc = strider_ir_test_utils::sentinel_node(
            b.function_mut(),
            NodeKind::Truncate,
            [a],
            [strider_ir::node::ValueKind::Typed(ValueType::int(32))],
        );
        b.function().node_outputs_exact::<1>(trunc).unwrap()[0]
    };
    let sum = b.build_int_binary_operation(a_trunc, bv, IntBinaryOp::Add, ValueType::int(32))?;
    b.build_return(Some(sum), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    detect(&mut fg)?;

    let arg0 = fg.side_tables().arg_index_to_values(0);
    assert_eq!(
        arg0.len(),
        1,
        "the I128 (16-byte) wide arg at sp+0 spans four slots but is ordinal 0"
    );
    assert!(
        matches!(fg.node_kind(fg.producer(arg0[0])), NodeKind::Load(_)),
        "arg 0 carrier must be a Load node"
    );

    let arg1 = fg.side_tables().arg_index_to_values(1);
    assert_eq!(
        arg1.len(),
        1,
        "the narrow I32 at sp+16 is ordinal 1, not lost to the three slots \
         (1..3) the wide I128 covers"
    );
    assert!(
        matches!(fg.node_kind(fg.producer(arg1[0])), NodeKind::Load(_)),
        "arg 1 carrier must be a Load node"
    );
    Ok(())
}

/// Two register args plus a stack arg at `sp+8` must register at indices 0, 1,
/// and 2.
#[test]
fn x86_64_mixed_reg_and_stack() -> Result<()> {
    let rdi = rdi_like_vn();
    let rsi = rsleigh::Vn {
        addr_off: 0x30,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let sp = stack_vn();
    // No ret-val regs, so the validator skips the Return tail's arity check.
    let mut b = RegisterSet::new()
        .tracked(rdi)
        .tracked(rsi)
        .tracked(sp)
        .arg(rdi)
        .arg(rsi)
        .stack_vn(sp)
        .stack_args(stack_args_at(8, 8))
        .callee_saved(rdi)
        .build_fn_single_region()?;

    let a = b.read_variable(&rdi)?;
    let bb = b.read_variable(&rsi)?;
    let sp_val = b.read_variable(&sp)?;
    let addr = b.build_const_binop(8u64, sp_val, IntBinaryOp::Add, ValueType::int(64))?;
    let c = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(64))?;
    let ab = b.build_int_binary_operation(a, bb, IntBinaryOp::Add, ValueType::int(64))?;
    let abc = b.build_int_binary_operation(ab, c, IntBinaryOp::Add, ValueType::int(64))?;
    b.build_return(Some(abc), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    detect(&mut fg)?;

    let arg0 = fg.side_tables().arg_index_to_values(0);
    assert!(!arg0.is_empty(), "arg 0 (rdi) should be registered");
    assert!(
        matches!(fg.node_kind(fg.producer(arg0[0])), NodeKind::InitialVar(v) if fg.initial_vn(*v) ==rdi),
        "arg 0 carrier must be InitialVar(rdi)"
    );

    let arg1 = fg.side_tables().arg_index_to_values(1);
    assert!(!arg1.is_empty(), "arg 1 (rsi) should be registered");
    assert!(
        matches!(fg.node_kind(fg.producer(arg1[0])), NodeKind::InitialVar(v) if fg.initial_vn(*v) ==rsi),
        "arg 1 carrier must be InitialVar(rsi)"
    );

    let arg2 = fg.side_tables().arg_index_to_values(2);
    assert!(!arg2.is_empty(), "arg 2 (sp+8) should be registered");
    assert!(
        matches!(fg.node_kind(fg.producer(arg2[0])), NodeKind::Load(_)),
        "arg 2 carrier must be a Load node"
    );
    Ok(())
}

/// A store before `Load[sp+4]` shadows it exactly when their byte ranges
/// overlap, whatever the store's own offset.
#[test]
fn a_store_shadows_the_slot_only_where_its_bytes_overlap() -> Result<()> {
    let (i32_ty, i64_ty) = (ValueType::int(32), ValueType::int(64));
    for (why, sp, store_off, store_ty, load_ty, detected) in [
        (
            "[0,8) overlaps [4,12)",
            stack_vn_aarch64(),
            0,
            i64_ty,
            i64_ty,
            false,
        ),
        (
            "[0,4) is disjoint from [4,8)",
            sp32_vn(),
            0,
            i32_ty,
            i32_ty,
            true,
        ),
        (
            "[4,8) is the load's own range",
            sp32_vn(),
            4,
            i32_ty,
            i32_ty,
            false,
        ),
    ] {
        let addr_ty = ValueType::int(u16::try_from(sp.size * 8)?);
        let mut b = sp_frame(sp)
            .stack_args(stack_args_at(4, 8))
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let store_addr = crate::test_support::at(&mut b, sp_val, store_off, addr_ty)?;
        let data = b.build_int_const(0x11u64, store_ty)?;
        b.build_store(store_addr, data, rsleigh::VnSpace::RAM)?;
        let addr4 = crate::test_support::at(&mut b, sp_val, 4, addr_ty)?;
        let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, load_ty)?;
        b.build_return(Some(loaded), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        detect(&mut fg)?;
        let want = if detected { vec![loaded] } else { vec![] };
        assert_eq!(fg.side_tables().arg_index_to_values(0), want, "{why}");
    }
    Ok(())
}

/// An isolated high-offset load with nothing below it registers no args at
/// all: nothing starts the contiguous prefix.
#[test]
fn isolated_high_offset_load_dropped() -> Result<()> {
    let sp = sp32_vn();
    let mut b = sp_frame(sp)
        .stack_args(stack_args_at(4, 4))
        .build_fn_single_region()?;
    let v = build_sp_load(&mut b, &sp, 12)?;
    b.build_return(Some(v), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    detect(&mut fg)?;

    assert_eq!(
        fg.side_tables().iter_arg_indices().count(),
        0,
        "isolated sp+12 load must not be registered without arg 0/1"
    );
    Ok(())
}

/// `sub rsp, 0xFFFFFFFFFFFFFFFC` is an alternate encoding of `add rsp, 4`:
/// sign-extended the constant is -4, so `Sub(sp, -4) = sp + 4` and the
/// resulting `Load` must still be a candidate for offset +4.
#[test]
fn load_via_sub_negative_unsigned_recognised_as_stack_arg() -> Result<()> {
    let sp = stack_vn();
    let mut b = sp_frame(sp)
        .stack_args(stack_args_at(4, 8))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    // -4 read as signed i64.
    let neg_four = b.build_int_const(0xFFFF_FFFF_FFFF_FFFCu64, ValueType::int(64))?;
    let addr = b.build_sub_as_add_neg(sp_val, neg_four, ValueType::int(64))?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(64))?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // ConstantFold folds `Add(_, Neg(K))` to `Add(_, IntConst(-K))`, the shape
    // decomposition sees in production; it does not peel the `Neg` itself.
    detect(&mut fg)?;

    let arg0_nodes = fg.side_tables().arg_index_to_values(0);
    assert!(
        !arg0_nodes.is_empty(),
        "Sub(sp, 0xFFFFFFFFFFFFFFFC_U64) must decompose to offset +4 and be registered as arg 0",
    );
    assert!(
        matches!(fg.node_kind(fg.producer(arg0_nodes[0])), NodeKind::Load(_)),
        "carrier for arg 0 (stack-arg via negative sub) must be a Load node"
    );
    Ok(())
}

/// A reload after the function writes its incoming slot is no carrier, but
/// the load before the write still is, and the slots after it stand.
///
/// Mutation: drop every load of a slot one of whose loads is dirty.
#[test]
fn a_dirty_reload_leaves_the_clean_carrier_of_its_slot() -> Result<()> {
    let sp = sp32_vn();
    let mut b = sp_frame(sp)
        .stack_args(stack_args_at(4, 8))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    let clean = build_sp_load(&mut b, &sp, 4)?;
    let addr = b.build_const_binop(4u64, sp_val, IntBinaryOp::Add, ValueType::int(32))?;
    let data = b.build_int_const(0x11u64, ValueType::int(32))?;
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    let dirty = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(32))?;
    let next = build_sp_load(&mut b, &sp, 12)?;
    let sum = b.build_int_binary_operation(clean, dirty, IntBinaryOp::Add, ValueType::int(32))?;
    let sum = b.build_int_binary_operation(sum, next, IntBinaryOp::Add, ValueType::int(32))?;
    b.build_return(Some(sum), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    detect(&mut fg)?;

    assert_eq!(fg.side_tables().arg_index_to_values(0), [clean]);
    assert_eq!(fg.side_tables().arg_index_to_values(1), [next]);
    Ok(())
}

/// A dirty reload spanning two slots may read the function's own wide store,
/// so it says nothing of where the arguments lie: the slot it reaches past
/// its own slot's clean loads ends the prefix, and nothing is merged.
///
/// Mutation: join a dirty load's span into its slot's reach.
#[test]
fn a_dirty_wide_reload_ends_the_prefix_at_its_slot() -> Result<()> {
    for (store_ty, with_at12) in [(ValueType::int(32), true), (ValueType::int(64), false)] {
        let sp = sp32_vn();
        let mut b = sp_frame(sp)
            .stack_args(stack_args_at(4, 4))
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let at4 = build_sp_load(&mut b, &sp, 4)?;
        let at8 = build_sp_load(&mut b, &sp, 8)?;
        let at12 = if with_at12 {
            Some(build_sp_load(&mut b, &sp, 12)?)
        } else {
            None
        };
        let addr = b.build_const_binop(4u64, sp_val, IntBinaryOp::Add, ValueType::int(32))?;
        let data = b.build_int_const(0x11u64, store_ty)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        let dirty = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        let low = b.truncate_if_needed(dirty, ValueType::int(32))?;
        let mut sum = low;
        for v in [Some(at4), Some(at8), at12].into_iter().flatten() {
            sum = b.build_int_binary_operation(sum, v, IntBinaryOp::Add, ValueType::int(32))?;
        }
        b.build_return(Some(sum), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;

        detect(&mut fg)?;

        assert_eq!(fg.side_tables().arg_index_to_values(0), [at4], "{store_ty}");
        assert!(
            fg.side_tables().arg_index_to_values(1).is_empty(),
            "{store_ty}"
        );
        assert!(
            fg.side_tables().arg_index_to_values(2).is_empty(),
            "{store_ty}"
        );
    }
    Ok(())
}

/// A dirty load starting inside a wide argument and reaching past it ends
/// the prefix after that argument too.
///
/// Mutation: test only a dirty load at the argument's first slot.
#[test]
fn a_dirty_reload_from_inside_a_wide_argument_ends_the_prefix() -> Result<()> {
    let sp = sp32_vn();
    let mut b = sp_frame(sp)
        .stack_args(stack_args_at(4, 4))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    let at4_addr = b.build_const_binop(4u64, sp_val, IntBinaryOp::Add, ValueType::int(32))?;
    let wide = b.build_load(at4_addr, rsleigh::VnSpace::RAM, ValueType::int(64))?;
    let at12 = build_sp_load(&mut b, &sp, 12)?;
    let addr = b.build_const_binop(8u64, sp_val, IntBinaryOp::Add, ValueType::int(32))?;
    let data = b.build_int_const(0x11u64, ValueType::int(32))?;
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    let dirty = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(64))?;
    let mut sum = at12;
    for v in [wide, dirty] {
        let low = b.truncate_if_needed(v, ValueType::int(32))?;
        sum = b.build_int_binary_operation(sum, low, IntBinaryOp::Add, ValueType::int(32))?;
    }
    b.build_return(Some(sum), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    detect(&mut fg)?;

    assert_eq!(fg.side_tables().arg_index_to_values(0), [wide]);
    assert!(fg.side_tables().arg_index_to_values(1).is_empty());
    Ok(())
}

/// Soundness floor: a cross-class store (a const-encoded global) cannot be
/// proven disjoint from an SP-rooted load, so with `stack_global_disjoint`
/// cleared it marks the chain dirty.  Registering the load anyway would
/// substitute a pre-entry value and mask the global's write.  Claimed, the
/// global write is assumed disjoint and the load is promoted.
#[test]
fn mem_chain_is_dirty_on_non_sp_intervening_store() -> Result<()> {
    for claimed in [false, true] {
        let sp = sp32_vn();
        let mut b = sp_frame(sp)
            .stack_args(stack_args_at(4, 8))
            .build_fn_single_region()?;
        // A global write to a fixed `.data` address.
        let global_addr = b.build_int_const(0xDEAD_BEEFu64, ValueType::int(32))?;
        let global_data = b.build_int_const(0x1234u64, ValueType::int(32))?;
        b.build_store(global_addr, global_data, rsleigh::VnSpace::RAM)?;
        // The load's memory predecessor IS the global store above.
        let loaded = build_sp_load(&mut b, &sp, 4)?;
        b.build_return(Some(loaded), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        fg.assume(&crate::AssumptionOptions {
            stack_global_disjoint: claimed,
            ..crate::AssumptionOptions::none()
        });

        detect(&mut fg)?;

        assert_eq!(
            fg.side_tables().arg_index_to_values(0),
            if claimed { vec![loaded] } else { vec![] },
            "claimed {claimed}: the global store shadows the slot only at the floor"
        );
    }
    Ok(())
}

/// A store through an SP-rooted phi that does NOT collapse to a single
/// terminal must conservatively mark the chain dirty.  The branches do
/// `sp -= 4` and `sp -= 8`, so the join phi disagrees, decomposition returns
/// `None`, and the store's address cannot be range-checked at all.
#[test]
fn mem_chain_is_dirty_terminates_at_overlapping_phi_of_sp() -> Result<()> {
    let sp = sp32_vn();
    let mut b = sp_frame(sp).stack_args(stack_args_at(4, 8)).build_fn()?;
    let entry = b.create_region_all()?;
    let then_r = b.create_region_all()?;
    let else_r = b.create_region_all()?;
    let join = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    // Snapshot the original SP before the diamond.
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let sp_orig = b.read_variable(&sp)?;
    let cond = b.build_boolean_const(true);
    b.build_if(cond, then_r, else_r)?;

    b.set_region(then_r);
    let sp_t = b.read_variable(&sp)?;
    let four_t = b.build_int_const(4u64, ValueType::int(32))?;
    let sp_t_new = b.build_sub_as_add_neg(sp_t, four_t, ValueType::int(32))?;
    b.write_variable(&sp, sp_t_new)?;
    b.build_branch(join)?;

    b.set_region(else_r);
    let sp_e = b.read_variable(&sp)?;
    let eight_e = b.build_int_const(8u64, ValueType::int(32))?;
    let sp_e_new = b.build_sub_as_add_neg(sp_e, eight_e, ValueType::int(32))?;
    b.write_variable(&sp, sp_e_new)?;
    b.build_branch(join)?;

    // Store through the phi'd SP, whose address decomposes to None, then load
    // the stack-arg slot off the original SP.
    b.set_region(join);
    let phi_sp = b.read_variable(&sp)?;
    let trash = b.build_int_const(0xAAu64, ValueType::int(32))?;
    b.build_store(phi_sp, trash, rsleigh::VnSpace::RAM)?;

    let addr_j = b.build_const_binop(4u64, sp_orig, IntBinaryOp::Add, ValueType::int(32))?;
    let loaded = b.build_load(addr_j, rsleigh::VnSpace::RAM, ValueType::int(32))?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    detect(&mut fg)?;

    let arg0_nodes = fg.side_tables().arg_index_to_values(0);
    assert!(
        arg0_nodes.is_empty(),
        "Store through a non-collapsing SP-phi address must conservatively mark chain dirty: no arg registered"
    );
    Ok(())
}

#[test]
fn mem_chain_is_dirty_handles_10k_disjoint_store_chain() -> Result<()> {
    const CHAIN_LEN: usize = 10_000;

    let sp = sp32_vn();
    let mut b = sp_frame(sp)
        .stack_args(stack_args_at(4, 8))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    // Disjoint stack stores at offsets 16, 20, 24, ...
    for i in 0..CHAIN_LEN {
        let addr = b.build_const_binop(
            ((i * 4) as u64) + 16,
            sp_val,
            IntBinaryOp::Add,
            ValueType::int(32),
        )?;
        let val = b.build_int_const(i as u64, ValueType::int(32))?;
        b.build_store(addr, val, rsleigh::VnSpace::RAM)?;
    }
    // Disjoint from every store above, so the walker passes through all of them.
    let loaded = build_sp_load(&mut b, &sp, 4)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    detect(&mut fg)?;

    let arg0_nodes = fg.side_tables().arg_index_to_values(0);
    assert!(
        !arg0_nodes.is_empty(),
        "10k disjoint stores must not mark the chain dirty: load at sp+4 should be registered as arg 0"
    );
    Ok(())
}

/// A `CallOther` reaches the memory chain only when its ABI says it clobbers
/// memory, and once there it blocks under either setting of
/// `assume_incoming_args_survive_calls`: the callee is opaque, so nothing can
/// be inferred from its arguments. With the claim granted, the relaxation
/// speaks for a conforming callee's frame discipline, which a syscall handed
/// a pointer into the incoming-argument area lacks; at the floor the
/// `CallOther` arm answers as a plain `Call` does.
#[test]
fn call_other_on_the_chain_blocks_under_either_survival_setting() -> Result<()> {
    for claimed in [true, false] {
        let sp = sp32_vn();
        let mut b = sp_frame(sp)
            .stack_args(stack_args_at(0, 8))
            .build_fn_single_region()?;
        // Slot 0's address is sp itself, the user-op's sole value-arg.
        let sp_val = b.read_variable(&sp)?;
        let (call_node, _outputs) = strider_ir_test_utils::Tb::named_call_other(
            &mut b,
            42,
            "escape_helper",
            &[sp_val],
            &[],
            true,
            false,
        )?;
        let call_mem_value = b.function().memory_output_of(call_node)?;
        b.advance_cur_region_memory(call_mem_value)?;
        let loaded = b.build_load(sp_val, rsleigh::VnSpace::RAM, ValueType::int(32))?;
        b.build_return(Some(loaded), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        fg.assume(&crate::AssumptionOptions {
            assume_incoming_args_survive_calls: claimed,
            ..crate::AssumptionOptions::none()
        });
        detect(&mut fg)?;
        assert!(
            fg.side_tables().arg_index_to_values(0).is_empty(),
            "survival claimed {claimed}: a CallOther on the chain marks the slot dirty",
        );
    }
    Ok(())
}

/// The clobber toggle for a plain `Call`: under the survival claim a call does
/// not on its own shadow a stack-arg slot, so the load is registered; with it
/// off any call on the chain marks the slot dirty.
#[test]
fn incoming_args_survive_calls_toggle_gates_arg_across_call() -> Result<()> {
    for claimed in [true, false] {
        let sp = sp32_vn();
        let mut b = sp_frame(sp)
            .stack_args(stack_args_at(4, 8))
            .build_fn_single_region()?;
        // The Call's only value input is its constant target, which is not
        // SP-rooted, so the toggle alone governs the verdict.
        let target = b.build_int_const(0x1000u64, ValueType::int(32))?;
        b.build_call_cc(target, None)?;
        let loaded = build_sp_load(&mut b, &sp, 4)?;
        b.build_return(Some(loaded), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        fg.assume(&crate::AssumptionOptions {
            assume_incoming_args_survive_calls: claimed,
            ..crate::AssumptionOptions::none()
        });
        detect(&mut fg)?;
        let want = if claimed { vec![loaded] } else { vec![] };
        assert_eq!(
            fg.side_tables().arg_index_to_values(0),
            want,
            "survival claimed {claimed}: Load[sp+4] across a plain Call"
        );
    }
    Ok(())
}

/// `assume_incoming_args_survive_calls` governs which loads count as incoming
/// arguments, not what the memory edges say. A load's narrowed memory input is
/// read afterwards by walks that honour no call relaxation, so narrowing it
/// past a `Call` would hand them one.
#[test]
fn arg_detection_does_not_narrow_a_load_past_a_call() -> Result<()> {
    let sp = sp32_vn();
    let mut b = sp_frame(sp)
        .stack_args(stack_args_at(4, 8))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    let addr = b.build_const_binop(4u64, sp_val, IntBinaryOp::Add, ValueType::int(32))?;
    let data = b.build_int_const(0x42u64, ValueType::int(32))?;
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    let target = b.build_int_const(0x1000u64, ValueType::int(32))?;
    b.build_call_cc(target, None)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(32))?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // Granted, so detection's policy steps the `Call` and only the narrowing,
    // honouring no call relaxation, keeps the edge where it is.
    fg.assume(&crate::AssumptionOptions {
        assume_incoming_args_survive_calls: true,
        ..crate::AssumptionOptions::none()
    });
    detect(&mut fg)?;

    let mem_in = fg.node_inputs(fg.producer(loaded))[0];
    let kind = fg.node_kind(fg.producer(mem_in));
    assert!(
        matches!(kind, NodeKind::Call { .. }),
        "the load's memory predecessor must still be the Call, got {kind:?}"
    );
    Ok(())
}

/// The stack-arg half of `rerunning_pass_is_idempotent_no_duplicate_carriers`:
/// index 0 there is a register arg the lifter recorded, which this pass never
/// writes, so only a stack ordinal can catch an appending re-run.
#[test]
fn rerunning_the_default_pipeline_keeps_one_carrier_per_stack_arg() -> Result<()> {
    let sp = sp32_vn();
    let mut b = sp_frame(sp)
        .stack_args(stack_args_at(4, 8))
        .build_fn_single_region()?;
    {
        let loaded = build_sp_load(&mut b, &sp, 4)?;
        b.build_return(Some(loaded), &[])?;
    }
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let pipeline = crate::default_pipeline();
    let mut previous: Option<Vec<ValueId>> = None;
    for run in 1..=3 {
        pipeline.run(&mut fg, &crate::OptCtx::new(None))?;
        let carriers = fg.side_tables().arg_index_to_values(0).to_vec();
        assert_eq!(
            carriers.len(),
            1,
            "run {run}: one Load at sp+4, one carrier"
        );
        if let Some(previous) = &previous {
            assert_eq!(*previous, carriers, "run {run}: carrier set changed");
        }
        previous = Some(carriers);
    }
    Ok(())
}

/// Return-target verification passes a constant-address store under
/// `stack_global_disjoint` whatever its own claim, so the narrowing may step
/// past one under that claim, and at the floor leaves it on the chain.
///
/// Mutations: leave the edge where it is; honour `stack_global_disjoint`
/// whatever the function claims.
#[test]
fn narrowing_honours_stack_global_disjoint() -> Result<()> {
    for claimed in [false, true] {
        let sp = stack_vn();
        let mut b = sp_frame(sp)
            .stack_args(stack_args_at(8, 8))
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let slot = b.build_const_binop(8u64, sp_val, IntBinaryOp::Add, ValueType::int(64))?;
        let global = b.build_int_const(0x4000u64, ValueType::int(64))?;
        let val = b.build_int_const(7u64, ValueType::int(64))?;
        b.build_store(global, val, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(slot, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        b.build_return(Some(loaded), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        run_passes(&mut fg, CF_RP)?;

        fg.assume(&crate::AssumptionOptions {
            stack_global_disjoint: claimed,
            ..crate::AssumptionOptions::none()
        });
        crate::pipeline::run_post(&FunctionArgDetect, &mut fg, &crate::OptCtx::new(None))?;

        let mem = fg.node_inputs(fg.producer(loaded))[0];
        let kind = fg.node_kind(fg.producer(mem));
        if claimed {
            assert!(
                matches!(kind, NodeKind::InitialMemory),
                "claimed: the edge must step past the global store, got {kind:?}"
            );
        } else {
            assert!(
                matches!(kind, NodeKind::Store(_)),
                "at the floor the global store must stay on the chain, got {kind:?}"
            );
        }
    }
    Ok(())
}

/// Doubling the calls between argument reads may only double the detection
/// work.
#[test]
fn detecting_across_call_diamonds_costs_near_linear_work() -> Result<()> {
    let work = |n: usize| -> Result<u64> {
        let mut fg = crate::test_support::memory_shapes::argument_reads_between_calls(n)?;
        crate::mem_analysis::WALK_STEPS.with(|c| c.set(0));
        crate::mem_ssa::CLIMB_STEPS.with(|c| c.set(0));
        crate::pipeline::run_post(&FunctionArgDetect, &mut fg, &crate::OptCtx::new(None))?;
        Ok(crate::mem_analysis::WALK_STEPS.with(std::cell::Cell::get)
            + crate::mem_ssa::CLIMB_STEPS.with(std::cell::Cell::get))
    };
    let (small, big) = (work(100)?, work(200)?);
    assert!(
        big * 100 <= small * 260,
        "doubling the call diamonds took {small} steps to {big}"
    );
    Ok(())
}

/// `noalias_allocators` puts a store through the allocation off the argument
/// slot, so the narrowing steps past it, but not past the call, which only
/// detection's own probe steps through, under
/// `assume_incoming_args_survive_calls`.
///
/// Mutations: leave the edge where it is; narrow under
/// `Policy::incoming_args`.
#[test]
fn arg_detection_narrows_past_a_heap_store_to_its_allocator() -> Result<()> {
    const ALLOCATOR: u64 = 0x1000;
    let sp = sp32_vn();
    // Without a return register the call has no fresh heap base.
    let ret_reg = reg_vn(0x00, 4);
    let mut b = sp_frame(sp)
        .tracked(ret_reg)
        .ret(ret_reg)
        .stack_args(stack_args_at(4, 8))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    // A frame below the arguments, so the call's own window misses them.
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
    // Off the entry SP, which `sp` no longer reads back.
    let addr = b.build_const_binop(4u64, sp_val, IntBinaryOp::Add, ValueType::int(32))?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(32))?;
    // Keeps the allocator's pointer live.
    let sum =
        b.build_int_binary_operation(loaded, rets[0], IntBinaryOp::Add, ValueType::int(32))?;
    b.build_return(Some(sum), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    fg.assume(&crate::AssumptionOptions {
        noalias_allocators: [ALLOCATOR].into_iter().collect(),
        assume_incoming_args_survive_calls: true,
        ..crate::AssumptionOptions::none()
    });
    detect(&mut fg)?;

    assert_eq!(fg.side_tables().arg_index_to_values(0), [loaded]);
    let mem_in = fg.node_inputs(fg.producer(loaded))[0];
    let kind = fg.node_kind(fg.producer(mem_in));
    assert!(
        matches!(kind, NodeKind::Call { .. }),
        "the load's memory predecessor must be the allocator Call, got {kind:?}"
    );
    Ok(())
}

/// A `Load` outside RAM (ppc `mfsrin` reads the register space) at an
/// SP-relative address reads no stack slot: it neither is an argument nor
/// hides the RAM load sharing its slot.
///
/// Mutation: take a candidate in any space.
#[test]
fn a_load_outside_ram_is_no_stack_arg() -> Result<()> {
    let sp = sp32_vn();
    let i32_ty = ValueType::int(32);
    let mut b = sp_frame(sp)
        .stack_args(stack_args_at(4, 4))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    let slot0 = b.build_const_binop(4u64, sp_val, IntBinaryOp::Add, i32_ty)?;
    let slot1 = b.build_const_binop(8u64, sp_val, IntBinaryOp::Add, i32_ty)?;
    let ram = b.build_load(slot0, rsleigh::VnSpace::RAM, i32_ty)?;
    let reg0 = b.build_load(slot0, rsleigh::VnSpace::REGISTER, i32_ty)?;
    let reg1 = b.build_load(slot1, rsleigh::VnSpace::REGISTER, i32_ty)?;
    let sum = b.build_int_binary_operation(ram, reg0, IntBinaryOp::Add, i32_ty)?;
    let sum = b.build_int_binary_operation(sum, reg1, IntBinaryOp::Add, i32_ty)?;
    b.build_return(Some(sum), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    detect(&mut fg)?;

    let arg0 = fg.side_tables().arg_index_to_values(0);
    assert_eq!(arg0.len(), 1, "{arg0:?}");
    assert!(matches!(
        fg.node_kind(fg.producer(arg0[0])),
        NodeKind::Load(rsleigh::VnSpace::RAM)
    ));
    assert!(fg.side_tables().arg_index_to_values(1).is_empty());
    Ok(())
}
