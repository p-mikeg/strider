use super::*;
use crate::error::Result;
use ir::node::{NodeKind, NodeOutputType};
use ir::{FunctionBuilder, IntBinaryOp};

/// Fake 4-byte SP varnode (x86-cdecl-like).
fn sp32_vn() -> rsleigh::Vn {
    rsleigh::Vn {
        addr: rsleigh::VnAddr {
            space: rsleigh::VnSpace::REGISTER,
            off: 0x20,
        },
        size: 4,
    }
}

/// 8-byte SP for aarch64/x86-64-like scenarios.
fn sp64_vn() -> rsleigh::Vn {
    rsleigh::Vn {
        addr: rsleigh::VnAddr {
            space: rsleigh::VnSpace::REGISTER,
            off: 0x40,
        },
        size: 8,
    }
}

fn reachable_count<F: Fn(&NodeKind) -> bool>(fg: &BuiltFunctionGraph, pred: F) -> usize {
    let reachable: std::collections::HashSet<_> = fg.preorder().collect();
    fg.all_node_ids()
        .filter(|n| reachable.contains(n))
        .filter(|&n| pred(fg.graph.node_kind(n)))
        .count()
}

/// Direct forward: `*(sp+4) = 0x11; return *(sp+4)` — the load vanishes
/// and the return sources from the stored constant.
#[test]
fn forward_basic() -> Result<()> {
    use crate::{ConstantFold, OptimizerPipeline, RedundantPhis, StackStoreDetect};

    let sp = sp32_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    let sp_val = b.read_variable(&sp)?;
    let four = b.build_int_const(4, NodeOutputType::U32);
    let addr =
        b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, NodeOutputType::U32)?;
    let data = b.build_int_const(0x11, NodeOutputType::U32);
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add(StackLoadForward::new(sp));
    pipeline.run(&mut fg)?;

    let reachable_loads = reachable_count(&fg, |k| matches!(k, NodeKind::Load(_)));
    assert_eq!(reachable_loads, 0, "Load[sp+4] should be forwarded away");
    Ok(())
}

/// A non-aliasing store at a different offset sits between the target
/// store and the load.  The walker must step past it and still forward
/// the earlier `StackStore{+4}`'s value to the load.
#[test]
fn forward_skips_non_aliasing_store() -> Result<()> {
    use crate::{ConstantFold, OptimizerPipeline, RedundantPhis, StackStoreDetect};

    let sp = sp32_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    let sp_val = b.read_variable(&sp)?;
    let four = b.build_int_const(4, NodeOutputType::U32);
    let twelve = b.build_int_const(12, NodeOutputType::U32);
    let addr4 =
        b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, NodeOutputType::U32)?;
    let addr12 =
        b.build_int_binary_operation(sp_val, twelve, IntBinaryOp::Add, NodeOutputType::U32)?;
    let a = b.build_int_const(0xAA, NodeOutputType::U32);
    let b_val = b.build_int_const(0xBB, NodeOutputType::U32);
    // Order: store at +4 first, then +12, then load +4.  The load's
    // memory input chain is store12 -> store4 -> InitialMemory.
    b.build_store(addr4, b_val, rsleigh::VnSpace::RAM)?;
    b.build_store(addr12, a, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add(StackLoadForward::new(sp));
    pipeline.run(&mut fg)?;

    let reachable_loads = reachable_count(&fg, |k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 0,
        "Load[sp+4] should forward past the non-aliasing StackStore{{+12}}"
    );
    Ok(())
}

/// Overlap case: `*(sp+0) = U64(...); return *(sp+4) as U32` — the store
/// covers `[0, 8)` which intersects the load's `[4, 8)`, so forwarding
/// must bail and the load must remain.
#[test]
fn bail_on_overlapping_store() -> Result<()> {
    use crate::{ConstantFold, OptimizerPipeline, RedundantPhis, StackStoreDetect};

    let sp = sp64_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    let sp_val = b.read_variable(&sp)?;
    let four = b.build_int_const(4, NodeOutputType::U64);
    let addr4 =
        b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, NodeOutputType::U64)?;
    // Store U64 at sp+0 (covers [0,8)), then load U32 from sp+4 ([4,8)).
    let wide_data = b.build_int_const(0xDEAD_BEEF_CAFE_BABE, NodeOutputType::U64);
    b.build_store(sp_val, wide_data, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add(StackLoadForward::new(sp));
    pipeline.run(&mut fg)?;

    let reachable_loads = reachable_count(&fg, |k| matches!(k, NodeKind::Load(_)));
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
    use crate::{ConstantFold, OptimizerPipeline, RedundantPhis, StackStoreDetect};

    let sp = sp64_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    let sp_val = b.read_variable(&sp)?;
    let narrow = b.build_int_const(0x11, NodeOutputType::U32);
    b.build_store(sp_val, narrow, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(sp_val, rsleigh::VnSpace::RAM, NodeOutputType::U64)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add(StackLoadForward::new(sp));
    pipeline.run(&mut fg)?;

    let reachable_loads = reachable_count(&fg, |k| matches!(k, NodeKind::Load(_)));
    assert_eq!(reachable_loads, 1, "type mismatch must prevent forwarding");
    Ok(())
}

/// An intervening `Store(_)` whose address is *not* SP-relative cannot be
/// proven non-aliasing in general — conservatively bail even though in
/// practice it can't overlap the stack slot.
#[test]
fn bail_on_opaque_store_between() -> Result<()> {
    use crate::{ConstantFold, OptimizerPipeline, RedundantPhis, StackStoreDetect};

    let sp = sp32_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    let sp_val = b.read_variable(&sp)?;
    let four = b.build_int_const(4, NodeOutputType::U32);
    let addr4 =
        b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, NodeOutputType::U32)?;
    let a = b.build_int_const(0xAA, NodeOutputType::U32);
    b.build_store(addr4, a, rsleigh::VnSpace::RAM)?;
    // Opaque store to a non-SP address (a compile-time constant address).
    let heap_addr = b.build_int_const(0x1000, NodeOutputType::U32);
    let other = b.build_int_const(0xBB, NodeOutputType::U32);
    b.build_store(heap_addr, other, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add(StackLoadForward::new(sp));
    pipeline.run(&mut fg)?;

    let reachable_loads = reachable_count(&fg, |k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 1,
        "opaque intervening Store must prevent forwarding"
    );
    Ok(())
}

/// A call between store and load clobbers memory (via `PostCallMemState`);
/// forwarding across it is unsafe, so the load must remain.  Uses a
/// link-register-style convention (ret_stack_pop=0) so SP stays stable
/// through the call, keeping the load's address decomposable.
#[test]
fn bail_on_call_between() -> Result<()> {
    use crate::{ConstantFold, OptimizerPipeline, RedundantPhis, StackStoreDetect};

    let sp = sp64_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], Some(sp), 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    let sp_val = b.read_variable(&sp)?;
    let four = b.build_int_const(4, NodeOutputType::U64);
    let addr4 =
        b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, NodeOutputType::U64)?;
    let data = b.build_int_const(0x11, NodeOutputType::U32);
    b.build_store(addr4, data, rsleigh::VnSpace::RAM)?;
    let target = b.build_int_const(0x1000, NodeOutputType::U64);
    b.build_call(target)?;
    // SP did not shift (ret_stack_pop=0), so sp+4 is still the same slot.
    let sp_val2 = b.read_variable(&sp)?;
    let addr4b =
        b.build_int_binary_operation(sp_val2, four, IntBinaryOp::Add, NodeOutputType::U64)?;
    let loaded = b.build_load(addr4b, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add(StackLoadForward::new(sp));
    pipeline.run(&mut fg)?;

    let reachable_loads = reachable_count(&fg, |k| matches!(k, NodeKind::Load(_)));
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
/// `MemPhi` (i.e. the merge `ControlState`'s dispatch output).
#[test]
fn phi_both_branches_store_same_offset() -> Result<()> {
    use crate::{ConstantFold, OptimizerPipeline, RedundantPhis, StackStoreDetect};

    let sp = sp32_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
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
    let four_t = b.build_int_const(4, NodeOutputType::U32);
    let addr_t =
        b.build_int_binary_operation(sp_t, four_t, IntBinaryOp::Add, NodeOutputType::U32)?;
    let a = b.build_int_const(0xAA, NodeOutputType::U32);
    b.build_store(addr_t, a, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    // else: *(sp+4) = 0xBB; goto merge
    b.set_region(else_r);
    let sp_e = b.read_variable(&sp)?;
    let four_e = b.build_int_const(4, NodeOutputType::U32);
    let addr_e =
        b.build_int_binary_operation(sp_e, four_e, IntBinaryOp::Add, NodeOutputType::U32)?;
    let bval = b.build_int_const(0xBB, NodeOutputType::U32);
    b.build_store(addr_e, bval, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    // merge: return *(sp+4)
    b.set_region(merge);
    let sp_m = b.read_variable(&sp)?;
    let four_m = b.build_int_const(4, NodeOutputType::U32);
    let addr_m =
        b.build_int_binary_operation(sp_m, four_m, IntBinaryOp::Add, NodeOutputType::U32)?;
    let loaded = b.build_load(addr_m, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    // Skip DeadBranchElimination so the `If(const true)` diamond survives
    // through the pass — otherwise both arms would collapse and there'd
    // be no MemPhi to synthesize a ValuePhi from.
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add(StackLoadForward::new(sp));
    pipeline.run(&mut fg)?;

    let reachable_loads = reachable_count(&fg, |k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 0,
        "Load at merge must be forwarded via synthesized ValuePhi"
    );
    let reachable_value_phis = reachable_count(&fg, |k| matches!(k, NodeKind::ValuePhi));
    assert_eq!(
        reachable_value_phis, 1,
        "exactly one ValuePhi must be synthesized"
    );

    // The ValuePhi's phi-token (input 0) must come from the same
    // ControlState as the MemPhi's phi-token.
    let reachable: std::collections::HashSet<_> = fg.preorder().collect();
    let value_phi = fg
        .all_node_ids()
        .find(|n| reachable.contains(n) && matches!(fg.graph.node_kind(*n), NodeKind::ValuePhi))
        .expect("ValuePhi found above");
    let mem_phi = fg
        .all_node_ids()
        .find(|n| reachable.contains(n) && matches!(fg.graph.node_kind(*n), NodeKind::MemPhi))
        .expect("MemPhi survived to the merge");
    let vp_token = fg.graph.node_inputs(value_phi)[0];
    let mp_token = fg.graph.node_inputs(mem_phi)[0];
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
    use crate::{ConstantFold, OptimizerPipeline, RedundantPhis, StackStoreDetect};

    let sp = sp32_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
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
    let four_t = b.build_int_const(4, NodeOutputType::U32);
    let addr_t =
        b.build_int_binary_operation(sp_t, four_t, IntBinaryOp::Add, NodeOutputType::U32)?;
    let a = b.build_int_const(0xAA, NodeOutputType::U32);
    b.build_store(addr_t, a, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    // else: no store — falls through with unchanged memory
    b.set_region(else_r);
    b.build_branch(merge)?;

    b.set_region(merge);
    let sp_m = b.read_variable(&sp)?;
    let four_m = b.build_int_const(4, NodeOutputType::U32);
    let addr_m =
        b.build_int_binary_operation(sp_m, four_m, IntBinaryOp::Add, NodeOutputType::U32)?;
    let loaded = b.build_load(addr_m, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add(StackLoadForward::new(sp));
    pipeline.run(&mut fg)?;

    let reachable_loads = reachable_count(&fg, |k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 1,
        "missing-store branch must prevent forwarding"
    );
    let reachable_value_phis = reachable_count(&fg, |k| matches!(k, NodeKind::ValuePhi));
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
/// resolver walks still bottom out on the same `StackStore{+4}`'s data.
/// The dedup path in `resolve()` must then skip the ValuePhi synthesis
/// and return the shared data output directly.
#[test]
fn phi_identical_values_no_new_phi() -> Result<()> {
    use crate::{ConstantFold, OptimizerPipeline, RedundantPhis, StackStoreDetect};

    let sp = sp32_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let entry = b.create_region()?;
    let then_r = b.create_region()?;
    let else_r = b.create_region()?;
    let merge = b.create_region()?;
    b.set_entry_region(entry)?;

    // entry: *(sp+4) = 0xAA; then if(true) goto then else goto else
    b.set_region(entry);
    let sp_e = b.read_variable(&sp)?;
    let four_e = b.build_int_const(4, NodeOutputType::U32);
    let addr_e =
        b.build_int_binary_operation(sp_e, four_e, IntBinaryOp::Add, NodeOutputType::U32)?;
    let shared = b.build_int_const(0xAA, NodeOutputType::U32);
    b.build_store(addr_e, shared, rsleigh::VnSpace::RAM)?;
    let cond = b.build_boolean_const(true);
    b.build_if(cond, then_r, else_r)?;

    // then: *(sp+8) = 0xBB; branch merge
    b.set_region(then_r);
    let sp_t = b.read_variable(&sp)?;
    let eight_t = b.build_int_const(8, NodeOutputType::U32);
    let addr_t =
        b.build_int_binary_operation(sp_t, eight_t, IntBinaryOp::Add, NodeOutputType::U32)?;
    let bt = b.build_int_const(0xBB, NodeOutputType::U32);
    b.build_store(addr_t, bt, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    // else: *(sp+12) = 0xCC; branch merge
    b.set_region(else_r);
    let sp_l = b.read_variable(&sp)?;
    let twelve_l = b.build_int_const(12, NodeOutputType::U32);
    let addr_l =
        b.build_int_binary_operation(sp_l, twelve_l, IntBinaryOp::Add, NodeOutputType::U32)?;
    let cc = b.build_int_const(0xCC, NodeOutputType::U32);
    b.build_store(addr_l, cc, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    // merge: return *(sp+4)
    b.set_region(merge);
    let sp_m = b.read_variable(&sp)?;
    let four_m = b.build_int_const(4, NodeOutputType::U32);
    let addr_m =
        b.build_int_binary_operation(sp_m, four_m, IntBinaryOp::Add, NodeOutputType::U32)?;
    let loaded = b.build_load(addr_m, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add(StackLoadForward::new(sp));
    pipeline.run(&mut fg)?;

    let reachable_loads = reachable_count(&fg, |k| matches!(k, NodeKind::Load(_)));
    assert_eq!(reachable_loads, 0, "Load must be forwarded");
    let reachable_value_phis = reachable_count(&fg, |k| matches!(k, NodeKind::ValuePhi));
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
    use crate::{OptimizerPipeline, RedundantPhis, StackStoreDetect};

    let sp = sp32_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    let sp_val = b.read_variable(&sp)?;
    let four = b.build_int_const(4, NodeOutputType::U32);
    let store_addr =
        b.build_int_binary_operation(sp_val, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
    let data = b.build_int_const(0x4242, NodeOutputType::U32);
    b.build_store(store_addr, data, rsleigh::VnSpace::RAM)?;

    let neg_four = b.build_int_const(0xFFFF_FFFC, NodeOutputType::U32);
    let load_addr =
        b.build_int_binary_operation(sp_val, neg_four, IntBinaryOp::Add, NodeOutputType::U32)?;
    let loaded = b.build_load(load_addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    // Intentionally omit `ConstantFold` so both encodings reach
    // `decompose_sp` as-lifted.
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add(StackLoadForward::new(sp));
    pipeline.run(&mut fg)?;

    let reachable_loads = reachable_count(&fg, |k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 0,
        "Load[Add(sp, 0xFFFFFFFC)] must be forwarded from Store[Sub(sp, 4)]",
    );
    let ret = fg
        .all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
        .expect("return node exists");
    let ret_inputs = fg.graph.node_inputs(ret);
    // Return inputs: [ctrl, mem, val_0, ...].
    let val_producer = fg.graph.get_node_from_output(ret_inputs[2]);
    assert!(
        matches!(fg.graph.node_kind(val_producer), NodeKind::IntConst(0x4242)),
        "forwarded value must be the stored constant 0x4242 — got {:?}",
        fg.graph.node_kind(val_producer),
    );
    Ok(())
}

/// Real-world pattern from `binary_tests/test.c::struct_test` at `-O0 -m32`:
/// the prologue spills a callee-saved register / arg to a 4-byte stack slot
/// via `StackStore u32`, then the body reads a single byte of that slot via
/// `Load u8` at the same SP offset.  The load is narrower than the store,
/// but its bytes are fully contained in the stored value — forwarding must
/// emit a `Truncate(stored_u32, u8)` (which `ConstantFold` then folds to a
/// byte constant when the stored value is itself a constant).
#[test]
fn narrow_load_from_wider_store_forwards_via_truncate() -> Result<()> {
    use crate::{ConstantFold, OptimizerPipeline, RedundantPhis, StackStoreDetect};

    let sp = sp32_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    let sp_val = b.read_variable(&sp)?;
    let eight = b.build_int_const(8, NodeOutputType::U32);
    let addr =
        b.build_int_binary_operation(sp_val, eight, IntBinaryOp::Sub, NodeOutputType::U32)?;
    // Store the full 4-byte value, then load only the low byte.
    let wide = b.build_int_const(0xDEAD_BEEF, NodeOutputType::U32);
    b.build_store(addr, wide, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U8)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add(StackLoadForward::new(sp));
    pipeline.run(&mut fg)?;

    let reachable_loads = reachable_count(&fg, |k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 0,
        "Load u8 at matching offset must be forwarded as the low byte of the u32 store",
    );
    let ret = fg
        .all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
        .expect("return node exists");
    let ret_inputs = fg.graph.node_inputs(ret);
    // `int_const_val` applies the output type's mask, so for a U8 output it
    // returns the low byte even when the backing `IntConst` node still
    // carries the full u32 bit-pattern internally.
    let val_ty = fg.graph.output_kind(ret_inputs[2]).as_value();
    assert_eq!(val_ty, Some(NodeOutputType::U8));
    assert_eq!(
        fg.int_const_val(ret_inputs[2]),
        Some(0xEF),
        "forwarded narrow load must fold to the low byte 0xEF",
    );
    Ok(())
}

/// As above but the load takes two bytes: `Load u16` at the matching
/// offset of a `StackStore u32`.  Folds to the low 16 bits of the stored
/// constant — `0xBEEF` for `0xDEADBEEF`.  Guards the u16 case independently
/// because the analyzer emits both for `struct_test`'s short/char reads.
#[test]
fn narrow_load_u16_from_u32_store_forwards_via_truncate() -> Result<()> {
    use crate::{ConstantFold, OptimizerPipeline, RedundantPhis, StackStoreDetect};

    let sp = sp32_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    let sp_val = b.read_variable(&sp)?;
    let twelve = b.build_int_const(12, NodeOutputType::U32);
    let addr =
        b.build_int_binary_operation(sp_val, twelve, IntBinaryOp::Sub, NodeOutputType::U32)?;
    let wide = b.build_int_const(0xDEAD_BEEF, NodeOutputType::U32);
    b.build_store(addr, wide, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U16)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add(StackLoadForward::new(sp));
    pipeline.run(&mut fg)?;

    let reachable_loads = reachable_count(&fg, |k| matches!(k, NodeKind::Load(_)));
    assert_eq!(reachable_loads, 0, "Load u16 must be forwarded");
    let ret = fg
        .all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
        .expect("return node exists");
    let ret_inputs = fg.graph.node_inputs(ret);
    let val_ty = fg.graph.output_kind(ret_inputs[2]).as_value();
    assert_eq!(val_ty, Some(NodeOutputType::U16));
    assert_eq!(
        fg.int_const_val(ret_inputs[2]),
        Some(0xBEEF),
        "forwarded u16 load must fold to low 16 bits 0xBEEF",
    );
    Ok(())
}
