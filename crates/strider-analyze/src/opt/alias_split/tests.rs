//! White-box tests for [`crate::opt::AliasSplit`].
//!
//! Each test builds a small synthetic function exhibiting a specific
//! memory-chain shape, runs `AliasSplit` once, and pins the resulting
//! IR structure (presence/absence of `MemPartition` / `MemUnion` nodes
//! and the partition-typed memory edges).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir::{AliasClass, Function};
use strider_ir_test_utils::{make_sp_fn, sp_vn_x86, SENTINEL_LIFT_ADDR};

use crate::opt::pipeline::{OptimizationResult, Optimizer};
use crate::opt::AliasSplit;

/// Run AliasSplit once on `function`.
fn run_split(function: &mut Function, sp: rsleigh::Vn) -> OptimizationResult {
    let pass = AliasSplit::new(sp);
    let entry = function.entry().unwrap();
    pass.optimize(function, entry).expect("AliasSplit must not error")
}

/// Count reachable nodes matching `pred`.
fn count_reachable(function: &Function, pred: impl Fn(&NodeKind) -> bool) -> usize {
    function.count_kind(pred)
}

/// Build `store sp-4 = 0x42; load sp-4; return loaded` — one stack
/// Store and one stack Load, then Return.
fn stack_store_load_return(sp: rsleigh::Vn) -> Function {
    make_sp_fn(sp, |b, sp_v| {
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr = b.build_int_sub(sp_v, four, NodeOutputType::U32)?;
        let data = b.build_int_const(0x42u64, NodeOutputType::U32)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })
    .unwrap()
}

/// Build `store unknown_addr = 0x99; return 0` — one unknown-class
/// Store, no SP-relative addressing.
fn unknown_store_return(sp: rsleigh::Vn) -> Function {
    make_sp_fn(sp, |b, _sp_v| {
        // Use a constant address that doesn't decompose to SP+K.
        let addr = b.build_int_const(0x1000u64, NodeOutputType::U32)?;
        let data = b.build_int_const(0x99u64, NodeOutputType::U32)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let zero = b.build_int_const(0u64, NodeOutputType::U32)?;
        b.build_return(Some(zero), &[])?;
        Ok(())
    })
    .unwrap()
}

/// Build `store sp-4 = 0x42; call f; load sp-4; return loaded` —
/// Stack store, Call barrier, Stack load.
fn stack_call_stack_return(sp: rsleigh::Vn) -> Function {
    make_sp_fn(sp, |b, sp_v| {
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr = b.build_int_sub(sp_v, four, NodeOutputType::U32)?;
        let data = b.build_int_const(0x42u64, NodeOutputType::U32)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        let target = b.build_int_const(0xCAFEu64, NodeOutputType::U32)?;
        b.build_call(target)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })
    .unwrap()
}

/// Builds `return 0` with no Loads/Stores.  No memory activity ⇒
/// AliasSplit should be a NoChange (no boundaries inserted).
fn empty_chain_return(sp: rsleigh::Vn) -> Function {
    make_sp_fn(sp, |b, _sp_v| {
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let zero = b.build_int_const(0u64, NodeOutputType::U32)?;
        b.build_return(Some(zero), &[])?;
        Ok(())
    })
    .unwrap()
}

#[test]
fn stack_only_chain_inserts_mempartition_and_memunion() {
    let sp = sp_vn_x86();
    let mut f = stack_store_load_return(sp);

    let r = run_split(&mut f, sp);
    assert_eq!(r, OptimizationResult::Changed);

    // Exactly one MemPartition + one MemUnion present and reachable.
    let n_part = count_reachable(&f, |k| matches!(k, NodeKind::MemPartition { .. }));
    let n_union = count_reachable(&f, |k| matches!(k, NodeKind::MemUnion));
    assert_eq!(n_part, 1, "exactly one MemPartition expected");
    assert_eq!(n_union, 1, "exactly one MemUnion expected (before Return)");

    // The partition table now has exactly one Stack partition.
    let parts: Vec<_> = f.partitions().iter().collect();
    assert_eq!(parts.len(), 1, "exactly one partition created");
    assert_eq!(parts[0].1.alias_class, AliasClass::Stack);

    // Validate IR after rewrite.
    let entry = f.entry().unwrap();
    strider_ir::validate::validate(&f, entry).expect("post-AliasSplit IR must validate");
}

#[test]
fn non_sp_chain_stays_unified() {
    // A Store at an unknown (constant 0x1000) address — should NOT be
    // partitioned in v1.
    let sp = sp_vn_x86();
    let mut f = unknown_store_return(sp);

    let r = run_split(&mut f, sp);
    // No boundaries inserted ⇒ NoChange.
    assert_eq!(r, OptimizationResult::NoChange);

    assert_eq!(count_reachable(&f, |k| matches!(k, NodeKind::MemPartition { .. })), 0);
    assert_eq!(count_reachable(&f, |k| matches!(k, NodeKind::MemUnion)), 0);

    let entry = f.entry().unwrap();
    strider_ir::validate::validate(&f, entry).expect("unmodified IR must still validate");
}

#[test]
fn empty_memory_chain_is_noop() {
    let sp = sp_vn_x86();
    let mut f = empty_chain_return(sp);
    let r = run_split(&mut f, sp);
    assert_eq!(r, OptimizationResult::NoChange);
    assert_eq!(count_reachable(&f, |k| matches!(k, NodeKind::MemPartition { .. })), 0);
    assert_eq!(count_reachable(&f, |k| matches!(k, NodeKind::MemUnion)), 0);
}

#[test]
fn call_in_middle_gets_memunion_before_and_mempartition_after() {
    let sp = sp_vn_x86();
    let mut f = stack_call_stack_return(sp);

    let r = run_split(&mut f, sp);
    assert_eq!(r, OptimizationResult::Changed);

    // Two segments (pre-Call + post-Call), each producing one
    // MemPartition + MemUnion boundary.
    let n_part = count_reachable(&f, |k| matches!(k, NodeKind::MemPartition { .. }));
    let n_union = count_reachable(&f, |k| matches!(k, NodeKind::MemUnion));
    assert_eq!(n_part, 2, "MemPartition before each Stack segment");
    assert_eq!(n_union, 2, "MemUnion before Call and before Return");

    // Two Stack partitions created (one per segment in v1; reusing
    // the same partition across segments is a follow-up).
    let parts: Vec<_> = f.partitions().iter().collect();
    assert_eq!(parts.len(), 2, "two Stack partitions");
    for (_, info) in parts {
        assert_eq!(info.alias_class, AliasClass::Stack);
    }

    let entry = f.entry().unwrap();
    strider_ir::validate::validate(&f, entry).expect("post-Call segment IR must validate");
}

#[test]
fn idempotent_on_already_partitioned_ir() {
    let sp = sp_vn_x86();
    let mut f = stack_store_load_return(sp);

    let r1 = run_split(&mut f, sp);
    assert_eq!(r1, OptimizationResult::Changed);

    // Second run must observe pre-existing MemPartition / MemUnion and
    // bail.
    let r2 = run_split(&mut f, sp);
    assert_eq!(r2, OptimizationResult::NoChange);
}

#[test]
fn unknown_then_stack_segment_left_unified_due_to_bail() {
    // An Unknown Store in the chain taints the whole segment under
    // v1's conservative bail semantics.  Validates the bail path.
    let sp = sp_vn_x86();
    let mut f = make_sp_fn(sp, |b, sp_v| {
        // Unknown-addr Store first.
        let unk_addr = b.build_int_const(0x1000u64, NodeOutputType::U32)?;
        let v1 = b.build_int_const(1u64, NodeOutputType::U32)?;
        b.build_store(unk_addr, v1, rsleigh::VnSpace::RAM)?;
        // Stack Store second.
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr = b.build_int_sub(sp_v, four, NodeOutputType::U32)?;
        let v2 = b.build_int_const(0x42u64, NodeOutputType::U32)?;
        b.build_store(addr, v2, rsleigh::VnSpace::RAM)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let zero = b.build_int_const(0u64, NodeOutputType::U32)?;
        b.build_return(Some(zero), &[])?;
        Ok(())
    })
    .unwrap();

    let r = run_split(&mut f, sp);
    // v1 bail-on-Unknown is conservative ⇒ no boundaries inserted.
    assert_eq!(r, OptimizationResult::NoChange);
    assert_eq!(count_reachable(&f, |k| matches!(k, NodeKind::MemPartition { .. })), 0);
    assert_eq!(count_reachable(&f, |k| matches!(k, NodeKind::MemUnion)), 0);
}

#[test]
fn indirect_branch_gets_memunion_before() {
    // store sp-4 = 0x42; indirect_branch(target) — IndirectBranch
    // acts as a barrier: a MemUnion gets spliced in front of it.
    let sp = sp_vn_x86();
    let mut f = make_sp_fn(sp, |b, sp_v| {
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr = b.build_int_sub(sp_v, four, NodeOutputType::U32)?;
        let data = b.build_int_const(0x42u64, NodeOutputType::U32)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        let target = b.build_int_const(0xCAFEu64, NodeOutputType::U32)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        b.build_indirect_branch(target)?;
        Ok(())
    })
    .unwrap();

    let r = run_split(&mut f, sp);
    assert_eq!(r, OptimizationResult::Changed);
    assert_eq!(
        count_reachable(&f, |k| matches!(k, NodeKind::MemPartition { .. })),
        1
    );
    assert_eq!(count_reachable(&f, |k| matches!(k, NodeKind::MemUnion)), 1);

    // IndirectBranch is the only barrier; its memory input now
    // consumes a MemUnion's output.
    let ib = f
        .all_node_ids()
        .find(|&n| matches!(f.node_kind(n), NodeKind::IndirectBranch))
        .expect("IndirectBranch present");
    // IndirectBranch inputs: [control, memory, target]
    let mem_in = f.node_inputs(ib).into_iter().nth(1).unwrap();
    let mem_in_producer = f.kind_of_output(mem_in);
    assert!(
        matches!(mem_in_producer, NodeKind::MemUnion),
        "IndirectBranch.memory_in must be a MemUnion output, got {mem_in_producer:?}"
    );

    let entry = f.entry().unwrap();
    strider_ir::validate::validate(&f, entry).expect("post-AliasSplit IR with IndirectBranch must validate");
}

#[test]
fn callother_with_memory_edge_gets_memunion() {
    use strider_ir::node::NodeKind as NK;
    let sp = sp_vn_x86();
    // build_call_other_modeled emits a CallOther node that takes the
    // current region's memory as an input (so it's seen as a barrier
    // by AliasSplit).  Note: the test-utils builder does NOT advance
    // the region's memory past the CallOther — the post-CallOther
    // Load/Return still consume the Store's memory output.  This
    // means the function has a single segment with two barriers
    // (CallOther + Return), both of which need MemUnion-wrapping.
    let mut f = make_sp_fn(sp, |b, sp_v| {
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr = b.build_int_sub(sp_v, four, NodeOutputType::U32)?;
        let data = b.build_int_const(0x42u64, NodeOutputType::U32)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        b.build_call_other_modeled(0xCAFE, "fake_op", &[], None, &[], &[], &[])?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })
    .unwrap();

    let r = run_split(&mut f, sp);
    assert_eq!(r, OptimizationResult::Changed);

    // One Stack segment containing Store + Load, terminated by both
    // CallOther and Return barriers.
    assert_eq!(
        count_reachable(&f, |k| matches!(k, NK::MemPartition { .. })),
        1,
        "one MemPartition at segment entry"
    );
    assert_eq!(
        count_reachable(&f, |k| matches!(k, NK::MemUnion)),
        2,
        "two MemUnion (one before CallOther, one before Return)"
    );

    // CallOther's memory input is a MemUnion output (the barrier
    // contract).
    let co = f
        .all_node_ids()
        .find(|&n| matches!(f.node_kind(n), NodeKind::CallOther { .. }))
        .expect("CallOther present");
    let mem_in = f.node_inputs(co).into_iter().nth(1).unwrap();
    assert!(
        matches!(f.kind_of_output(mem_in), NodeKind::MemUnion),
        "CallOther.memory_in must be a MemUnion output"
    );

    let entry = f.entry().unwrap();
    strider_ir::validate::validate(&f, entry).expect("post-AliasSplit IR with CallOther must validate");
}

#[test]
fn pipeline_with_alias_split_validates() {
    // Smoke test: AliasSplit running inside a tiny pipeline doesn't
    // break the post-pipeline validator.
    use crate::opt::{ConstantFold, OptimizerPipeline};

    let sp = sp_vn_x86();
    let mut f = stack_store_load_return(sp);
    let entry = f.entry().unwrap();

    let mut p = OptimizerPipeline::new();
    p.add(ConstantFold);
    p.add(AliasSplit::new(sp));
    p.run(&mut f, entry).expect("pipeline must converge & validate");
}
