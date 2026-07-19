use super::*;
use crate::error::Result;
use crate::{ConstantFold, OptimizerPipeline, PhiCollapse, RegionCollapse};
use strider_ir::node::{NodeKind, ValueType};
use strider_ir::{IRBuilderExt, IRViewer, IRWalker, IntBinaryOp};
use strider_ir_test_utils::IrBuilderEx;
use strider_ir_test_utils::IrWalkerEx;
use strider_ir_test_utils::{
    RegisterSet, SENTINEL_LIFT_ADDR, stack_vn_aarch64 as sp64_vn, stack_vn_x86 as sp32_vn,
};
use strider_target::Endianness;

/// Counts anonymous (Vn-untagged) `Phi` nodes, excluding the Vn-tagged phis
/// the lifter emits for register-aliased reads.  `LoadForward` must never
/// create one of these.
fn reachable_anonymous_phi_count(function: &strider_ir::Function) -> usize {
    let reachable: entity_utils::DenseEntitySet<strider_ir::node::NodeId> =
        function.walk().collect();
    function
        .graph()
        .all_node_ids()
        .filter(|n| {
            reachable.contains(*n)
                && matches!(function.node_kind(*n), NodeKind::Phi)
                && function
                    .get_vn_for_value(function.node_outputs(*n)[0])
                    .is_none()
        })
        .count()
}

/// Pins the memory-SSA walk as iterative: a recursive walk would overflow
/// the default 8 MB stack at this chain depth.
#[test]
fn forward_through_long_chain_of_disjoint_stack_stores() -> Result<()> {
    const CHAIN_LEN: usize = 10_000;

    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        // The value to forward.
        let zero = b.build_int_const(0u64, ValueType::I32)?;
        let target_addr =
            b.build_int_binary_operation(sp_val, zero, IntBinaryOp::Add, ValueType::I32)?;
        let target_val = b.build_int_const(0x99u64, ValueType::I32)?;
        b.build_store(target_addr, target_val, rsleigh::VnSpace::RAM)?;

        // Disjoint stores the walk must step past to reach the target.
        for i in 1..=CHAIN_LEN {
            let off = b.build_int_const(((i * 4) as u64) + 8, ValueType::I32)?;
            let addr =
                b.build_int_binary_operation(sp_val, off, IntBinaryOp::Add, ValueType::I32)?;
            let val = b.build_int_const(i as u64, ValueType::I32)?;
            b.build_store(addr, val, rsleigh::VnSpace::RAM)?;
        }

        let loaded = b.build_load(target_addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let pipeline = crate::test_support::standard_test();
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 0,
        "Load at sp+0 must forward past all {CHAIN_LEN} disjoint stack stores"
    );
    Ok(())
}

/// A load with nothing to forward still gets its memory edge narrowed past
/// the proven-disjoint stores onto `InitialMemory`.
#[test]
fn non_forwardable_load_is_narrowed_to_initial_memory() -> Result<()> {
    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        for off in [8u64, 16u64] {
            let o = b.build_int_const(off, ValueType::I32)?;
            let addr = b.build_int_binary_operation(sp_val, o, IntBinaryOp::Add, ValueType::I32)?;
            let v = b.build_int_const(0xA0u64 + off, ValueType::I32)?;
            b.build_store(addr, v, rsleigh::VnSpace::RAM)?;
        }
        let zero = b.build_int_const(0u64, ValueType::I32)?;
        let load_addr =
            b.build_int_binary_operation(sp_val, zero, IntBinaryOp::Add, ValueType::I32)?;
        let loaded = b.build_load(load_addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let pipeline = crate::test_support::standard_test();
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let load = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
        .expect("the sp+0 load is not forwardable and must survive");
    let mem = fg.node_inputs(load)[0];
    assert!(
        matches!(fg.node_kind(fg.producer(mem)), NodeKind::InitialMemory),
        "non-forwardable load narrowed onto InitialMemory, got {:?}",
        fg.node_kind(fg.producer(mem)),
    );
    Ok(())
}

/// Same address, different `VnSpace`: distinct spaces never alias, so the RAM
/// load must not take the REGISTER store's value.
#[test]
fn forward_does_not_cross_address_spaces() -> Result<()> {
    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let eight = b.build_int_const(8u64, ValueType::I32)?;
        let addr = b.build_int_binary_operation(sp_val, eight, IntBinaryOp::Add, ValueType::I32)?;
        let v = b.build_int_const(0x11u64, ValueType::I32)?;
        b.build_store(addr, v, rsleigh::VnSpace::REGISTER)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let pipeline = crate::test_support::standard_test();
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let load = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
        .expect("RAM load must survive: a REGISTER store cannot forward into it");
    let mem = fg.node_inputs(load)[0];
    assert!(
        matches!(fg.node_kind(fg.producer(mem)), NodeKind::InitialMemory),
        "RAM load must narrow onto InitialMemory, not the REGISTER store; got {:?}",
        fg.node_kind(fg.producer(mem)),
    );
    Ok(())
}

/// Two stores at the same offset: the forwarder must take the live (nearest)
/// one, not the shadowed earlier one.
#[test]
fn forward_takes_nearest_of_two_same_offset_stores() -> Result<()> {
    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let eight = b.build_int_const(8u64, ValueType::I32)?;
        let addr = b.build_int_binary_operation(sp_val, eight, IntBinaryOp::Add, ValueType::I32)?;
        let v = b.build_int_const(0x11u64, ValueType::I32)?;
        let w = b.build_int_const(0x22u64, ValueType::I32)?;
        b.build_store(addr, v, rsleigh::VnSpace::RAM)?;
        b.build_store(addr, w, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let pipeline = crate::test_support::standard_test();
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 0,
        "Load[sp+8] must forward the nearest store"
    );
    let ret_val = crate::test_support::return_value(fg.graph())?;
    assert!(
        fg.int_const_u128(ret_val) == Some(0x22),
        "forwarded value must be the NEAREST store's 0x22, got {:?}",
        fg.int_const_u128(ret_val),
    );
    Ok(())
}

#[test]
fn forward_load_after_matching_store_returns_stored_value() -> Result<()> {
    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let four = b.build_int_const(4u64, ValueType::I32)?;
        let addr = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I32)?;
        let data = b.build_int_const(0x11u64, ValueType::I32)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let pipeline = crate::test_support::standard_test();
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(reachable_loads, 0, "Load[sp+4] should be forwarded away");
    Ok(())
}

/// A store at `InitialVar(sp) + 8` and a load at `(sp & -16) + 8` share the
/// offset but not the base: the aligned base differs from initial SP by
/// `sp mod 16`, which is caller-dependent, so these are different memory.
/// Comparing offset alone would wrongly forward.
#[test]
fn does_not_forward_across_distinct_sp_bases_at_equal_offset() -> Result<()> {
    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let eight = b.build_int_const(8u64, ValueType::I32)?;
        let store_addr =
            b.build_int_binary_operation(sp_val, eight, IntBinaryOp::Add, ValueType::I32)?;
        let data = b.build_int_const(0x11u64, ValueType::I32)?;
        b.build_store(store_addr, data, rsleigh::VnSpace::RAM)?;
        // `and rsp, -16`, then + 8.
        let mask = b.build_int_const(0xFFFF_FFF0u64, ValueType::I32)?;
        let aligned =
            b.build_int_binary_operation(sp_val, mask, IntBinaryOp::And, ValueType::I32)?;
        let load_addr =
            b.build_int_binary_operation(aligned, eight, IntBinaryOp::Add, ValueType::I32)?;
        let loaded = b.build_load(load_addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let pipeline = crate::test_support::standard_test();
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 1,
        "load at (sp & -16) + 8 must NOT be forwarded from a store at sp + 8 — \
         different SP bases are different memory",
    );
    Ok(())
}

/// The walker must step past an intervening non-aliasing store.
#[test]
fn forward_skips_non_aliasing_store() -> Result<()> {
    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let four = b.build_int_const(4u64, ValueType::I32)?;
        let twelve = b.build_int_const(12u64, ValueType::I32)?;
        let addr4 = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I32)?;
        let addr12 =
            b.build_int_binary_operation(sp_val, twelve, IntBinaryOp::Add, ValueType::I32)?;
        let a = b.build_int_const(0xAAu64, ValueType::I32)?;
        let b_val = b.build_int_const(0xBBu64, ValueType::I32)?;
        // Chain from the load: store12 -> store4 -> InitialMemory.
        b.build_store(addr4, b_val, rsleigh::VnSpace::RAM)?;
        b.build_store(addr12, a, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let pipeline = crate::test_support::standard_test();
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 0,
        "Load[sp+4] should forward past the non-aliasing Store(sp+12)"
    );
    Ok(())
}

/// The store covers `[0, 8)`, intersecting but not matching the load's
/// `[4, 8)`, so forwarding must bail.
#[test]
fn bail_on_overlapping_store() -> Result<()> {
    let sp = sp64_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let four = b.build_int_const(4u64, ValueType::I64)?;
        let addr4 = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I64)?;
        let wide_data = b.build_int_const(0xDEAD_BEEF_CAFE_BABEu64, ValueType::I64)?;
        b.build_store(sp_val, wide_data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let pipeline = crate::test_support::standard_test();
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 1,
        "overlapping store must prevent forwarding"
    );
    Ok(())
}

/// Store strictly inside the load: `[2, 6)` within `[0, 8)`.  The stored
/// bytes don't fully back the load, so forwarding must bail.
#[test]
fn bail_on_narrower_store_inside_wider_load_range() -> Result<()> {
    let sp = sp64_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let two = b.build_int_const(2u64, ValueType::I64)?;
        let addr2 = b.build_int_binary_operation(sp_val, two, IntBinaryOp::Add, ValueType::I64)?;
        let narrow = b.build_int_const(0x1122_3344u64, ValueType::I32)?;
        b.build_store(addr2, narrow, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(sp_val, rsleigh::VnSpace::RAM, ValueType::I64)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let pipeline = crate::test_support::standard_test();
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 1,
        "a 4-byte store at sp+2 cannot back an 8-byte load at sp+0 — no forward",
    );
    Ok(())
}

/// A narrower same-address store between a wide store and a wide load is the
/// nearest memory def yet only partially backs the load, so neither value
/// forwards.
#[test]
fn bail_on_narrower_same_address_store_between_wide_store_and_load() -> Result<()> {
    let sp = sp64_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let wide = b.build_int_const(0xAAAA_BBBB_CCCC_DDDDu64, ValueType::I64)?;
        b.build_store(sp_val, wide, rsleigh::VnSpace::RAM)?;
        let narrow = b.build_int_const(0x1234u64, ValueType::I32)?;
        b.build_store(sp_val, narrow, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(sp_val, rsleigh::VnSpace::RAM, ValueType::I64)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let pipeline = crate::test_support::standard_test();
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 1,
        "the intervening narrower same-address store must block forwarding \
         of both stores' values",
    );
    Ok(())
}

/// Offsets agree but widths differ, so the stored bytes don't fully back the
/// load.
#[test]
fn bail_on_type_mismatch() -> Result<()> {
    let sp = sp64_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let narrow = b.build_int_const(0x11u64, ValueType::I32)?;
        b.build_store(sp_val, narrow, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(sp_val, rsleigh::VnSpace::RAM, ValueType::I64)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let pipeline = crate::test_support::standard_test();
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(reachable_loads, 1, "type mismatch must prevent forwarding");
    Ok(())
}

/// Soundness floor: a non-SP-rooted store cannot be proven disjoint from an
/// SP-rooted load (its constant address could equal `sp + K`, or it could be
/// an escaped SP-derived pointer).  Under `Strict` the walker must bail.
#[test]
fn strict_does_not_forward_across_non_sp_intervening_store() -> Result<()> {
    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let four = b.build_int_const(4u64, ValueType::I32)?;
        let addr4 = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I32)?;
        let a = b.build_int_const(0xAAu64, ValueType::I32)?;
        b.build_store(addr4, a, rsleigh::VnSpace::RAM)?;
        // Cross-class against the SP-rooted load; not provably disjoint
        // without `AliasMode::StackGlobalDisjoint`.
        let heap_addr = b.build_int_const(0x1000u64, ValueType::I32)?;
        let other = b.build_int_const(0xBBu64, ValueType::I32)?;
        b.build_store(heap_addr, other, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    // Strict is pinned explicitly: the default `StackGlobalDisjoint` assumes
    // the const-addressed store disjoint and forwards instead (covered by
    // `permissive_forwards_across_const_intervening_store`).
    let pipeline = crate::test_support::standard_test();
    pipeline.run(&mut fg, &mut crate::test_support::octx_strict())?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 1,
        "Strict mode: the cross-class intervening Store must block forwarding; \
         the Load[sp+4] must remain in the graph"
    );
    Ok(())
}

/// Under `StackGlobalDisjoint` the const-addressed store is assumed to live
/// outside the stack region, so the walker steps through it.
#[test]
fn permissive_forwards_across_const_intervening_store() -> Result<()> {
    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let four = b.build_int_const(4u64, ValueType::I32)?;
        let addr4 = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I32)?;
        let a = b.build_int_const(0xAAu64, ValueType::I32)?;
        b.build_store(addr4, a, rsleigh::VnSpace::RAM)?;
        let heap_addr = b.build_int_const(0x1000u64, ValueType::I32)?;
        let other = b.build_int_const(0xBBu64, ValueType::I32)?;
        b.build_store(heap_addr, other, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let pipeline = crate::test_support::standard_test();
    pipeline.run(&mut fg, &mut crate::test_support::octx_permissive())?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 0,
        "Permissive mode: the IntConst-addressed Store cannot alias \
         sp+4, so the Load[sp+4] must forward to 0xAA"
    );
    let ret_val = crate::test_support::return_value(fg.graph())?;
    assert!(
        fg.int_const_u128(ret_val) == Some(0xAA),
        "forwarded value must be IntConst(0xAA), got {:?}",
        fg.int_const_u128(ret_val)
    );
    Ok(())
}

/// Even under `StackGlobalDisjoint`, an Anchor address (neither SP-rooted nor
/// an `IntConst`) still bails; closing that gap needs escape analysis we do
/// not have.
#[test]
fn permissive_still_bails_on_anchor_intervening_store() -> Result<()> {
    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let four = b.build_int_const(4u64, ValueType::I32)?;
        let addr4 = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I32)?;
        let a = b.build_int_const(0xAAu64, ValueType::I32)?;
        b.build_store(addr4, a, rsleigh::VnSpace::RAM)?;
        // Anchor address: a loaded global used as the store address.
        let global_addr = b.build_int_const(0x2000u64, ValueType::I32)?;
        let p = b.build_load(global_addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
        let other = b.build_int_const(0xBBu64, ValueType::I32)?;
        b.build_store(p, other, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let pipeline = crate::test_support::standard_test();
    pipeline.run(&mut fg, &mut crate::test_support::octx_permissive())?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert!(
        reachable_loads >= 1,
        "Permissive mode: an Anchor (non-IntConst non-SP) intervening \
         Store must still block forwarding; expected ≥1 Load remaining, \
         got {reachable_loads}"
    );
    Ok(())
}

/// Const-address load: matched by `IntConst` equality, with the intervening
/// const-address store proven disjoint via `ranges_disjoint`.  Forwards under
/// both alias modes.
#[test]
fn forwards_constant_address_load_across_disjoint_const_store() -> Result<()> {
    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, _sp_val| {
        let addr1 = b.build_int_const(0x1000u64, ValueType::I32)?;
        let addr2 = b.build_int_const(0x2000u64, ValueType::I32)?;
        let data_a = b.build_int_const(0xAAu64, ValueType::I32)?;
        let data_b = b.build_int_const(0xBBu64, ValueType::I32)?;
        b.build_store(addr1, data_a, rsleigh::VnSpace::RAM)?;
        b.build_store(addr2, data_b, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr1, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let pipeline = crate::test_support::standard_test();
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 0,
        "Constant-address Load must forward across a disjoint constant-address \
         intervening Store; the matching store provides 0xAA"
    );
    let ret_val = crate::test_support::return_value(fg.graph())?;
    assert!(
        fg.int_const_u128(ret_val) == Some(0xAA),
        "forwarded value must be IntConst(0xAA), got {:?}",
        fg.int_const_u128(ret_val)
    );
    Ok(())
}

/// Load and store match by `ValueId` equality on the address slot.  Forwards
/// under both alias modes.
#[test]
fn forwards_anchor_load_with_same_id_store_no_interferer() -> Result<()> {
    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, _sp_val| {
        let table_base = b.build_int_const(0x100u64, ValueType::I32)?;
        let p = b.build_load(table_base, rsleigh::VnSpace::RAM, ValueType::I32)?;
        let data = b.build_int_const(0xCCu64, ValueType::I32)?;
        b.build_store(p, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(p, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let pipeline = crate::test_support::standard_test();
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    // Only the `p = Load(IntConst(0x100))` address-producer may remain.
    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 1,
        "Anchor-address Load with same-ValueId Store and no interferer \
         must forward; only the address-producer Load(IntConst(0x100)) survives"
    );
    let ret_val = crate::test_support::return_value(fg.graph())?;
    assert!(
        fg.int_const_u128(ret_val) == Some(0xCC),
        "forwarded value must be IntConst(0xCC), got {:?}",
        fg.int_const_u128(ret_val)
    );
    Ok(())
}

/// The matching `Store(p, ...)` exists upstream, but the intervening
/// `Store(q, ...)` has a different `ValueId` and `q != p` is unprovable at
/// runtime, so forwarding must bail.
#[test]
fn does_not_forward_anchor_load_across_different_anchor_interferer() -> Result<()> {
    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, _sp_val| {
        let base1 = b.build_int_const(0x100u64, ValueType::I32)?;
        let base2 = b.build_int_const(0x200u64, ValueType::I32)?;
        let p = b.build_load(base1, rsleigh::VnSpace::RAM, ValueType::I32)?;
        let q = b.build_load(base2, rsleigh::VnSpace::RAM, ValueType::I32)?;
        let data_c = b.build_int_const(0xCCu64, ValueType::I32)?;
        let data_d = b.build_int_const(0xDDu64, ValueType::I32)?;
        b.build_store(p, data_c, rsleigh::VnSpace::RAM)?;
        b.build_store(q, data_d, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(p, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let pipeline = crate::test_support::standard_test();
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert!(
        reachable_loads >= 3,
        "Anchor-address Load with different-ValueId Anchor interferer \
         must NOT forward; expected ≥3 Load nodes (2 address-producers + \
         the unforwarded Load(p)), got {reachable_loads}"
    );
    Ok(())
}

/// A call clobbers memory, so forwarding across it is unsafe.  Uses a
/// link-register-style convention (ret_stack_pop=0) so SP stays stable and
/// the load's address remains decomposable.
#[test]
fn bail_on_call_between() -> Result<()> {
    let sp = sp64_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .build_fn_single_region()?;

    let sp_val = b.read_variable(&sp)?;
    let four = b.build_int_const(4u64, ValueType::I64)?;
    let addr4 = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I64)?;
    let data = b.build_int_const(0x11u64, ValueType::I32)?;
    b.build_store(addr4, data, rsleigh::VnSpace::RAM)?;
    let target = b.build_int_const(0x1000u64, ValueType::I64)?;
    b.build_call_cc(target, None)?;
    // SP did not shift, so sp+4 is still the same slot.
    let sp_val2 = b.read_variable(&sp)?;
    let addr4b = b.build_int_binary_operation(sp_val2, four, IntBinaryOp::Add, ValueType::I64)?;
    let loaded = b.build_load(addr4b, rsleigh::VnSpace::RAM, ValueType::I32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let pipeline = crate::test_support::standard_test();
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 1,
        "Call on memory chain must prevent forwarding"
    );
    Ok(())
}

/// Each arm stores a distinct constant at `sp+4`, leaving a non-trivial
/// `MemPhi` at the merge.  No single stored value backs the load across it,
/// so the forward bails: the load survives and no value-`Phi` is created.
#[test]
fn per_branch_stores_same_offset_do_not_forward_and_synthesize_no_phi() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .build_fn()?;
    let entry = b.create_region_all()?;
    let then_r = b.create_region_all()?;
    let else_r = b.create_region_all()?;
    let merge = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    // if const(true) { *(sp+4) = 0xAA } else { *(sp+4) = 0xBB }; return *(sp+4)
    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, then_r, else_r)?;

    b.set_region(then_r);
    let sp_t = b.read_variable(&sp)?;
    let four_t = b.build_int_const(4u64, ValueType::I32)?;
    let addr_t = b.build_int_binary_operation(sp_t, four_t, IntBinaryOp::Add, ValueType::I32)?;
    let a = b.build_int_const(0xAAu64, ValueType::I32)?;
    b.build_store(addr_t, a, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    b.set_region(else_r);
    let sp_e = b.read_variable(&sp)?;
    let four_e = b.build_int_const(4u64, ValueType::I32)?;
    let addr_e = b.build_int_binary_operation(sp_e, four_e, IntBinaryOp::Add, ValueType::I32)?;
    let bval = b.build_int_const(0xBBu64, ValueType::I32)?;
    b.build_store(addr_e, bval, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    b.set_region(merge);
    let sp_m = b.read_variable(&sp)?;
    let four_m = b.build_int_const(4u64, ValueType::I32)?;
    let addr_m = b.build_int_binary_operation(sp_m, four_m, IntBinaryOp::Add, ValueType::I32)?;
    let loaded = b.build_load(addr_m, rsleigh::VnSpace::RAM, ValueType::I32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let phis_before = reachable_anonymous_phi_count(&fg);

    // No DeadBranchElimination here, so the `If(const true)` diamond (and its
    // MemPhi) survives instead of collapsing.
    let pipeline = crate::test_support::standard_test();
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 1,
        "per-branch stores leave a surviving MemPhi boundary; the load must NOT forward",
    );
    let phis_after = reachable_anonymous_phi_count(&fg);
    assert_eq!(
        phis_after, phis_before,
        "no value-Phi may be synthesized across the MemPhi boundary",
    );
    assert_eq!(phis_after, 0, "load_forward is phi-free");
    Ok(())
}

/// Same contract as the 2-way diamond, but a nested `If` gives the merge
/// three predecessors: the 3-input `MemPhi` is still an opaque boundary.
#[test]
fn three_predecessor_memphi_blocks_forwarding_no_phi() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .build_fn()?;
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

    // Each arm stores a distinct constant at sp+4.
    for (region, val) in [(arm_a, 0xAAu64), (arm_b, 0xBB), (arm_c, 0xCC)] {
        b.set_region(region);
        let sp_v = b.read_variable(&sp)?;
        let four = b.build_int_const(4u64, ValueType::I32)?;
        let addr = b.build_int_binary_operation(sp_v, four, IntBinaryOp::Add, ValueType::I32)?;
        let c = b.build_int_const(val, ValueType::I32)?;
        b.build_store(addr, c, rsleigh::VnSpace::RAM)?;
        b.build_branch(merge)?;
    }

    b.set_region(merge);
    let sp_m = b.read_variable(&sp)?;
    let four_m = b.build_int_const(4u64, ValueType::I32)?;
    let addr_m = b.build_int_binary_operation(sp_m, four_m, IntBinaryOp::Add, ValueType::I32)?;
    let loaded = b.build_load(addr_m, rsleigh::VnSpace::RAM, ValueType::I32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let phis_before = reachable_anonymous_phi_count(&fg);

    let pipeline = crate::test_support::standard_test();
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 1,
        "a 3-predecessor MemPhi is an opaque boundary; the load must NOT forward",
    );
    let phis_after = reachable_anonymous_phi_count(&fg);
    assert_eq!(
        phis_after, phis_before,
        "no value-Phi may be synthesized across the 3-way MemPhi boundary",
    );
    Ok(())
}

/// A store dominating an `If` with empty branches, loaded after the join.
/// Both arms carry the identical memory token, so `PhiCollapse` removes the
/// trivial `MemPhi`, the chain becomes linear, and the walk reaches the
/// dominating store without synthesizing a value-`Phi`.
#[test]
fn dominating_store_across_collapsible_merge_forwards_with_no_phi() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .build_fn()?;
    let entry = b.create_region_all()?;
    let then_r = b.create_region_all()?;
    let else_r = b.create_region_all()?;
    let merge = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    b.set_region(entry);
    let sp_en = b.read_variable(&sp)?;
    let four_en = b.build_int_const(4u64, ValueType::I32)?;
    let addr_en = b.build_int_binary_operation(sp_en, four_en, IntBinaryOp::Add, ValueType::I32)?;
    let dominating = b.build_int_const(0xABu64, ValueType::I32)?;
    b.build_store(addr_en, dominating, rsleigh::VnSpace::RAM)?;
    let cond = b.build_boolean_const(true);
    b.build_if(cond, then_r, else_r)?;

    // Both arms empty: no memory writes.
    b.set_region(then_r);
    b.build_branch(merge)?;
    b.set_region(else_r);
    b.build_branch(merge)?;

    b.set_region(merge);
    let sp_m = b.read_variable(&sp)?;
    let four_m = b.build_int_const(4u64, ValueType::I32)?;
    let addr_m = b.build_int_binary_operation(sp_m, four_m, IntBinaryOp::Add, ValueType::I32)?;
    let loaded = b.build_load(addr_m, rsleigh::VnSpace::RAM, ValueType::I32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let phis_before = reachable_anonymous_phi_count(&fg);

    let pipeline = crate::test_support::standard_test();
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 0,
        "dominating store across a collapsible merge must still forward",
    );
    let ret_val = crate::test_support::return_value(fg.graph())?;
    assert!(
        fg.int_const_u128(ret_val) == Some(0xAB),
        "forwarded value must be the dominating store's 0xAB, got {:?}",
        fg.int_const_u128(ret_val),
    );
    let phis_after = reachable_anonymous_phi_count(&fg);
    assert_eq!(
        phis_after, phis_before,
        "dominating-store forwarding must not synthesize a value-Phi",
    );
    assert_eq!(phis_after, 0, "load_forward is phi-free");
    Ok(())
}

/// Only the then-arm stores at `sp+4`; the else-arm reaches the merge with
/// stale memory, so the whole forward must bail.
#[test]
fn phi_missing_store_on_one_branch_bails() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .build_fn()?;
    let entry = b.create_region_all()?;
    let then_r = b.create_region_all()?;
    let else_r = b.create_region_all()?;
    let merge = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, then_r, else_r)?;

    b.set_region(then_r);
    let sp_t = b.read_variable(&sp)?;
    let four_t = b.build_int_const(4u64, ValueType::I32)?;
    let addr_t = b.build_int_binary_operation(sp_t, four_t, IntBinaryOp::Add, ValueType::I32)?;
    let a = b.build_int_const(0xAAu64, ValueType::I32)?;
    b.build_store(addr_t, a, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    // else: no store, so memory is unchanged.
    b.set_region(else_r);
    b.build_branch(merge)?;

    b.set_region(merge);
    let sp_m = b.read_variable(&sp)?;
    let four_m = b.build_int_const(4u64, ValueType::I32)?;
    let addr_m = b.build_int_binary_operation(sp_m, four_m, IntBinaryOp::Add, ValueType::I32)?;
    let loaded = b.build_load(addr_m, rsleigh::VnSpace::RAM, ValueType::I32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let pipeline = crate::test_support::standard_test();
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 1,
        "missing-store branch must prevent forwarding"
    );
    let reachable_value_phis = reachable_anonymous_phi_count(&fg);
    assert_eq!(
        reachable_value_phis, 0,
        "no ValuePhi should be synthesized when a branch bails"
    );
    Ok(())
}

/// Both arms store at different non-aliasing offsets above a shared
/// `Store(sp+4)` in the entry block, so the merge `MemPhi` has distinct
/// per-predecessor inputs (uncollapsible) yet both walks bottom out on the
/// same store.  The load forwards from it directly, with no value-`Phi`.
#[test]
fn phi_identical_values_no_new_phi() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .build_fn()?;
    let entry = b.create_region_all()?;
    let then_r = b.create_region_all()?;
    let else_r = b.create_region_all()?;
    let merge = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let sp_e = b.read_variable(&sp)?;
    let four_e = b.build_int_const(4u64, ValueType::I32)?;
    let addr_e = b.build_int_binary_operation(sp_e, four_e, IntBinaryOp::Add, ValueType::I32)?;
    let shared = b.build_int_const(0xAAu64, ValueType::I32)?;
    b.build_store(addr_e, shared, rsleigh::VnSpace::RAM)?;
    let cond = b.build_boolean_const(true);
    b.build_if(cond, then_r, else_r)?;

    b.set_region(then_r);
    let sp_t = b.read_variable(&sp)?;
    let eight_t = b.build_int_const(8u64, ValueType::I32)?;
    let addr_t = b.build_int_binary_operation(sp_t, eight_t, IntBinaryOp::Add, ValueType::I32)?;
    let bt = b.build_int_const(0xBBu64, ValueType::I32)?;
    b.build_store(addr_t, bt, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    b.set_region(else_r);
    let sp_l = b.read_variable(&sp)?;
    let twelve_l = b.build_int_const(12u64, ValueType::I32)?;
    let addr_l = b.build_int_binary_operation(sp_l, twelve_l, IntBinaryOp::Add, ValueType::I32)?;
    let cc = b.build_int_const(0xCCu64, ValueType::I32)?;
    b.build_store(addr_l, cc, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    b.set_region(merge);
    let sp_m = b.read_variable(&sp)?;
    let four_m = b.build_int_const(4u64, ValueType::I32)?;
    let addr_m = b.build_int_binary_operation(sp_m, four_m, IntBinaryOp::Add, ValueType::I32)?;
    let loaded = b.build_load(addr_m, rsleigh::VnSpace::RAM, ValueType::I32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let pipeline = crate::test_support::standard_test();
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(reachable_loads, 0, "Load must be forwarded");
    let reachable_value_phis = reachable_anonymous_phi_count(&fg);
    assert_eq!(
        reachable_value_phis, 0,
        "identical branch values must skip the ValuePhi synthesis"
    );
    Ok(())
}

/// Stores via `Sub(sp, 4)` but loads back via `Add(sp, 0xFFFFFFFC)`: both
/// encodings must normalise to offset -4.
#[test]
fn forwarding_bridges_sub_and_add_encodings_of_same_offset() -> Result<()> {
    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let four = b.build_int_const(4u64, ValueType::I32)?;
        let store_addr = b.build_sub_as_add_neg(sp_val, four, ValueType::I32)?;
        let data = b.build_int_const(0x4242u64, ValueType::I32)?;
        b.build_store(store_addr, data, rsleigh::VnSpace::RAM)?;

        let neg_four = b.build_int_const(0xFFFF_FFFCu64, ValueType::I32)?;
        let load_addr =
            b.build_int_binary_operation(sp_val, neg_four, IntBinaryOp::Add, ValueType::I32)?;
        let loaded = b.build_load(load_addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    // `ConstantFold` must run first: LoadForward does not peel the `Neg`
    // itself, it needs both sides already at `Add(sp, IntConst(-4))`.
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold::new());
    pipeline.add(PhiCollapse);
    pipeline.add(RegionCollapse);
    pipeline.add(LoadForward);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 0,
        "Load[Add(sp, 0xFFFFFFFC)] must be forwarded from Store[Sub(sp, 4)]",
    );
    let ret = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
        .expect("return node exists");
    let ret_inputs = fg.node_inputs(ret);
    // Return inputs: [ctrl, mem, val_0, ...].
    let ret_val = ret_inputs[2];
    assert!(
        fg.int_const_u128(ret_val) == Some(0x4242),
        "forwarded value must be the stored constant 0x4242 — got {:?}",
        fg.int_const_u128(ret_val),
    );
    Ok(())
}

/// Real-world `-O0 -m32` shape: the prologue spills 4 bytes to a stack slot
/// and the body reads one byte back from the same offset.  The load is
/// narrower but fully covered, so forwarding emits a `Truncate`.
#[test]
fn narrow_load_from_wider_store_forwards_via_truncate() -> Result<()> {
    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let eight = b.build_int_const(8u64, ValueType::I32)?;
        let addr = b.build_sub_as_add_neg(sp_val, eight, ValueType::I32)?;
        let wide = b.build_int_const(0xDEAD_BEEFu64, ValueType::I32)?;
        b.build_store(addr, wide, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I8)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let pipeline = crate::test_support::standard_test();
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 0,
        "Load u8 at matching offset must be forwarded as the low byte of the u32 store",
    );
    let ret = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
        .expect("return node exists");
    let ret_inputs = fg.node_inputs(ret);
    // `int_const_u128` masks to the output type, so an I8 output reads back as
    // the low byte even if the backing node holds the full u32 pattern.
    let val_ty = fg.value_type_opt(ret_inputs[2]);
    assert_eq!(val_ty, Some(ValueType::I8));
    assert_eq!(
        fg.int_const_u128(ret_inputs[2]),
        Some(0xEF),
        "forwarded narrow load must fold to the low byte 0xEF",
    );
    Ok(())
}

/// The two-byte case, guarded separately from the one-byte case because real
/// binaries emit both.
#[test]
fn narrow_load_u16_from_u32_store_forwards_via_truncate() -> Result<()> {
    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let twelve = b.build_int_const(12u64, ValueType::I32)?;
        let addr = b.build_sub_as_add_neg(sp_val, twelve, ValueType::I32)?;
        let wide = b.build_int_const(0xDEAD_BEEFu64, ValueType::I32)?;
        b.build_store(addr, wide, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I16)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let pipeline = crate::test_support::standard_test();
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(reachable_loads, 0, "Load u16 must be forwarded");
    let ret = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
        .expect("return node exists");
    let ret_inputs = fg.node_inputs(ret);
    let val_ty = fg.value_type_opt(ret_inputs[2]);
    assert_eq!(val_ty, Some(ValueType::I16));
    assert_eq!(
        fg.int_const_u128(ret_inputs[2]),
        Some(0xBEEF),
        "forwarded u16 load must fold to low 16 bits 0xBEEF",
    );
    Ok(())
}

/// On big-endian the load takes the high bytes, so forwarding must shift
/// before truncating rather than emit the LE plain `Truncate`.
///
/// `ConstantFold` is deliberately omitted: it would collapse the chain to a
/// single `IntConst` and the structural assertions would have nothing to see.
#[test]
fn narrow_load_from_wider_store_be_shifts_high_bytes() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .endianness(Endianness::Big)
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    let eight = b.build_int_const(8u64, ValueType::I32)?;
    let addr = b.build_sub_as_add_neg(sp_val, eight, ValueType::I32)?;
    let wide = b.build_int_const(0xDEAD_BEEFu64, ValueType::I32)?;
    b.build_store(addr, wide, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I8)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(PhiCollapse);
    pipeline.add(RegionCollapse);
    pipeline.add(LoadForward);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 0,
        "Load u8 at matching offset must be forwarded as the high byte of the u32 store on BE",
    );

    let ret = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
        .expect("return node exists");
    let ret_inputs = fg.node_inputs(ret);
    // Return inputs: [ctrl, mem, val_0, ...].
    let val_value = ret_inputs[2];
    let val_ty = fg.value_type_opt(val_value);
    assert_eq!(val_ty, Some(ValueType::I8));

    let outer = fg.producer(val_value);
    assert!(
        matches!(fg.node_kind(outer), NodeKind::Truncate),
        "BE narrow forward must wrap data in a Truncate — got {:?}",
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
        "BE narrow forward must shift before truncation — got {:?}",
        fg.node_kind(inner),
    );

    // ShiftRight inputs: [data, shift_const]; here (4 - 1) * 8 = 24.
    let shr_inputs = fg.node_inputs(inner);
    assert_eq!(shr_inputs.len(), 2, "ShiftRight has two inputs");
    let shift_val = shr_inputs[1];
    assert!(
        fg.int_const_u128(shift_val) == Some(24),
        "BE shift amount must be (store_size - load_size) * 8 = 24 — got {:?}",
        fg.int_const_u128(shift_val),
    );
    Ok(())
}

/// One arm stores a wider value at the load offset, the other stores
/// nothing, so the `MemPhi` is a boundary and the forward bails.  Reshape
/// nodes are only built after the exact-match decision is final, so the bail
/// must leave no orphan `Truncate` or value-`Phi` behind.
#[test]
fn aborted_memphi_resolution_creates_no_nodes() -> Result<()> {
    let sp = sp64_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .build_fn()?;
    let entry = b.create_region_all()?;
    let then_r = b.create_region_all()?;
    let else_r = b.create_region_all()?;
    let merge = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, then_r, else_r)?;

    b.set_region(then_r);
    let sp_t = b.read_variable(&sp)?;
    let wide = b.build_int_const(0xDEAD_BEEF_CAFE_BABEu64, ValueType::I64)?;
    b.build_store(sp_t, wide, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    // No store at sp+0 here, so the arms disagree and the forward bails.
    b.set_region(else_r);
    b.build_branch(merge)?;

    b.set_region(merge);
    let sp_m = b.read_variable(&sp)?;
    let loaded = b.build_load(sp_m, rsleigh::VnSpace::RAM, ValueType::I32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // Normalize before measuring the baseline so prep-introduced nodes are
    // not mistaken for a leak.
    let mut prep = OptimizerPipeline::new();
    prep.add(ConstantFold::new());
    prep.add(PhiCollapse);
    prep.add(RegionCollapse);
    prep.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let total_truncate_before = fg
        .graph()
        .all_node_ids()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Truncate))
        .count();
    let total_value_phi_before = fg
        .graph()
        .all_node_ids()
        .filter(|&n| {
            matches!(fg.node_kind(n), NodeKind::Phi)
                && fg.get_vn_for_value(fg.node_outputs(n)[0]).is_none()
        })
        .count();

    // In isolation, so any leaked node is attributable to LoadForward.
    crate::pipeline::run_one(&LoadForward, &mut fg, &mut crate::OptCtx::new(None))?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(reachable_loads, 1, "load must remain — bail expected");

    let total_truncate_after = fg
        .graph()
        .all_node_ids()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Truncate))
        .count();
    let total_value_phi_after = fg
        .graph()
        .all_node_ids()
        .filter(|&n| {
            matches!(fg.node_kind(n), NodeKind::Phi)
                && fg.get_vn_for_value(fg.node_outputs(n)[0]).is_none()
        })
        .count();
    assert_eq!(
        total_truncate_after, total_truncate_before,
        "abort path leaked an orphan Truncate node",
    );
    assert_eq!(
        total_value_phi_after, total_value_phi_before,
        "abort path leaked an orphan ValuePhi node",
    );
    Ok(())
}

/// `load_forward` is phi-free: it may only decrease the graph's `Phi` count,
/// never increase it.  Exercised on the surviving-MemPhi diamond, the shape
/// most likely to tempt a synthesis.
#[test]
fn load_forward_never_increases_phi_count() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .build_fn()?;
    let entry = b.create_region_all()?;
    let then_r = b.create_region_all()?;
    let else_r = b.create_region_all()?;
    let merge = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, then_r, else_r)?;

    b.set_region(then_r);
    let sp_t = b.read_variable(&sp)?;
    let four_t = b.build_int_const(4u64, ValueType::I32)?;
    let addr_t = b.build_int_binary_operation(sp_t, four_t, IntBinaryOp::Add, ValueType::I32)?;
    let a = b.build_int_const(0xAAu64, ValueType::I32)?;
    b.build_store(addr_t, a, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    b.set_region(else_r);
    let sp_e = b.read_variable(&sp)?;
    let four_e = b.build_int_const(4u64, ValueType::I32)?;
    let addr_e = b.build_int_binary_operation(sp_e, four_e, IntBinaryOp::Add, ValueType::I32)?;
    let bval = b.build_int_const(0xBBu64, ValueType::I32)?;
    b.build_store(addr_e, bval, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    b.set_region(merge);
    let sp_m = b.read_variable(&sp)?;
    let four_m = b.build_int_const(4u64, ValueType::I32)?;
    let addr_m = b.build_int_binary_operation(sp_m, four_m, IntBinaryOp::Add, ValueType::I32)?;
    let loaded = b.build_load(addr_m, rsleigh::VnSpace::RAM, ValueType::I32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // Collapse trivial phis first so the baseline is the graph LoadForward
    // actually sees.
    crate::test_support::cf_rp_pipeline().run(&mut fg, &mut crate::OptCtx::new(None))?;

    let total_phis_before = fg
        .graph()
        .all_node_ids()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Phi))
        .count();

    crate::pipeline::run_one(&LoadForward, &mut fg, &mut crate::OptCtx::new(None))?;

    let total_phis_after = fg
        .graph()
        .all_node_ids()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Phi))
        .count();
    assert!(
        total_phis_after <= total_phis_before,
        "load_forward must never INCREASE the Phi count (before={total_phis_before}, \
         after={total_phis_after})",
    );
    Ok(())
}
