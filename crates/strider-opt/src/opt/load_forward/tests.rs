use super::*;
use crate::error::Result;
use crate::pipeline::OptimizerTestExt;
use crate::{ConstantFold, OptimizerPipeline, PhiCollapse, RegionCollapse};
use strider_ir::IRBuilderExt;
use strider_ir::IRWalker;
use strider_ir::IntBinaryOp;
use strider_ir::node::{NodeKind, ValueType};
use strider_ir::IRViewer;
use strider_ir_test_utils::{
    RegisterSet, SENTINEL_LIFT_ADDR, stack_vn_aarch64 as sp64_vn, stack_vn_x86 as sp32_vn,
};
use strider_target::Endianness;

/// Counts reachable anonymous (Vn-untagged) `Phi` nodes — the shape
/// `LoadForward` synthesises when forwarding a load across a
/// `MemPhi`.  Vn-tagged phis (created at lift time for register-aliased
/// reads) are excluded.
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

/// Direct forward: `*(sp+4) = 0x11; return *(sp+4)` — the load vanishes
/// and the return sources from the stored constant.
/// Stress test for the iterative `probe`: a long chain of disjoint
/// SP-relative stores between an early store and a load that targets it.
/// The recursive version this replaces would burn one stack frame
/// per store in the chain — pathological input would
/// stack-overflow.  The iterative worklist must complete on any
/// chain depth.
#[test]
fn forward_through_long_chain_of_disjoint_stack_stores() -> Result<()> {
    // 10k-store chain pins the iterative form of the memory-SSA walk
    // (`mem_ssa::find_nearest_clobber`).  The prior recursive form
    // would stack-overflow on the default 8 MB Rust stack at this
    // depth.  See the deep-chain regression test below for a smaller,
    // deterministic check.
    const CHAIN_LEN: usize = 10_000;

    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        // Store 0x99 at sp+0 first (the value we'll forward to).
        let zero = b.build_int_const(0u64, ValueType::I32)?;
        let target_addr =
            b.build_int_binary_operation(sp_val, zero, IntBinaryOp::Add, ValueType::I32)?;
        let target_val = b.build_int_const(0x99u64, ValueType::I32)?;
        b.build_store(target_addr, target_val, rsleigh::VnSpace::RAM)?;

        // CHAIN_LEN disjoint stores at increasing offsets.  Each is
        // a fresh SP-relative store in the memory chain that the probe must
        // walk past to reach the target store.
        for i in 1..=CHAIN_LEN {
            let off = b.build_int_const(((i * 4) as u64) + 8, ValueType::I32)?;
            let addr =
                b.build_int_binary_operation(sp_val, off, IntBinaryOp::Add, ValueType::I32)?;
            let val = b.build_int_const(i as u64, ValueType::I32)?;
            b.build_store(addr, val, rsleigh::VnSpace::RAM)?;
        }

        // Load from sp+0 — this drives `probe` backward through every
        // store in the chain.  After forwarding, the load and the
        // intermediate stores at irrelevant offsets should still exist
        // (they're side-effecting writes), but the load itself must
        // disappear.
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

/// A load with no matching store cannot forward, but the memory-SSA walker
/// still narrows its memory edge: `Store[sp+8]; Store[sp+16]; Load[sp+0]`
/// → the `sp+0` load survives (clean chain, nothing to forward) with its
/// memory input repointed past both disjoint stores onto `InitialMemory`.
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

/// A store and a load at the SAME address but in DIFFERENT spaces must not
/// forward: `Store[REGISTER, sp+8]=v; Load[RAM, sp+8]`.  Distinct `VnSpace`s
/// never alias, so the RAM load must survive (narrowed to `InitialMemory`),
/// NOT take the REGISTER store's value.
#[test]
fn forward_does_not_cross_address_spaces() -> Result<()> {
    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let eight = b.build_int_const(8u64, ValueType::I32)?;
        let addr = b.build_int_binary_operation(sp_val, eight, IntBinaryOp::Add, ValueType::I32)?;
        let v = b.build_int_const(0x11u64, ValueType::I32)?;
        // Store lives in REGISTER space; the load reads RAM at the same address.
        b.build_store(addr, v, rsleigh::VnSpace::REGISTER)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let pipeline = crate::test_support::standard_test();
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    // The RAM load must NOT have forwarded the REGISTER store — it survives.
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

/// Two stores at the SAME offset, then a load: the forwarder must take
/// the NEAREST store's value (the live one), not the earlier shadowed
/// one.  `Store[sp+8]=v; Store[sp+8]=w; Load[sp+8]` → forwards `w`.
#[test]
fn forward_takes_nearest_of_two_same_offset_stores() -> Result<()> {
    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let eight = b.build_int_const(8u64, ValueType::I32)?;
        let addr = b.build_int_binary_operation(sp_val, eight, IntBinaryOp::Add, ValueType::I32)?;
        let v = b.build_int_const(0x11u64, ValueType::I32)?;
        let w = b.build_int_const(0x22u64, ValueType::I32)?;
        // v first (shadowed), then w (live), then load.
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
        fg.int_const_val(ret_val) == Some(0x22),
        "forwarded value must be the NEAREST store's 0x22, got {:?}",
        fg.int_const_val(ret_val),
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

/// Soundness: a store rooted at `InitialVar(sp) + 8` and a load rooted at
/// `(sp & -16) + 8` share the numeric offset 8 but have DIFFERENT SP bases.
/// The aligned base differs from initial SP by `sp mod 16` (caller-dependent
/// and unknown), so the two addresses are NOT the same memory.  LoadForward
/// must therefore NOT forward the store's value to the load.  Before the
/// base-aware fix it compared offset alone and wrongly forwarded.
#[test]
fn does_not_forward_across_distinct_sp_bases_at_equal_offset() -> Result<()> {
    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let eight = b.build_int_const(8u64, ValueType::I32)?;
        // store at InitialVar(sp) + 8
        let store_addr =
            b.build_int_binary_operation(sp_val, eight, IntBinaryOp::Add, ValueType::I32)?;
        let data = b.build_int_const(0x11u64, ValueType::I32)?;
        b.build_store(store_addr, data, rsleigh::VnSpace::RAM)?;
        // aligned base: `sp & 0xFFFFFFF0` (i.e. `and rsp, -16`), then + 8
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

/// A non-aliasing store at a different offset sits between the target
/// store and the load.  The walker must step past it and still forward
/// the `Store(sp+4)` value to the load.
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
        // Order: store at +4 first, then +12, then load +4.  The load's
        // memory input chain is store12 -> store4 -> InitialMemory.
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

/// Overlap case: `*(sp+0) = I64(...); return *(sp+4) as I32` — the store
/// covers `[0, 8)` which intersects the load's `[4, 8)`, so forwarding
/// must bail and the load must remain.
#[test]
fn bail_on_overlapping_store() -> Result<()> {
    let sp = sp64_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let four = b.build_int_const(4u64, ValueType::I64)?;
        let addr4 = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I64)?;
        // Store I64 at sp+0 (covers [0,8)), then load I32 from sp+4 ([4,8)).
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

/// Partial-overlap, store-inside-load orientation: `*(sp+2) = I32(...);
/// return *(sp+0) as I64`.  The store covers `[2, 6)` — strictly inside
/// the load's `[0, 8)` — so the stored bytes don't fully back the load and
/// forwarding must bail (the load remains; no value-Phi tricks).
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

/// A NARROWER store at the SAME address sits between a wide store and a
/// wide load: `*(sp+0) = I64(A); *(sp+0) = I32(B); return *(sp+0) as I64`.
/// The narrow store partially clobbers the wide one (it is the nearest
/// memory def but doesn't fully back the 8-byte load), so the walk must
/// bail — neither value forwards and the load survives.
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

/// Type-mismatch at matching offset: `*(sp+0) = I32(...); return *(sp+0) as I64`.
/// Offsets agree but widths differ, so the stored bytes don't fully back
/// the load — bail.
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

/// Soundness floor: a non-SP-rooted intervening Store cannot be proven
/// disjoint from an SP-rooted Load (the constant address could
/// coincidentally equal `sp + K`, or the address could be an escaped
/// SP-derived pointer the lifter loses track of).  Under `AliasMode::Strict`
/// (the default) the walker must bail rather than silently pass through.
#[test]
fn strict_does_not_forward_across_non_sp_intervening_store() -> Result<()> {
    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let four = b.build_int_const(4u64, ValueType::I32)?;
        let addr4 = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I32)?;
        let a = b.build_int_const(0xAAu64, ValueType::I32)?;
        b.build_store(addr4, a, rsleigh::VnSpace::RAM)?;
        // Opaque store to a non-SP address (a compile-time constant address).
        // Cross-class against the SP-rooted load — cannot be proven disjoint
        // without `AliasMode::StackGlobalDisjoint`.
        let heap_addr = b.build_int_const(0x1000u64, ValueType::I32)?;
        let other = b.build_int_const(0xBBu64, ValueType::I32)?;
        b.build_store(heap_addr, other, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    // Pin Strict explicitly: this test exercises the conservative floor.
    // The pass default is now `StackGlobalDisjoint`, under which the
    // const-addressed store is assumed disjoint and forwarding succeeds
    // (covered by `permissive_forwards_across_const_intervening_store`).
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

/// Under `AliasMode::StackGlobalDisjoint`, the cross-class
/// intervening Store(IntConst, _) is assumed to live outside the stack
/// region and the walker steps through it.  The SP-rooted Load
/// forwards from the matching SP-rooted Store.
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
        fg.int_const_val(ret_val) == Some(0xAA),
        "forwarded value must be IntConst(0xAA), got {:?}",
        fg.int_const_val(ret_val)
    );
    Ok(())
}

/// Even under `StackGlobalDisjoint`, an intervening Store whose
/// address is neither SP-rooted nor an `IntConst` (an Anchor address)
/// still bails — closing that gap would require escape analysis we
/// have not implemented.
#[test]
fn permissive_still_bails_on_anchor_intervening_store() -> Result<()> {
    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let four = b.build_int_const(4u64, ValueType::I32)?;
        let addr4 = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I32)?;
        let a = b.build_int_const(0xAAu64, ValueType::I32)?;
        b.build_store(addr4, a, rsleigh::VnSpace::RAM)?;
        // Anchor address: a load of a constant global, whose loaded
        // value is then used as a store address.  Neither SP-rooted
        // nor an IntConst.
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

// ── Const-address forwarding (Test B from the design plan) ──────────────────

/// `Store(IntConst(0x1000), 0xAA); Store(IntConst(0x2000), 0xBB);
///  Load(IntConst(0x1000))` — the load matches the matching store by
/// IntConst equality, and the intervening const-address store is
/// proven disjoint via `ranges_disjoint`.  Forwards under both modes.
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
        fg.int_const_val(ret_val) == Some(0xAA),
        "forwarded value must be IntConst(0xAA), got {:?}",
        fg.int_const_val(ret_val)
    );
    Ok(())
}

// ── Same-ValueId forwarding (Test C from the design plan) ──────────────

/// `p = Load(IntConst(0x100)); Store(p, 0xCC); Load(p)` — the load
/// matches the store by `ValueId` equality on the address slot.
/// Forwards under both modes.
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

    // Exactly one Load should remain: the `p = Load(IntConst(0x100))`
    // address-producer.  The Load(p) we wanted to forward must be gone.
    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 1,
        "Anchor-address Load with same-ValueId Store and no interferer \
         must forward; only the address-producer Load(IntConst(0x100)) survives"
    );
    let ret_val = crate::test_support::return_value(fg.graph())?;
    assert!(
        fg.int_const_val(ret_val) == Some(0xCC),
        "forwarded value must be IntConst(0xCC), got {:?}",
        fg.int_const_val(ret_val)
    );
    Ok(())
}

// ── Same-ValueId load with different-ValueId interferer ────────────
// (Test D from the design plan.)

/// `p = Load(0x100); q = Load(0x200); Store(p, 0xCC); Store(q, 0xDD);
///  Load(p)` — even though the matching Store(p, 0xCC) exists upstream,
/// the intervening `Store(q, 0xDD)` carries a DIFFERENT ValueId so
/// we cannot prove `q ≠ p` at runtime.  Forwarding must bail.
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

    // The matching Load(p) we wanted to forward must remain, alongside
    // the two address-producer Loads.
    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert!(
        reachable_loads >= 3,
        "Anchor-address Load with different-ValueId Anchor interferer \
         must NOT forward; expected ≥3 Load nodes (2 address-producers + \
         the unforwarded Load(p)), got {reachable_loads}"
    );
    Ok(())
}

/// A call between store and load clobbers memory; forwarding across it is
/// unsafe, so the load must remain.  Uses a
/// link-register-style convention (ret_stack_pop=0) so SP stays stable
/// through the call, keeping the load's address decomposable.
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
    b.build_call(target, None)?;
    // SP did not shift (ret_stack_pop=0), so sp+4 is still the same slot.
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

/// If/else diamond where each arm stores a *distinct* constant at `sp+4`,
/// then the merge loads `sp+4`.  The two per-branch stores leave a
/// surviving (non-trivial) `MemPhi` at the merge.
///
/// BEHAVIOUR CHANGE (intentional): the forwarder no longer synthesizes a
/// value-`Phi` to bridge a control merge.  `load_forward` now walks the
/// memory-SSA chain with `MemPhiPolicy::Boundary`, so the surviving
/// `MemPhi` is an opaque clobber boundary — there is no single stored
/// value that backs the load across the merge, so the forward bails.  The
/// load must SURVIVE and NO value-`Phi` may be created.  (The previous
/// version of this test asserted exactly one synthesized ValuePhi whose
/// phi-token matched the MemPhi's; that synthesis path has been deleted.)
#[test]
fn per_branch_stores_same_offset_do_not_forward_and_synthesize_no_phi() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .build_fn()?;
    let entry = b.create_region()?;
    let then_r = b.create_region()?;
    let else_r = b.create_region()?;
    let merge = b.create_region()?;
    b.set_entry_region(entry)?;

    // entry: if const(true) { then } else { else }
    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, then_r, else_r)?;

    // then: *(sp+4) = 0xAA; goto merge
    b.set_region(then_r);
    let sp_t = b.read_variable(&sp)?;
    let four_t = b.build_int_const(4u64, ValueType::I32)?;
    let addr_t = b.build_int_binary_operation(sp_t, four_t, IntBinaryOp::Add, ValueType::I32)?;
    let a = b.build_int_const(0xAAu64, ValueType::I32)?;
    b.build_store(addr_t, a, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    // else: *(sp+4) = 0xBB; goto merge
    b.set_region(else_r);
    let sp_e = b.read_variable(&sp)?;
    let four_e = b.build_int_const(4u64, ValueType::I32)?;
    let addr_e = b.build_int_binary_operation(sp_e, four_e, IntBinaryOp::Add, ValueType::I32)?;
    let bval = b.build_int_const(0xBBu64, ValueType::I32)?;
    b.build_store(addr_e, bval, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    // merge: return *(sp+4)
    b.set_region(merge);
    let sp_m = b.read_variable(&sp)?;
    let four_m = b.build_int_const(4u64, ValueType::I32)?;
    let addr_m = b.build_int_binary_operation(sp_m, four_m, IntBinaryOp::Add, ValueType::I32)?;
    let loaded = b.build_load(addr_m, rsleigh::VnSpace::RAM, ValueType::I32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let phis_before = reachable_anonymous_phi_count(&fg);

    // Skip DeadBranchElimination so the `If(const true)` diamond survives
    // through the pass — otherwise both arms would collapse and the
    // MemPhi would too.
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

/// Three-way control merge: a nested `If` gives the merge region THREE
/// predecessors, each storing a distinct constant at `sp+4`, then the
/// merge loads `sp+4`.  Same contract as the 2-way diamond: the surviving
/// 3-input `MemPhi` is an opaque boundary — the load must NOT forward and
/// no value-`Phi` may be synthesized.
#[test]
fn three_predecessor_memphi_blocks_forwarding_no_phi() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .build_fn()?;
    let entry = b.create_region()?;
    let arm_a = b.create_region()?;
    let inner = b.create_region()?;
    let arm_b = b.create_region()?;
    let arm_c = b.create_region()?;
    let merge = b.create_region()?;
    b.set_entry_region(entry)?;

    // entry: if cond { arm_a } else { inner };  inner: if cond { arm_b } else { arm_c }
    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, arm_a, inner)?;
    b.set_region(inner);
    let cond2 = b.build_boolean_const(false);
    b.build_if(cond2, arm_b, arm_c)?;

    // Each arm: *(sp+4) = <distinct const>; goto merge.
    for (region, val) in [(arm_a, 0xAAu64), (arm_b, 0xBB), (arm_c, 0xCC)] {
        b.set_region(region);
        let sp_v = b.read_variable(&sp)?;
        let four = b.build_int_const(4u64, ValueType::I32)?;
        let addr = b.build_int_binary_operation(sp_v, four, IntBinaryOp::Add, ValueType::I32)?;
        let c = b.build_int_const(val, ValueType::I32)?;
        b.build_store(addr, c, rsleigh::VnSpace::RAM)?;
        b.build_branch(merge)?;
    }

    // merge: return *(sp+4)
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

/// A store BEFORE an `If` (dominating both branches), empty branches, then
/// a load AFTER the join reading the same slot with no intervening store.
/// `PhiCollapse` collapses the trivial merge `MemPhi` (both arms carry the
/// identical pre-branch memory token), so the load's chain becomes linear
/// and the memory-SSA walk reaches the dominating store directly — the
/// load forwards.  Crucially this happens WITHOUT synthesizing any
/// value-`Phi`.
#[test]
fn dominating_store_across_collapsible_merge_forwards_with_no_phi() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .build_fn()?;
    let entry = b.create_region()?;
    let then_r = b.create_region()?;
    let else_r = b.create_region()?;
    let merge = b.create_region()?;
    b.set_entry_region(entry)?;

    // entry: *(sp+4) = 0xAB; if const(true) { then } else { else }
    b.set_region(entry);
    let sp_en = b.read_variable(&sp)?;
    let four_en = b.build_int_const(4u64, ValueType::I32)?;
    let addr_en = b.build_int_binary_operation(sp_en, four_en, IntBinaryOp::Add, ValueType::I32)?;
    let dominating = b.build_int_const(0xABu64, ValueType::I32)?;
    b.build_store(addr_en, dominating, rsleigh::VnSpace::RAM)?;
    let cond = b.build_boolean_const(true);
    b.build_if(cond, then_r, else_r)?;

    // then / else: empty — just branch to merge (no memory writes).
    b.set_region(then_r);
    b.build_branch(merge)?;
    b.set_region(else_r);
    b.build_branch(merge)?;

    // merge: return *(sp+4)
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
        fg.int_const_val(ret_val) == Some(0xAB),
        "forwarded value must be the dominating store's 0xAB, got {:?}",
        fg.int_const_val(ret_val),
    );
    let phis_after = reachable_anonymous_phi_count(&fg);
    assert_eq!(
        phis_after, phis_before,
        "dominating-store forwarding must not synthesize a value-Phi",
    );
    assert_eq!(phis_after, 0, "load_forward is phi-free");
    Ok(())
}

/// If/else diamond where only the then-arm stores at `sp+4`, then the
/// merge loads `sp+4`.  The else-branch reaches the merge with a stale
/// memory (InitialMemory), so the walk through the MemPhi must fail on
/// that predecessor and the entire forward must bail — the load stays.
#[test]
fn phi_missing_store_on_one_branch_bails() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .build_fn()?;
    let entry = b.create_region()?;
    let then_r = b.create_region()?;
    let else_r = b.create_region()?;
    let merge = b.create_region()?;
    b.set_entry_region(entry)?;

    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, then_r, else_r)?;

    // then: *(sp+4) = 0xAA
    b.set_region(then_r);
    let sp_t = b.read_variable(&sp)?;
    let four_t = b.build_int_const(4u64, ValueType::I32)?;
    let addr_t = b.build_int_binary_operation(sp_t, four_t, IntBinaryOp::Add, ValueType::I32)?;
    let a = b.build_int_const(0xAAu64, ValueType::I32)?;
    b.build_store(addr_t, a, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    // else: no store — falls through with unchanged memory
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

/// If/else diamond where both arms store at *different* non-aliasing
/// offsets but share an earlier store at `sp+4` in the entry block.
/// This forces the MemPhi at the merge to have distinct per-predecessor
/// memory inputs (so it can't collapse into a single value) while both
/// resolver walks still bottom out on the same `Store(sp+4)`'s data.
/// The dedup path in `resolve()` must then skip the ValuePhi synthesis
/// and return the shared data output directly.
#[test]
fn phi_identical_values_no_new_phi() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .build_fn()?;
    let entry = b.create_region()?;
    let then_r = b.create_region()?;
    let else_r = b.create_region()?;
    let merge = b.create_region()?;
    b.set_entry_region(entry)?;

    // entry: *(sp+4) = 0xAA; then if(true) goto then else goto else
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let sp_e = b.read_variable(&sp)?;
    let four_e = b.build_int_const(4u64, ValueType::I32)?;
    let addr_e = b.build_int_binary_operation(sp_e, four_e, IntBinaryOp::Add, ValueType::I32)?;
    let shared = b.build_int_const(0xAAu64, ValueType::I32)?;
    b.build_store(addr_e, shared, rsleigh::VnSpace::RAM)?;
    let cond = b.build_boolean_const(true);
    b.build_if(cond, then_r, else_r)?;

    // then: *(sp+8) = 0xBB; branch merge
    b.set_region(then_r);
    let sp_t = b.read_variable(&sp)?;
    let eight_t = b.build_int_const(8u64, ValueType::I32)?;
    let addr_t = b.build_int_binary_operation(sp_t, eight_t, IntBinaryOp::Add, ValueType::I32)?;
    let bt = b.build_int_const(0xBBu64, ValueType::I32)?;
    b.build_store(addr_t, bt, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    // else: *(sp+12) = 0xCC; branch merge
    b.set_region(else_r);
    let sp_l = b.read_variable(&sp)?;
    let twelve_l = b.build_int_const(12u64, ValueType::I32)?;
    let addr_l = b.build_int_binary_operation(sp_l, twelve_l, IntBinaryOp::Add, ValueType::I32)?;
    let cc = b.build_int_const(0xCCu64, ValueType::I32)?;
    b.build_store(addr_l, cc, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    // merge: return *(sp+4)
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

/// Stores via `Sub(sp, 4)` and loads back via `Add(sp, 0xFFFFFFFC_U32)`.
/// Both forms must normalise to offset `-4` so the forwarder pipes the
/// stored value straight into the return — even without `ConstantFold`
/// running to canonicalise the encodings first.
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

    // Canonicalize first: `ConstantFold` folds both the sub and add encodings
    // to the same `Add(sp, IntConst(-4))` shape, then LoadForward bridges the
    // store and load (it does not peel the `Neg` itself).
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
        fg.int_const_val(ret_val) == Some(0x4242),
        "forwarded value must be the stored constant 0x4242 — got {:?}",
        fg.int_const_val(ret_val),
    );
    Ok(())
}

/// Real-world pattern from `fixtures/test.c::struct_test` at `-O0 -m32`:
/// the prologue spills a callee-saved register / arg to a 4-byte stack slot
/// via `Store u32` at sp-relative offset, then the body reads a single byte
/// via `Load u8` at the same SP offset.  The load is narrower than the store,
/// but its bytes are fully contained in the stored value — forwarding must
/// emit a `Truncate(stored_u32, u8)` (which `ConstantFold` then folds to a
/// byte constant when the stored value is itself a constant).
#[test]
fn narrow_load_from_wider_store_forwards_via_truncate() -> Result<()> {
    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let eight = b.build_int_const(8u64, ValueType::I32)?;
        let addr = b.build_sub_as_add_neg(sp_val, eight, ValueType::I32)?;
        // Store the full 4-byte value, then load only the low byte.
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
    // `int_const_val` applies the output type's mask, so for a I8 output it
    // returns the low byte even when the backing `IntConst` node still
    // carries the full u32 bit-pattern internally.
    let val_ty = fg.value_type_opt(ret_inputs[2]);
    assert_eq!(val_ty, Some(ValueType::I8));
    assert_eq!(
        fg.int_const_val(ret_inputs[2]),
        Some(0xEF),
        "forwarded narrow load must fold to the low byte 0xEF",
    );
    Ok(())
}

/// As above but the load takes two bytes: `Load u16` at the matching
/// offset of a `Store u32`.  Folds to the low 16 bits of the stored
/// constant — `0xBEEF` for `0xDEADBEEF`.  Guards the u16 case independently
/// because the analyzer emits both for `struct_test`'s short/char reads.
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
        fg.int_const_val(ret_inputs[2]),
        Some(0xBEEF),
        "forwarded u16 load must fold to low 16 bits 0xBEEF",
    );
    Ok(())
}

/// Big-endian narrow-load-from-wider-store: the load takes the *high*
/// `load_size` bytes of the stored value, so forwarding must synthesise
/// `Truncate(ShiftRight(data, (store_size - load_size) * 8))` rather than
/// the LE plain `Truncate(data)`.
///
/// We use the same fixture as
/// `narrow_load_from_wider_store_forwards_via_truncate` but configure the
/// pass with `Endianness::Big`.  We deliberately omit `ConstantFold` so the
/// `Truncate(ShiftRight(...))` chain survives intact for inspection — under
/// folding it would collapse to a single `IntConst(0xDE)` and we'd lose the
/// structural assertion.
#[test]
fn narrow_load_from_wider_store_be_shifts_high_bytes() -> Result<()> {
    let sp = sp32_vn();
    // The function is the SSoT for endianness, so build it big-endian.
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .endianness(Endianness::Big)
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    let eight = b.build_int_const(8u64, ValueType::I32)?;
    let addr = b.build_sub_as_add_neg(sp_val, eight, ValueType::I32)?;
    // Store the full 4-byte value, then load only the high byte (BE).
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

    // Outer node: Truncate.
    let outer = fg.producer(val_value);
    assert!(
        matches!(fg.node_kind(outer), NodeKind::Truncate),
        "BE narrow forward must wrap data in a Truncate — got {:?}",
        fg.node_kind(outer),
    );

    // Inner node: ShiftRight.
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

    // ShiftRight inputs: [data, shift_const]; shift_const = (4 - 1) * 8 = 24.
    let shr_inputs = fg.node_inputs(inner);
    assert_eq!(shr_inputs.len(), 2, "ShiftRight has two inputs");
    let shift_val = shr_inputs[1];
    assert!(
        fg.int_const_val(shift_val) == Some(24),
        "BE shift amount must be (store_size - load_size) * 8 = 24 — got {:?}",
        fg.int_const_val(shift_val),
    );
    Ok(())
}

/// Diamond MemPhi where one predecessor stores a *wider* value at the load
/// offset and the other predecessor has no matching store.  The arms
/// disagree (one clobbers sp+0, one is clean), so the memory-SSA walk
/// makes the `MemPhi` the boundary and the forward bails.  Because the
/// forwarder only builds reshape nodes AFTER the exact-match decision is
/// final, the bail creates NO nodes at all — no orphan `Truncate` /
/// value-`Phi` may appear.
#[test]
fn aborted_memphi_resolution_creates_no_nodes() -> Result<()> {
    let sp = sp64_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .build_fn()?;
    let entry = b.create_region()?;
    let then_r = b.create_region()?;
    let else_r = b.create_region()?;
    let merge = b.create_region()?;
    b.set_entry_region(entry)?;

    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, then_r, else_r)?;

    // then: *(sp+0) = I64 wide value (will trigger narrow synthesis at the
    // I32 load below).
    b.set_region(then_r);
    let sp_t = b.read_variable(&sp)?;
    let wide = b.build_int_const(0xDEAD_BEEF_CAFE_BABEu64, ValueType::I64)?;
    b.build_store(sp_t, wide, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    // else: no store at sp+0 — the arms disagree, forcing a bail.
    b.set_region(else_r);
    b.build_branch(merge)?;

    // merge: load I32 from sp+0.  The walk joins pred(then) (a clobber)
    // with pred(else) (clean) → disagreement → MemPhi boundary → bail.
    b.set_region(merge);
    let sp_m = b.read_variable(&sp)?;
    let loaded = b.build_load(sp_m, rsleigh::VnSpace::RAM, ValueType::I32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // Normalize the graph (single-pred phi collapse) BEFORE measuring the
    // leak baseline so that prep-introduced node changes don't show up as
    // a "leak" attributable to SLF.
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

    // Run LoadForward in isolation so the leak attributable to it is
    // observable directly (a multi-pass pipeline would obscure the
    // attribution).
    LoadForward.run_one(&mut fg, &mut crate::OptCtx::new(None))?;

    // The load must NOT have been forwarded (one branch has no matching
    // store), AND no orphan Truncate / ValuePhi may remain in the arena.
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

/// Pins the global invariant: `load_forward` is phi-free — running it can
/// only ever DECREASE (or leave unchanged) the number of `Phi` nodes in
/// the graph, never INCREASE it.  Exercised on a diamond with per-branch
/// stores at the same offset (a surviving MemPhi) where the *old* pass
/// would have synthesized a value-`Phi`.
#[test]
fn load_forward_never_increases_phi_count() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .build_fn()?;
    let entry = b.create_region()?;
    let then_r = b.create_region()?;
    let else_r = b.create_region()?;
    let merge = b.create_region()?;
    b.set_entry_region(entry)?;

    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, then_r, else_r)?;

    // then: *(sp+4) = 0xAA
    b.set_region(then_r);
    let sp_t = b.read_variable(&sp)?;
    let four_t = b.build_int_const(4u64, ValueType::I32)?;
    let addr_t = b.build_int_binary_operation(sp_t, four_t, IntBinaryOp::Add, ValueType::I32)?;
    let a = b.build_int_const(0xAAu64, ValueType::I32)?;
    b.build_store(addr_t, a, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    // else: *(sp+4) = 0xBB
    b.set_region(else_r);
    let sp_e = b.read_variable(&sp)?;
    let four_e = b.build_int_const(4u64, ValueType::I32)?;
    let addr_e = b.build_int_binary_operation(sp_e, four_e, IntBinaryOp::Add, ValueType::I32)?;
    let bval = b.build_int_const(0xBBu64, ValueType::I32)?;
    b.build_store(addr_e, bval, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    // merge: load *(sp+4)
    b.set_region(merge);
    let sp_m = b.read_variable(&sp)?;
    let four_m = b.build_int_const(4u64, ValueType::I32)?;
    let addr_m = b.build_int_binary_operation(sp_m, four_m, IntBinaryOp::Add, ValueType::I32)?;
    let loaded = b.build_load(addr_m, rsleigh::VnSpace::RAM, ValueType::I32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // Normalise first (collapse trivial phis) so the baseline reflects the
    // graph LoadForward actually sees.
    crate::test_support::cf_rp_pipeline().run(&mut fg, &mut crate::OptCtx::new(None))?;

    let total_phis_before = fg
        .graph()
        .all_node_ids()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Phi))
        .count();

    LoadForward.run_one(&mut fg, &mut crate::OptCtx::new(None))?;

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
