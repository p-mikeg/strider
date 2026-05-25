use super::*;
use crate::opt::error::Result;
use crate::opt::pipeline::Optimizer;
use crate::opt::{AliasSplit, ConstantFold, OptimizerPipeline, RedundantPhis};
use strider_ir::node::{NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};
use strider_ir::{AliasClass};
use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR};
use strider_ir::IntBinaryOp;
use strider_target::Endianness;

/// Fake 4-byte SP varnode (x86-cdecl-like).
fn sp32_vn() -> rsleigh::Vn {
    rsleigh::Vn {
        addr_off: 0x20,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    }
}

/// 8-byte SP for aarch64/x86-64-like scenarios.
fn sp64_vn() -> rsleigh::Vn {
    rsleigh::Vn {
        addr_off: 0x40,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    }
}

/// Counts reachable anonymous (Vn-untagged) `Phi` nodes — the shape
/// `StackLoadForward` synthesises when forwarding a load across a
/// `MemPhi`.  Vn-tagged phis (created at lift time for register-aliased
/// reads) are excluded.
fn reachable_anonymous_phi_count(fg: &strider_ir::Function) -> usize {
    let reachable: entity_utils::DenseEntitySet<strider_ir::node::NodeId> =
        fg.preorder().collect();
    fg.all_node_ids()
        .filter(|n| reachable.contains(*n)
            && matches!(fg.node_kind(*n), NodeKind::Phi)
            && fg.phi_var_tag(*n).is_none())
        .count()
}

/// Finds the unique reachable anonymous (Vn-untagged) `Phi` node.
fn find_reachable_anonymous_phi(
    fg: &strider_ir::Function,
) -> Option<strider_ir::node::NodeId> {
    let reachable: entity_utils::DenseEntitySet<strider_ir::node::NodeId> =
        fg.preorder().collect();
    fg.all_node_ids().find(|n| reachable.contains(*n)
        && matches!(fg.node_kind(*n), NodeKind::Phi)
        && fg.phi_var_tag(*n).is_none())
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

    // 10k-store chain pins the iterative form of
    // `find_stack_stored_value_at_offset`.  The prior recursive form
    // would stack-overflow on the default 8 MB Rust stack at this
    // depth.  See the deep-chain regression test below for a smaller,
    // deterministic check.
    const CHAIN_LEN: usize = 10_000;

    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        // Store 0x99 at sp+0 first (the value we'll forward to).
        let zero = b.build_int_const(0u64, NodeOutputType::U32)?;
        let target_addr = b.build_int_binary_operation(
            sp_val, zero, IntBinaryOp::Add, NodeOutputType::U32,
        )?;
        let target_val = b.build_int_const(0x99u64, NodeOutputType::U32)?;
        b.build_store(target_addr, target_val, rsleigh::VnSpace::RAM)?;

        // CHAIN_LEN disjoint stores at increasing offsets.  Each is
        // a fresh SP-relative store in the memory chain that the probe must
        // walk past to reach the target store.
        for i in 1..=CHAIN_LEN {
            let off = b.build_int_const(((i * 4) as u64) + 8, NodeOutputType::U32)?;
            let addr = b.build_int_binary_operation(
                sp_val, off, IntBinaryOp::Add, NodeOutputType::U32,
            )?;
            let val = b.build_int_const(i as u64, NodeOutputType::U32)?;
            b.build_store(addr, val, rsleigh::VnSpace::RAM)?;
        }

        // Load from sp+0 — this drives `probe` backward through every
        // store in the chain.  After forwarding, the load and the
        // intermediate stores at irrelevant offsets should still exist
        // (they're side-effecting writes), but the load itself must
        // disappear.
        let loaded = b.build_load(target_addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let pipeline = crate::opt::test_support::standard_test(sp, Endianness::Little);
    pipeline.run_built(&mut fg)?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 0,
        "Load at sp+0 must forward past all {CHAIN_LEN} disjoint stack stores"
    );
    Ok(())
}

#[test]
fn forward_load_after_matching_store_returns_stored_value() -> Result<()> {

    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr =
            b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, NodeOutputType::U32)?;
        let data = b.build_int_const(0x11u64, NodeOutputType::U32)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let pipeline = crate::opt::test_support::standard_test(sp, Endianness::Little);
    pipeline.run_built(&mut fg)?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(reachable_loads, 0, "Load[sp+4] should be forwarded away");
    Ok(())
}

/// A non-aliasing store at a different offset sits between the target
/// store and the load.  The walker must step past it and still forward
/// the `Store(sp+4)` value to the load.
#[test]
fn forward_skips_non_aliasing_store() -> Result<()> {

    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let twelve = b.build_int_const(12u64, NodeOutputType::U32)?;
        let addr4 =
            b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, NodeOutputType::U32)?;
        let addr12 =
            b.build_int_binary_operation(sp_val, twelve, IntBinaryOp::Add, NodeOutputType::U32)?;
        let a = b.build_int_const(0xAAu64, NodeOutputType::U32)?;
        let b_val = b.build_int_const(0xBBu64, NodeOutputType::U32)?;
        // Order: store at +4 first, then +12, then load +4.  The load's
        // memory input chain is store12 -> store4 -> InitialMemory.
        b.build_store(addr4, b_val, rsleigh::VnSpace::RAM)?;
        b.build_store(addr12, a, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let pipeline = crate::opt::test_support::standard_test(sp, Endianness::Little);
    pipeline.run_built(&mut fg)?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 0,
        "Load[sp+4] should forward past the non-aliasing Store(sp+12)"
    );
    Ok(())
}

/// Overlap case: `*(sp+0) = U64(...); return *(sp+4) as U32` — the store
/// covers `[0, 8)` which intersects the load's `[4, 8)`, so forwarding
/// must bail and the load must remain.
#[test]
fn bail_on_overlapping_store() -> Result<()> {

    let sp = sp64_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let four = b.build_int_const(4u64, NodeOutputType::U64)?;
        let addr4 =
            b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, NodeOutputType::U64)?;
        // Store U64 at sp+0 (covers [0,8)), then load U32 from sp+4 ([4,8)).
        let wide_data = b.build_int_const(0xDEAD_BEEF_CAFE_BABEu64, NodeOutputType::U64)?;
        b.build_store(sp_val, wide_data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let pipeline = crate::opt::test_support::standard_test(sp, Endianness::Little);
    pipeline.run_built(&mut fg)?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 1,
        "overlapping store must prevent forwarding"
    );
    Ok(())
}

/// Type-mismatch at matching offset: `*(sp+0) = U32(...); return *(sp+0) as U64`.
/// Offsets agree but widths differ, so the stored bytes don't fully back
/// the load — bail.
#[test]
fn bail_on_type_mismatch() -> Result<()> {

    let sp = sp64_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let narrow = b.build_int_const(0x11u64, NodeOutputType::U32)?;
        b.build_store(sp_val, narrow, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(sp_val, rsleigh::VnSpace::RAM, NodeOutputType::U64)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let pipeline = crate::opt::test_support::standard_test(sp, Endianness::Little);
    pipeline.run_built(&mut fg)?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(reachable_loads, 1, "type mismatch must prevent forwarding");
    Ok(())
}

/// An intervening `Store(_)` whose address is *not* SP-relative is provably
/// non-aliasing with any stack slot (different address spaces, or at least
/// different decomposition: one is `sp + K`, the other isn't).  The walker
/// passes through it and forwards the load.
///
/// Was previously `bail_on_opaque_store_between` — pinned the
/// over-conservative pre--fix behaviour.  Renamed to reflect the new
/// (correct) behaviour and the original docstring's actual semantic
/// observation.
#[test]
fn forwards_across_non_sp_store_between() -> Result<()> {

    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr4 =
            b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, NodeOutputType::U32)?;
        let a = b.build_int_const(0xAAu64, NodeOutputType::U32)?;
        b.build_store(addr4, a, rsleigh::VnSpace::RAM)?;
        // Opaque store to a non-SP address (a compile-time constant address —
        // can't be SP-relative because SP is an InitialVar reading the entry
        // SP, while the address here is a literal IntConst).
        let heap_addr = b.build_int_const(0x1000u64, NodeOutputType::U32)?;
        let other = b.build_int_const(0xBBu64, NodeOutputType::U32)?;
        b.build_store(heap_addr, other, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let pipeline = crate::opt::test_support::standard_test(sp, Endianness::Little);
    pipeline.run_built(&mut fg)?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 0,
        "non-SP-relative intervening Store must not block forwarding \
         (the addresses are provably non-aliasing)"
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
        .sp(sp)
        .build_fn_single_region()?;

    let sp_val = b.read_variable(&sp)?;
    let four = b.build_int_const(4u64, NodeOutputType::U64)?;
    let addr4 =
        b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, NodeOutputType::U64)?;
    let data = b.build_int_const(0x11u64, NodeOutputType::U32)?;
    b.build_store(addr4, data, rsleigh::VnSpace::RAM)?;
    let target = b.build_int_const(0x1000u64, NodeOutputType::U64)?;
    b.build_call(target)?;
    // SP did not shift (ret_stack_pop=0), so sp+4 is still the same slot.
    let sp_val2 = b.read_variable(&sp)?;
    let addr4b =
        b.build_int_binary_operation(sp_val2, four, IntBinaryOp::Add, NodeOutputType::U64)?;
    let loaded = b.build_load(addr4b, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let pipeline = crate::opt::test_support::standard_test(sp, Endianness::Little);
    pipeline.run_built(&mut fg)?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 1,
        "Call on memory chain must prevent forwarding"
    );
    Ok(())
}

/// If/else diamond where each arm stores a distinct constant at `sp+4`,
/// then the merge loads `sp+4`.  After the pass the load must be gone
/// and a single `ValuePhi` synthesized in its place; the phi-token of
/// that `ValuePhi` must be the same token fed into the underlying
/// `MemPhi` (i.e. the merge `Region`'s dispatch output).
#[test]
fn phi_both_branches_store_same_offset() -> Result<()> {

    let sp = sp32_vn();
    let mut b = RegisterSet::new().tracked(sp).callee_saved(sp).build_fn()?;
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
    let four_t = b.build_int_const(4u64, NodeOutputType::U32)?;
    let addr_t =
        b.build_int_binary_operation(sp_t, four_t, IntBinaryOp::Add, NodeOutputType::U32)?;
    let a = b.build_int_const(0xAAu64, NodeOutputType::U32)?;
    b.build_store(addr_t, a, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    // else: *(sp+4) = 0xBB; goto merge
    b.set_region(else_r);
    let sp_e = b.read_variable(&sp)?;
    let four_e = b.build_int_const(4u64, NodeOutputType::U32)?;
    let addr_e =
        b.build_int_binary_operation(sp_e, four_e, IntBinaryOp::Add, NodeOutputType::U32)?;
    let bval = b.build_int_const(0xBBu64, NodeOutputType::U32)?;
    b.build_store(addr_e, bval, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    // merge: return *(sp+4)
    b.set_region(merge);
    let sp_m = b.read_variable(&sp)?;
    let four_m = b.build_int_const(4u64, NodeOutputType::U32)?;
    let addr_m =
        b.build_int_binary_operation(sp_m, four_m, IntBinaryOp::Add, NodeOutputType::U32)?;
    let loaded = b.build_load(addr_m, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // Skip DeadBranchElimination so the `If(const true)` diamond survives
    // through the pass — otherwise both arms would collapse and there'd
    // be no MemPhi to synthesize a ValuePhi from.
    let pipeline = crate::opt::test_support::standard_test(sp, Endianness::Little);
    pipeline.run_built(&mut fg)?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 0,
        "Load at merge must be forwarded via synthesized ValuePhi"
    );
    let reachable_value_phis = reachable_anonymous_phi_count(&fg);
    assert_eq!(
        reachable_value_phis, 1,
        "exactly one ValuePhi must be synthesized"
    );

    // The ValuePhi's phi-token (input 0) must come from the same
    // Region as the MemPhi's phi-token.
    let reachable: entity_utils::DenseEntitySet<strider_ir::node::NodeId> =
        fg.preorder().collect();
    let value_phi = find_reachable_anonymous_phi(&fg)
        .expect("ValuePhi found above");
    let mem_phi = fg
        .all_node_ids()
        .find(|n| reachable.contains(*n) && matches!(fg.node_kind(*n), NodeKind::MemPhi))
        .expect("MemPhi survived to the merge");
    let vp_token = fg.node_inputs(value_phi)[0];
    let mp_token = fg.node_inputs(mem_phi)[0];
    assert_eq!(
        vp_token, mp_token,
        "ValuePhi's phi-token must match the MemPhi's phi-token"
    );
    Ok(())
}

/// If/else diamond where only the then-arm stores at `sp+4`, then the
/// merge loads `sp+4`.  The else-branch reaches the merge with a stale
/// memory (InitialMemory), so the walk through the MemPhi must fail on
/// that predecessor and the entire forward must bail — the load stays.
#[test]
fn phi_missing_store_on_one_branch_bails() -> Result<()> {

    let sp = sp32_vn();
    let mut b = RegisterSet::new().tracked(sp).callee_saved(sp).build_fn()?;
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
    let four_t = b.build_int_const(4u64, NodeOutputType::U32)?;
    let addr_t =
        b.build_int_binary_operation(sp_t, four_t, IntBinaryOp::Add, NodeOutputType::U32)?;
    let a = b.build_int_const(0xAAu64, NodeOutputType::U32)?;
    b.build_store(addr_t, a, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    // else: no store — falls through with unchanged memory
    b.set_region(else_r);
    b.build_branch(merge)?;

    b.set_region(merge);
    let sp_m = b.read_variable(&sp)?;
    let four_m = b.build_int_const(4u64, NodeOutputType::U32)?;
    let addr_m =
        b.build_int_binary_operation(sp_m, four_m, IntBinaryOp::Add, NodeOutputType::U32)?;
    let loaded = b.build_load(addr_m, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let pipeline = crate::opt::test_support::standard_test(sp, Endianness::Little);
    pipeline.run_built(&mut fg)?;

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
    let mut b = RegisterSet::new().tracked(sp).callee_saved(sp).build_fn()?;
    let entry = b.create_region()?;
    let then_r = b.create_region()?;
    let else_r = b.create_region()?;
    let merge = b.create_region()?;
    b.set_entry_region(entry)?;

    // entry: *(sp+4) = 0xAA; then if(true) goto then else goto else
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let sp_e = b.read_variable(&sp)?;
    let four_e = b.build_int_const(4u64, NodeOutputType::U32)?;
    let addr_e =
        b.build_int_binary_operation(sp_e, four_e, IntBinaryOp::Add, NodeOutputType::U32)?;
    let shared = b.build_int_const(0xAAu64, NodeOutputType::U32)?;
    b.build_store(addr_e, shared, rsleigh::VnSpace::RAM)?;
    let cond = b.build_boolean_const(true);
    b.build_if(cond, then_r, else_r)?;

    // then: *(sp+8) = 0xBB; branch merge
    b.set_region(then_r);
    let sp_t = b.read_variable(&sp)?;
    let eight_t = b.build_int_const(8u64, NodeOutputType::U32)?;
    let addr_t =
        b.build_int_binary_operation(sp_t, eight_t, IntBinaryOp::Add, NodeOutputType::U32)?;
    let bt = b.build_int_const(0xBBu64, NodeOutputType::U32)?;
    b.build_store(addr_t, bt, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    // else: *(sp+12) = 0xCC; branch merge
    b.set_region(else_r);
    let sp_l = b.read_variable(&sp)?;
    let twelve_l = b.build_int_const(12u64, NodeOutputType::U32)?;
    let addr_l =
        b.build_int_binary_operation(sp_l, twelve_l, IntBinaryOp::Add, NodeOutputType::U32)?;
    let cc = b.build_int_const(0xCCu64, NodeOutputType::U32)?;
    b.build_store(addr_l, cc, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    // merge: return *(sp+4)
    b.set_region(merge);
    let sp_m = b.read_variable(&sp)?;
    let four_m = b.build_int_const(4u64, NodeOutputType::U32)?;
    let addr_m =
        b.build_int_binary_operation(sp_m, four_m, IntBinaryOp::Add, NodeOutputType::U32)?;
    let loaded = b.build_load(addr_m, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let pipeline = crate::opt::test_support::standard_test(sp, Endianness::Little);
    pipeline.run_built(&mut fg)?;

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
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let store_addr =
            b.build_int_sub(sp_val, four, NodeOutputType::U32)?;
        let data = b.build_int_const(0x4242u64, NodeOutputType::U32)?;
        b.build_store(store_addr, data, rsleigh::VnSpace::RAM)?;

        let neg_four = b.build_int_const(0xFFFF_FFFCu64, NodeOutputType::U32)?;
        let load_addr =
            b.build_int_binary_operation(sp_val, neg_four, IntBinaryOp::Add, NodeOutputType::U32)?;
        let loaded = b.build_load(load_addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    // Intentionally omit `ConstantFold` so both encodings reach
    // `decompose_sp` as-lifted.  StackLoadForward's `Store` arm
    // decomposes addresses directly.
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(RedundantPhis);
    pipeline.add(StackLoadForward::new(sp, Endianness::Little));
    pipeline.run_built(&mut fg)?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 0,
        "Load[Add(sp, 0xFFFFFFFC)] must be forwarded from Store[Sub(sp, 4)]",
    );
    let ret = fg
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
        .expect("return node exists");
    let ret_inputs = fg.node_inputs(ret);
    // Return inputs: [ctrl, mem, val_0, ...].
    let val_kind = fg.kind_of_output(ret_inputs[2]);
    assert!(
        matches!(val_kind, NodeKind::IntConst(0x4242)),
        "forwarded value must be the stored constant 0x4242 — got {val_kind:?}",
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
        let eight = b.build_int_const(8u64, NodeOutputType::U32)?;
        let addr =
            b.build_int_sub(sp_val, eight, NodeOutputType::U32)?;
        // Store the full 4-byte value, then load only the low byte.
        let wide = b.build_int_const(0xDEAD_BEEFu64, NodeOutputType::U32)?;
        b.build_store(addr, wide, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U8)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let pipeline = crate::opt::test_support::standard_test(sp, Endianness::Little);
    pipeline.run_built(&mut fg)?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 0,
        "Load u8 at matching offset must be forwarded as the low byte of the u32 store",
    );
    let ret = fg
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
        .expect("return node exists");
    let ret_inputs = fg.node_inputs(ret);
    // `int_const_val` applies the output type's mask, so for a U8 output it
    // returns the low byte even when the backing `IntConst` node still
    // carries the full u32 bit-pattern internally.
    let val_ty = fg.output_kind(ret_inputs[2]).as_value();
    assert_eq!(val_ty, Some(NodeOutputType::U8));
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
        let twelve = b.build_int_const(12u64, NodeOutputType::U32)?;
        let addr =
            b.build_int_sub(sp_val, twelve, NodeOutputType::U32)?;
        let wide = b.build_int_const(0xDEAD_BEEFu64, NodeOutputType::U32)?;
        b.build_store(addr, wide, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U16)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let pipeline = crate::opt::test_support::standard_test(sp, Endianness::Little);
    pipeline.run_built(&mut fg)?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(reachable_loads, 0, "Load u16 must be forwarded");
    let ret = fg
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
        .expect("return node exists");
    let ret_inputs = fg.node_inputs(ret);
    let val_ty = fg.output_kind(ret_inputs[2]).as_value();
    assert_eq!(val_ty, Some(NodeOutputType::U16));
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
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let eight = b.build_int_const(8u64, NodeOutputType::U32)?;
        let addr =
            b.build_int_sub(sp_val, eight, NodeOutputType::U32)?;
        // Store the full 4-byte value, then load only the high byte (BE).
        let wide = b.build_int_const(0xDEAD_BEEFu64, NodeOutputType::U32)?;
        b.build_store(addr, wide, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U8)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(RedundantPhis);
    pipeline.add(StackLoadForward::new(sp, Endianness::Big));
    pipeline.run_built(&mut fg)?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 0,
        "Load u8 at matching offset must be forwarded as the high byte of the u32 store on BE",
    );

    let ret = fg
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
        .expect("return node exists");
    let ret_inputs = fg.node_inputs(ret);
    // Return inputs: [ctrl, mem, val_0, ...].
    let val_out = ret_inputs[2];
    let val_ty = fg.output_kind(val_out).as_value();
    assert_eq!(val_ty, Some(NodeOutputType::U8));

    // Outer node: Truncate.
    let outer = fg.get_node_from_output(val_out);
    assert!(
        matches!(fg.node_kind(outer), NodeKind::Truncate),
        "BE narrow forward must wrap data in a Truncate — got {:?}",
        fg.node_kind(outer),
    );

    // Inner node: ShiftRight.
    let outer_inputs = fg.node_inputs(outer);
    assert_eq!(outer_inputs.len(), 1, "Truncate has a single input");
    let inner = fg.get_node_from_output(outer_inputs[0]);
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
    let shift_kind = fg.kind_of_output(shr_inputs[1]);
    assert!(
        matches!(shift_kind, NodeKind::IntConst(24)),
        "BE shift amount must be (store_size - load_size) * 8 = 24 — got {shift_kind:?}",
    );
    Ok(())
}

/// Diamond MemPhi where one predecessor stores a *wider* value at the load
/// offset (triggering narrow-load synthesis) and the other predecessor has
/// no matching store (forces a bail).  Without the probe/realize split, the
/// narrow synthesis on the first predecessor leaks a `Truncate` node into
/// the graph as an orphan even though the overall walk returns `None`.
#[test]
fn aborted_memphi_resolution_does_not_leak_truncate() -> Result<()> {

    let sp = sp64_vn();
    let mut b = RegisterSet::new().tracked(sp).callee_saved(sp).build_fn()?;
    let entry = b.create_region()?;
    let then_r = b.create_region()?;
    let else_r = b.create_region()?;
    let merge = b.create_region()?;
    b.set_entry_region(entry)?;

    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, then_r, else_r)?;

    // then: *(sp+0) = U64 wide value (will trigger narrow synthesis at the
    // U32 load below).
    b.set_region(then_r);
    let sp_t = b.read_variable(&sp)?;
    let wide = b.build_int_const(0xDEAD_BEEF_CAFE_BABEu64, NodeOutputType::U64)?;
    b.build_store(sp_t, wide, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    // else: no store at sp+0 — forces resolve to bail.
    b.set_region(else_r);
    b.build_branch(merge)?;

    // merge: load U32 from sp+0.  resolve walks pred(then) first (narrow
    // synthesis would create a Truncate), then pred(else) returns None.
    b.set_region(merge);
    let sp_m = b.read_variable(&sp)?;
    let loaded = b.build_load(sp_m, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // Normalize the graph (single-pred phi collapse) BEFORE measuring the
    // leak baseline so that prep-introduced node changes don't show up as
    // a "leak" attributable to SLF.
    let mut prep = OptimizerPipeline::new();
    prep.add(ConstantFold);
    prep.add(RedundantPhis);
    let entry = fg.entry().unwrap();
    prep.run(&mut fg, entry)?;

    let total_truncate_before = fg
        .all_node_ids()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Truncate))
        .count();
    let total_value_phi_before = fg
        .all_node_ids()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Phi)
            && fg.phi_var_tag(n).is_none())
        .count();

    // Run StackLoadForward in isolation so the leak attributable to it is
    // observable directly (a multi-pass pipeline would obscure the
    // attribution).
    let entry = fg.entry().unwrap();
    StackLoadForward::new(sp, Endianness::Little).optimize(&mut fg, entry)?;

    // The load must NOT have been forwarded (one branch has no matching
    // store), AND no orphan Truncate / ValuePhi may remain in the arena.
    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(reachable_loads, 1, "load must remain — bail expected");

    let total_truncate_after = fg
        .all_node_ids()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Truncate))
        .count();
    let total_value_phi_after = fg
        .all_node_ids()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Phi)
            && fg.phi_var_tag(n).is_none())
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

// ── public helper for the indirect-branch classifier ─────────────
//
// `find_stack_stored_value_at_offset` walks the memory chain backward from
// a given `mem` looking for a `Store(sp+offset)` whose value type matches
// the caller's expectation.  Used by the
// indirect-branch classifier to look up entries of a stack-array of label
// addresses one offset at a time (computed-goto via local stack
// array).  These tests pin the helper's contract in isolation, before the
// classifier wires it in.

/// One stack store at the requested offset, value type matches: helper
/// returns the stored value's output id.
#[test]
fn find_stack_stored_value_finds_matching_store() -> crate::opt::Result<()> {

    let sp = sp64_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let twentyfour = b.build_int_const(24u64, NodeOutputType::U64)?;
        let addr = b.build_int_sub(sp_val, twentyfour, NodeOutputType::U64,
        )?;
        let stored = b.build_int_const(0xCAFEu64, NodeOutputType::U64)?;
        b.build_store(addr, stored, rsleigh::VnSpace::RAM)?;
        // Touch the stored memory token so it survives DCE: emit a load of the
        // same slot and return the loaded value.
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    // Run only ConstantFold + RedundantPhis (not StackLoadForward) so the
    // load + its memory input survive for inspection.
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.run_built(&mut fg)?;

    // Reach the surviving Load and use its memory-input as the chain root.
    let load = fg
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
        .expect("Load survives without StackLoadForward");
    let mem = fg.node_inputs(load).into_iter().next().unwrap();

    let mut memo = SpExprMemo::default();
    let mut walk_memo = StackStoredValueMemo::default();
    let result = find_stack_stored_value_at_offset(
        &fg,
        mem,
        -24,
        NodeOutputType::U64,
        sp,
        &mut memo,
        &mut walk_memo,
    );
    let value = result.expect("helper should find Store at offset -24");
    // The found value must be the stored constant 0xCAFE.
    assert_eq!(fg.int_const_val(value), Some(0xCAFE));
    Ok(())
}

/// Walks past a non-aliasing intermediate SP-relative store (different offset)
/// and finds the requested-offset store.
#[test]
fn find_stack_stored_value_walks_past_non_aliasing() -> crate::opt::Result<()> {

    let sp = sp64_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        // Two stores at distinct offsets that both belong to the chain reaching
        // a final load — mimics the array-of-labels prologue.
        let off24 = b.build_int_const(24u64, NodeOutputType::U64)?;
        let off16 = b.build_int_const(16u64, NodeOutputType::U64)?;
        let addr_24 =
            b.build_int_sub(sp_val, off24, NodeOutputType::U64)?;
        let addr_16 =
            b.build_int_sub(sp_val, off16, NodeOutputType::U64)?;
        let v_24 = b.build_int_const(0xAAAAu64, NodeOutputType::U64)?;
        let v_16 = b.build_int_const(0xBBBBu64, NodeOutputType::U64)?;
        b.build_store(addr_24, v_24, rsleigh::VnSpace::RAM)?;
        b.build_store(addr_16, v_16, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr_24, rsleigh::VnSpace::RAM, NodeOutputType::U64)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.run_built(&mut fg)?;

    let load = fg
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
        .expect("Load survives");
    let mem = fg.node_inputs(load).into_iter().next().unwrap();

    let mut memo = SpExprMemo::default();
    let mut walk_memo = StackStoredValueMemo::default();
    // Look up offset -16: the chain has the latest store at -16 and an
    // earlier store at -24 (non-aliasing).  Helper must find -16's value.
    let v16 = find_stack_stored_value_at_offset(
        &fg,
        mem,
        -16,
        NodeOutputType::U64,
        sp,
        &mut memo,
        &mut walk_memo,
    );
    assert_eq!(fg.int_const_val(v16.expect("find -16")), Some(0xBBBB));

    // Look up offset -24: must walk through the -16 store (non-aliasing) and
    // find -24's value.
    let v24 = find_stack_stored_value_at_offset(
        &fg,
        mem,
        -24,
        NodeOutputType::U64,
        sp,
        &mut memo,
        &mut walk_memo,
    );
    assert_eq!(fg.int_const_val(v24.expect("find -24")), Some(0xAAAA));
    Ok(())
}

/// No store at the requested offset: helper returns None (chain bottoms out
/// at InitialMemory without producing a value).
#[test]
fn find_stack_stored_value_no_match_returns_none() -> crate::opt::Result<()> {

    let sp = sp64_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let off24 = b.build_int_const(24u64, NodeOutputType::U64)?;
        let addr_24 =
            b.build_int_sub(sp_val, off24, NodeOutputType::U64)?;
        let v_24 = b.build_int_const(0xAAAAu64, NodeOutputType::U64)?;
        b.build_store(addr_24, v_24, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr_24, rsleigh::VnSpace::RAM, NodeOutputType::U64)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.run_built(&mut fg)?;

    let load = fg
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
        .expect("Load survives");
    let mem = fg.node_inputs(load).into_iter().next().unwrap();

    let mut memo = SpExprMemo::default();
    let mut walk_memo = StackStoredValueMemo::default();
    let result = find_stack_stored_value_at_offset(
        &fg,
        mem,
        -8,  // No store at -8.
        NodeOutputType::U64,
        sp,
        &mut memo,
        &mut walk_memo,
    );
    assert!(result.is_none(), "no store at -8 → helper returns None");
    Ok(())
}

/// Aliasing intermediate SP-relative store (overlaps the requested offset) is
/// the LIVE value at that slot — the helper returns the live store's value, not
/// the older one.
#[test]
fn find_stack_stored_value_returns_latest_at_aliasing_offset() -> crate::opt::Result<()> {

    let sp = sp64_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let off24 = b.build_int_const(24u64, NodeOutputType::U64)?;
        let addr_24 =
            b.build_int_sub(sp_val, off24, NodeOutputType::U64)?;
        let first = b.build_int_const(0xAAAAu64, NodeOutputType::U64)?;
        let second = b.build_int_const(0xBBBBu64, NodeOutputType::U64)?;
        // Two stores at the SAME offset; the second alias-overwrites the first.
        b.build_store(addr_24, first, rsleigh::VnSpace::RAM)?;
        b.build_store(addr_24, second, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr_24, rsleigh::VnSpace::RAM, NodeOutputType::U64)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.run_built(&mut fg)?;

    let load = fg
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
        .expect("Load survives");
    let mem = fg.node_inputs(load).into_iter().next().unwrap();

    let mut memo = SpExprMemo::default();
    let mut walk_memo = StackStoredValueMemo::default();
    let result = find_stack_stored_value_at_offset(
        &fg,
        mem,
        -24,
        NodeOutputType::U64,
        sp,
        &mut memo,
        &mut walk_memo,
    );
    // The helper must return the *live* (latest) value: the second store.
    let v = result.expect("must find live store");
    assert_eq!(fg.int_const_val(v), Some(0xBBBB));
    Ok(())
}

/// Type mismatch (store width != requested width at the matching offset)
/// returns None — the helper is strict about types because the classifier
/// needs an exact-typed match to safely treat the value as a target address.
#[test]
fn find_stack_stored_value_type_mismatch_returns_none() -> crate::opt::Result<()> {

    let sp = sp64_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let off24 = b.build_int_const(24u64, NodeOutputType::U64)?;
        let addr_24 =
            b.build_int_sub(sp_val, off24, NodeOutputType::U64)?;
        // Store U32, then load U64 — overlapping byte ranges intersect.
        let stored = b.build_int_const(0xAAAAu64, NodeOutputType::U32)?;
        b.build_store(addr_24, stored, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr_24, rsleigh::VnSpace::RAM, NodeOutputType::U64)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.run_built(&mut fg)?;

    let load = fg
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
        .expect("Load survives");
    let mem = fg.node_inputs(load).into_iter().next().unwrap();

    let mut memo = SpExprMemo::default();
    let mut walk_memo = StackStoredValueMemo::default();
    let result = find_stack_stored_value_at_offset(
        &fg,
        mem,
        -24,
        NodeOutputType::U64, // request U64 from a U32 store
        sp,
        &mut memo,
        &mut walk_memo,
    );
    assert!(result.is_none(), "type mismatch at offset -24 → None");
    Ok(())
}

/// End-to-end recipe: the classifier loop calls the helper once per i to
/// enumerate a stack-array of label addresses.  Mirrors the x64 
/// fixture's prologue (sp-24 → L0, sp-16 → L1).  Asserts the helper
/// produces N IntConst values that the classifier can then return as
/// `ResolvedTargets::Multiple`.
#[test]
fn find_stack_stored_value_enumerates_array_entries() -> crate::opt::Result<()> {

    let sp = sp64_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        // Mirror the x64 prologue: store target0 at sp-24, target1 at sp-16.
        let off24 = b.build_int_const(24u64, NodeOutputType::U64)?;
        let off16 = b.build_int_const(16u64, NodeOutputType::U64)?;
        let addr_24 =
            b.build_int_sub(sp_val, off24, NodeOutputType::U64)?;
        let addr_16 =
            b.build_int_sub(sp_val, off16, NodeOutputType::U64)?;
        let target0 = b.build_int_const(0x401190u64, NodeOutputType::U64)?;
        let target1 = b.build_int_const(0x401180u64, NodeOutputType::U64)?;
        b.build_store(addr_24, target0, rsleigh::VnSpace::RAM)?;
        b.build_store(addr_16, target1, rsleigh::VnSpace::RAM)?;
        // The actual load uses a symbolic address, but for THIS helper test we
        // only exercise the "look up by concrete offset" API — the symbolic
        // shape match lives in the classifier (tested separately in
        // `indirect_resolve_classify`).
        let loaded = b.build_load(addr_24, rsleigh::VnSpace::RAM, NodeOutputType::U64)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.run_built(&mut fg)?;

    let load = fg
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
        .expect("Load survives");
    let mem = fg.node_inputs(load).into_iter().next().unwrap();

    let mut memo = SpExprMemo::default();
    let mut walk_memo = StackStoredValueMemo::default();
    // The classifier loop: for each i in 0..2, look up base + i*stride.
    let base = -24i64;
    let stride = 8i64;
    let mut targets = Vec::new();
    for i in 0..2 {
        let off = base + i * stride;
        let v = find_stack_stored_value_at_offset(
            &fg,
            mem,
            off,
            NodeOutputType::U64,
            sp,
            &mut memo,
            &mut walk_memo,
        )
        .unwrap_or_else(|| panic!("must find store at offset {off}"));
        let c = fg.int_const_val(v).expect("stored value is IntConst");
        targets.push(c as u64);
    }
    assert_eq!(targets, vec![0x401190u64, 0x401180u64]);
    Ok(())
}

// ── MemProject / MemUnion boundary traversal ─────────────────────────────

/// StackLoadForward walks through a `MemProject` boundary node.
///
/// After `AliasSplit` runs, the memory chain from the `Load`'s
/// perspective is:
///
///   `Load[sp+4]` ← `Store(sp+4)` ← `MemProject(Stack)` ← `InitialMemory`
///
/// With a disjoint `Store(sp+8)` after `MemProject`, the backward
/// walk passes through `Store(sp+8)` (disjoint, skip), then hits
/// `MemProject` — previously the `_` arm returned `Verdict(None)`,
/// causing the load to stay.  Now the walk passes through `MemProject`
/// to `InitialMemory` without finding a matching store, confirming the
/// boundary is traversed rather than treated as an opaque barrier.
///
/// We also confirm the simple case — load at the same offset as the
/// store — where the walk matches `Store(sp+4)` immediately (before
/// even reaching `MemProject`), giving full forwarding.
#[test]
fn stack_load_forward_walks_through_mempartition() -> Result<()> {
    let sp = sp32_vn();

    // Build: store sp+4 = 0x42; load sp+4; return
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr =
            b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, NodeOutputType::U32)?;
        let data = b.build_int_const(0x42u64, NodeOutputType::U32)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    // Pipeline: ConstantFold → RedundantPhis → AliasSplit → StackLoadForward.
    // AliasSplit inserts `MemProject(Stack)` right after InitialMemory
    // and `MemUnion` before the Return.  After this, the Load's backward
    // chain passes through a Store(sp+4) then a MemProject node.
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(AliasSplit::new(sp, strider_target::ArchPreset::X86));
    pipeline.add(StackLoadForward::new(sp, Endianness::Little));

    let entry = fg.entry().unwrap();
    pipeline.run(&mut fg, entry)?;

    // AliasSplit must have inserted a MemProject node with a Stack output slot.
    let has_stack_mp = fg.all_node_ids().any(|n| {
        if !matches!(fg.node_kind(n), NodeKind::MemProject) {
            return false;
        }
        fg.node_outputs(n).iter().any(|&out| {
            matches!(
                fg.output_kind(out),
                strider_ir::node::NodeOutputKind::Memory(Some(strider_ir::AliasClass::Stack))
            )
        })
    });
    assert!(
        has_stack_mp,
        "AliasSplit must insert a MemProject with a Stack output slot",
    );

    // The Load should be forwarded: no reachable Load nodes remain.
    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 0,
        "Load[sp+4] must be forwarded through the MemProject boundary"
    );

    // The return value must be IntConst(0x42) — the stored value.
    let ret_kind = crate::opt::test_support::return_kind(fg.graph())?;
    assert!(
        matches!(ret_kind, NodeKind::IntConst(0x42)),
        "forwarded value must be IntConst(0x42), got {ret_kind:?}"
    );
    Ok(())
}

/// StackLoadForward walks through a `MemUnion` boundary to the
/// Stack-partition input.
///
/// `MemUnion` has N inputs (one per partition-typed memory edge).
/// When the backward walk arrives at a `MemUnion`, only the
/// Stack-partition input is relevant.  The pass must identify that
/// input by checking each input's `NodeOutputKind` for
/// `Memory(Some(AliasClass::Stack))`, and
/// continue through it.
///
/// Graph constructed manually to exercise the `MemUnion` arm directly:
///
///   `InitialMemory → MemProject(Stack) → Store(sp+4) → MemUnion → Load{sp+4}`
///
/// The `MemUnion` has a single Stack-partition input (from `Store`).
/// The `Load`'s memory input is the `MemUnion` output (`Memory(None)`).
/// Without the new arm, the walk bails on `MemUnion` and the load stays.
/// With the fix, the walk routes through the Stack-partition input to
/// `Store(sp+4)`, finds the matching store, and forwards the value.
#[test]
fn stack_load_forward_walks_through_memunion_to_stack_input() -> Result<()> {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    let sp = sp32_vn();

    // 1. build a simple function with store + load.
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr =
            b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, NodeOutputType::U32)?;
        let data = b.build_int_const(0xBEEFu64, NodeOutputType::U32)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    // 2. run ConstantFold + RedundantPhis to normalize the graph.
    {
        let mut prep = OptimizerPipeline::new();
        prep.add(ConstantFold);
        prep.add(RedundantPhis);
        let entry = fg.entry().unwrap();
        prep.run(&mut fg, entry)?;
    }

    // 3. manually wire MemProject + MemUnion around the Store
    //         so the Load's memory input goes through MemUnion rather than
    //         directly to Store.
    //
    //         Before manual wiring (after ConstantFold + RedundantPhis):
    //           InitialMemory → Store(sp+4) → Load[mem]
    //         After manual wiring:
    //           InitialMemory → MemProject(Stack) → Store(sp+4) → MemUnion → Load[mem]
    {
        // Locate nodes before any mutation.
        let im_node = fg
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::InitialMemory))
            .expect("InitialMemory must exist");
        let ss_node = fg
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Store(_)))
            .expect("Store must exist");
        let load_node = fg
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
            .expect("Load must exist");

        let [im_out] = fg.node_outputs_exact::<1>(im_node).unwrap();

        // Create MemProject consuming InitialMemory output.
        // Output slot 0 = Stack, slot 1 = Unknown.
        let part_node = fg.create_node_attributed(
            NodeKind::MemProject,
            [im_out],
            [
                NodeOutputKind::Memory(Some(AliasClass::Stack)),
                NodeOutputKind::Memory(Some(AliasClass::Unknown)),
            ],
            &[im_node],
        );
        // Use the Stack output (slot 0).
        let part_out = fg.node_outputs(part_node).to_vec()[0];

        // Rewire Store's memory input (slot 0) to consume MemProject
        // output instead of InitialMemory output.
        let ss_mem_input_id = fg.graph().node_input_id_at(ss_node, 0).unwrap();
        fg.graph_mut().update_input(ss_mem_input_id, part_out);

        // Retype Store's memory output to Memory(Some(AliasClass::Stack)).
        let ss_mem_out: NodeOutputId = fg.graph().memory_output_of(ss_node).unwrap();
        fg.graph_mut()
            .set_memory_partition(ss_mem_out, Some(AliasClass::Stack))
            .unwrap();

        // Create MemUnion consuming the Store's partition-typed output.
        // Output: Memory(None) (unified).
        let union_node = fg.create_node_attributed(
            NodeKind::MemUnion,
            [ss_mem_out],
            [NodeOutputKind::Memory(None)],
            &[ss_node],
        );
        let [union_out] = fg.node_outputs_exact::<1>(union_node).unwrap();

        // Rewire the Load's memory input to consume MemUnion output.
        let load_mem_input_id = fg.graph().node_input_id_at(load_node, 0).unwrap();
        fg.graph_mut().update_input(load_mem_input_id, union_out);
    }

    // 4. confirm the graph validates before we forward.
    let entry = fg.entry().unwrap();
    strider_ir::validate::validate(&fg, entry)
        .expect("manually-wired graph must pass IR validation before forward pass");

    // 5. run StackLoadForward.  Without the MemUnion arm the load
    //         would stay; with it the backward walk routes through the
    //         MemUnion to Store(sp+4) and forwards 0xBEEF.
    let fwd = StackLoadForward::new(sp, Endianness::Little);
    fwd.optimize(&mut fg, entry)?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 0,
        "Load[sp+4] must be forwarded through the MemUnion boundary"
    );

    let ret_kind = crate::opt::test_support::return_kind(fg.graph())?;
    assert!(
        matches!(ret_kind, NodeKind::IntConst(0xBEEF)),
        "forwarded value must be IntConst(0xBEEF), got {ret_kind:?}"
    );
    Ok(())
}

/// Regression: StackLoadForward must NOT forward a Stack-class Load across a
/// LOCK barrier.
///
/// Before the soundness fix, LOCK was `PureMem` (MEM_CLOBBER_HEAP_UNKNOWN),
/// which placed it only on the Unknown chain.  AliasSplit would build:
///
///   Stack chain:  MemProject[Stack] → Store(sp-4) ──────────────→ Load(sp-4)
///   Unknown chain: MemProject[Unknown] → LOCK(mem-clobber) → MemProject[Unknown]
///
/// and StackLoadForward would forward Load(sp-4) directly from Store(sp-4),
/// bypassing LOCK — unsound when another CPU could have modified the stack
/// slot via the locked operation.
///
/// After the fix, LOCK is `FullClobber` (MEM_CLOBBER_FULL), so AliasSplit
/// inserts a MemProject[Stack] RE-PROJECTION after LOCK on the Stack chain:
///
///   Stack chain:  MP[Stack] → Store(sp-4) → MemUnion → LOCK → MP[Stack] → Load(sp-4)
///
/// StackLoadForward's backward walk stops at LOCK's re-projection boundary
/// and the Load remains (cannot forward across a full-clobber barrier).
#[test]
fn lock_barrier_prevents_stack_load_forwarding() -> Result<()> {
    use strider_ir_test_utils::SENTINEL_LIFT_ADDR;

    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr = b.build_int_sub(sp_val, four, NodeOutputType::U32)?;
        let data = b.build_int_const(0x99u64, NodeOutputType::U32)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        // Emit a LOCK CallOther.  LOCK is now FullClobber, so AliasSplit
        // must break the Stack chain here.
        let (lock_node, _v, _w) = b.build_call_other_modeled(
            0x1234, "LOCK", &[], None, &[], &[], &[],
        )?;
        let lock_mem_out = b.graph().memory_output_of(lock_node)?;
        b.advance_cur_region_memory(lock_mem_out)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    // Pipeline: ConstantFold → RedundantPhis → AliasSplit → StackLoadForward.
    // AliasSplit must break the Stack chain at LOCK (FullClobber).
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(AliasSplit::new(sp, strider_target::ArchPreset::X86));
    pipeline.add(StackLoadForward::new(sp, Endianness::Little));

    let entry = fg.entry().unwrap();
    pipeline.run(&mut fg, entry)?;

    // The Load must NOT be forwarded — LOCK is a full-clobber barrier.
    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 1,
        "Load[sp-4] must NOT be forwarded across a LOCK barrier; \
         LOCK is FullClobber and breaks the Stack chain"
    );
    Ok(())
}
