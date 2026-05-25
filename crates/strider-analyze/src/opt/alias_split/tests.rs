//! White-box tests for [`crate::opt::AliasSplit`] (the forked
//! per-partition Memory-SSA rewrite).
//!
//! Each test builds a small synthetic function exhibiting a specific
//! memory-chain shape, runs `AliasSplit` once, and pins the resulting
//! IR structure (presence/absence of `MemPartition` / `MemUnion`
//! nodes, the partition-typed memory edges, and — critically — the
//! per-partition bypass shape that makes a disjoint-partition store
//! flow through a `Call` or skip over an unrelated-partition store).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use strider_ir::node::{NodeKind, NodeOutputKind, NodeOutputType};
use strider_ir::{AliasClass, Function};
use strider_ir_test_utils::{SENTINEL_LIFT_ADDR, make_sp_fn, sp_vn_x86};
use strider_target::ArchPreset;

use crate::opt::AliasSplit;
use crate::opt::pipeline::{OptimizationResult, Optimizer};

/// Run AliasSplit once on `function` for the x86 preset.
fn run_split(function: &mut Function, sp: rsleigh::Vn) -> OptimizationResult {
    let pass = AliasSplit::new(sp, ArchPreset::X86);
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

/// `Return 0` with no Loads/Stores at all.
fn empty_chain_return(sp: rsleigh::Vn) -> Function {
    make_sp_fn(sp, |b, _sp_v| {
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let zero = b.build_int_const(0u64, NodeOutputType::U32)?;
        b.build_return(Some(zero), &[])?;
        Ok(())
    })
    .unwrap()
}

/// Helper: find the unique node of `kind`-matching predicate.
fn unique_node(
    function: &Function,
    pred: impl Fn(&NodeKind) -> bool,
) -> strider_ir::node::NodeId {
    let mut iter = function.all_node_ids().filter(|&n| pred(function.node_kind(n)));
    let first = iter.next().expect("at least one matching node");
    assert!(iter.next().is_none(), "more than one matching node");
    first
}

/// Helper: collect the AliasClass of every reachable MemPartition.
fn mem_partition_classes(function: &Function) -> Vec<AliasClass> {
    function
        .preorder_kind(|k| matches!(k, NodeKind::MemPartition { .. }))
        .map(|n| match function.node_kind(n) {
            NodeKind::MemPartition { class } => *class,
            _ => unreachable!(),
        })
        .collect()
}

// ─── Entry-partition shape ────────────────────────────────────────────────

#[test]
fn entry_projects_all_three_active_partitions() {
    // Stack-only function: the pass should still project Stack, Heap,
    // AND Unknown from InitialMemory at entry — over-projection is
    // sound and lets a later pass add a non-stack consumer without
    // re-running AliasSplit.
    let sp = sp_vn_x86();
    let mut f = stack_store_load_return(sp);

    let r = run_split(&mut f, sp);
    assert_eq!(r, OptimizationResult::Changed);

    use std::collections::HashSet;
    let classes: HashSet<AliasClass> = mem_partition_classes(&f).into_iter().collect();
    assert!(classes.contains(&AliasClass::Stack), "Stack partition projected");
    assert!(classes.contains(&AliasClass::Heap), "Heap partition projected");
    assert!(classes.contains(&AliasClass::Unknown), "Unknown partition projected");

    let entry = f.entry().unwrap();
    strider_ir::validate::validate(&f, entry)
        .expect("post-AliasSplit IR must validate");
}

#[test]
fn empty_chain_with_return_partitions_for_terminator() {
    // No Stores/Loads/Calls but Return consumes the memory chain.
    // Pass should still fire (terminator clobbers all partitions).
    let sp = sp_vn_x86();
    let mut f = empty_chain_return(sp);
    let r = run_split(&mut f, sp);
    assert_eq!(r, OptimizationResult::Changed);

    // 3 entry MemPartitions + 1 MemUnion at Return.
    let n_part = count_reachable(&f, |k| matches!(k, NodeKind::MemPartition { .. }));
    let n_union = count_reachable(&f, |k| matches!(k, NodeKind::MemUnion));
    assert_eq!(n_part, 3, "3 entry MemPartitions (Stack/Heap/Unknown)");
    assert_eq!(n_union, 1, "1 MemUnion at Return");

    let entry = f.entry().unwrap();
    strider_ir::validate::validate(&f, entry).expect("must validate");
}

// ─── Forked chain shape — the headline new behaviour ──────────────────────

/// Build `store sp+0 = a; store unk = b; store sp+4 = c; return` —
/// the canonical a→b→c example where the Stack chain skips the
/// non-stack store.  v1 classifies non-SP addresses as Unknown.
fn forked_disjoint_chain(sp: rsleigh::Vn) -> Function {
    make_sp_fn(sp, |b, sp_v| {
        // a: Stack store at sp+0
        let a_data = b.build_int_const(0x11u64, NodeOutputType::U32)?;
        b.build_store(sp_v, a_data, rsleigh::VnSpace::RAM)?;
        // b: Unknown store at constant 0x1000
        let b_addr = b.build_int_const(0x1000u64, NodeOutputType::U32)?;
        let b_data = b.build_int_const(0x22u64, NodeOutputType::U32)?;
        b.build_store(b_addr, b_data, rsleigh::VnSpace::RAM)?;
        // c: Stack store at sp+4
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let c_addr = b.build_int_binary_operation(
            sp_v,
            four,
            strider_ir::IntBinaryOp::Add,
            NodeOutputType::U32,
        )?;
        let c_data = b.build_int_const(0x33u64, NodeOutputType::U32)?;
        b.build_store(c_addr, c_data, rsleigh::VnSpace::RAM)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let zero = b.build_int_const(0u64, NodeOutputType::U32)?;
        b.build_return(Some(zero), &[])?;
        Ok(())
    })
    .unwrap()
}

#[test]
fn forked_chains_skip_other_partition() {
    // a (Stack), b (Unknown), c (Stack).  c's stack-chain
    // predecessor must trace back to a (not through b) because the
    // Stack chain bypasses the Unknown chain.
    let sp = sp_vn_x86();
    let mut f = forked_disjoint_chain(sp);
    let r = run_split(&mut f, sp);
    assert_eq!(r, OptimizationResult::Changed);

    // Three reachable Stores; each is now partition-typed.
    let stores: Vec<_> = f
        .all_node_ids()
        .filter(|&n| matches!(f.node_kind(n), NodeKind::Store(_)))
        .collect();
    assert_eq!(stores.len(), 3, "three Stores in fixture");
    for &s in &stores {
        let out = f.memory_output_of(s).unwrap();
        let kind = f.output_kind(out);
        assert!(
            matches!(kind, NodeOutputKind::Memory(Some(_))),
            "Store mem-output should be partition-typed, got {kind:?}",
        );
    }

    // The Stack stores (a at sp+0, c at sp+4): both should have a
    // Stack-typed mem-output.
    let stack_stores: Vec<_> = stores
        .iter()
        .copied()
        .filter(|&s| {
            let out = f.memory_output_of(s).unwrap();
            f.output_kind(out).memory_partition() == Some(AliasClass::Stack)
        })
        .collect();
    assert_eq!(stack_stores.len(), 2, "a and c are Stack-classified");

    // Each Stack store's mem-input traces back via a Stack-typed
    // producer.  In particular: the second Stack store (c) MUST
    // trace to another Stack node (the first Stack store a or via
    // a Stack-typed MemPhi / MemPartition), NEVER through the
    // Unknown store b — the test that the Stack chain bypasses the
    // Unknown chain.
    for &stack_store in &stack_stores {
        let mem_in = f.node_inputs(stack_store).into_iter().next().unwrap();
        let prod_kind = f.output_kind(mem_in);
        assert_eq!(
            prod_kind.memory_partition(),
            Some(AliasClass::Stack),
            "Stack store's mem-input must be Stack-partition-typed; got {prod_kind:?}",
        );
    }

    // The Unknown store b's chain must NOT have a Stack-typed
    // predecessor — symmetry check.
    let unknown_stores: Vec<_> = stores
        .iter()
        .copied()
        .filter(|&s| {
            let out = f.memory_output_of(s).unwrap();
            f.output_kind(out).memory_partition() == Some(AliasClass::Unknown)
        })
        .collect();
    assert_eq!(unknown_stores.len(), 1, "b is the only Unknown store");
    let b = unknown_stores[0];
    let b_mem_in = f.node_inputs(b).into_iter().next().unwrap();
    let b_pred_kind = f.output_kind(b_mem_in);
    assert_eq!(
        b_pred_kind.memory_partition(),
        Some(AliasClass::Unknown),
        "Unknown store's mem-input must be Unknown-partition-typed (not Stack); got {b_pred_kind:?}",
    );

    let entry = f.entry().unwrap();
    strider_ir::validate::validate(&f, entry).expect("post-AliasSplit IR must validate");
}

// ─── Per-Call clobber: Stack flows through Call ──────────────────────────

#[test]
fn call_does_not_clobber_stack_chain() {
    // store sp-4; call f; load sp-4 — under the new design, Call
    // clobbers [Heap, Unknown] by default; the Stack chain flows
    // through.  The Load's mem-input should trace back to the Store
    // directly.
    let sp = sp_vn_x86();
    let mut f = stack_call_stack_return(sp);
    let r = run_split(&mut f, sp);
    assert_eq!(r, OptimizationResult::Changed);

    let load = unique_node(&f, |k| matches!(k, NodeKind::Load(_)));
    let mem_in = f.node_inputs(load).into_iter().next().unwrap();
    let producer = f.get_node_from_output(mem_in);
    let producer_kind = f.node_kind(producer);

    assert!(
        matches!(producer_kind, NodeKind::Store(_)),
        "Stack load's mem-input must be the Stack Store directly, not a Call-emitted \
         boundary; got {producer_kind:?}",
    );
    let prod_out = f.memory_output_of(producer).unwrap();
    assert_eq!(
        f.output_kind(prod_out).memory_partition(),
        Some(AliasClass::Stack),
    );

    let entry = f.entry().unwrap();
    strider_ir::validate::validate(&f, entry).expect("post-AliasSplit IR must validate");
}

// ─── CallOther with full clobber breaks Stack chain ───────────────────────

#[test]
fn callother_with_full_clobber_breaks_stack_chain() {
    // store sp-4; software_interrupt (FullClobber); load sp-4 —
    // software_interrupt's ABI is MEM_CLOBBER_FULL so the Stack
    // chain IS broken.  The Load's mem-input must trace through a
    // CallOther-emitted MemPartition[Stack], not directly back to
    // the Store.
    let sp = sp_vn_x86();
    let mut f = make_sp_fn(sp, |b, sp_v| {
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr = b.build_int_sub(sp_v, four, NodeOutputType::U32)?;
        let data = b.build_int_const(0x42u64, NodeOutputType::U32)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        b.build_call_other_modeled(0xCAFE, "software_interrupt", &[], None, &[], &[], &[])?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })
    .unwrap();

    let pass = AliasSplit::new(sp, ArchPreset::X86);
    let entry = f.entry().unwrap();
    let r = pass.optimize(&mut f, entry).expect("AliasSplit must not error");
    assert_eq!(r, OptimizationResult::Changed);

    let load = unique_node(&f, |k| matches!(k, NodeKind::Load(_)));
    let mem_in = f.node_inputs(load).into_iter().next().unwrap();
    let producer = f.get_node_from_output(mem_in);
    let producer_kind = f.node_kind(producer);
    assert!(
        matches!(
            producer_kind,
            NodeKind::MemPartition { class: AliasClass::Stack }
        ),
        "after a full-clobber CallOther, the Stack load must re-enter via a fresh \
         MemPartition[Stack]; got {producer_kind:?}",
    );

    strider_ir::validate::validate(&f, entry)
        .expect("post-AliasSplit IR with CallOther full clobber must validate");
}

// ─── Idempotency ──────────────────────────────────────────────────────────

#[test]
fn idempotent_on_already_partitioned_ir() {
    let sp = sp_vn_x86();
    let mut f = stack_store_load_return(sp);

    let r1 = run_split(&mut f, sp);
    assert_eq!(r1, OptimizationResult::Changed);

    let r2 = run_split(&mut f, sp);
    assert_eq!(r2, OptimizationResult::NoChange);
}

// ─── IndirectBranch is a terminal barrier ─────────────────────────────────

#[test]
fn indirect_branch_function_is_left_unified_v1() {
    // v1 scope: AliasSplit bails to NoChange on any function
    // containing an `IndirectBranch` placeholder.  The
    // indirect-branch resolver's stack-array classifier walks the
    // memory chain backward from the dispatching Load to find the
    // stored target values; the new forked chain shape's
    // interaction with that classifier is still under audit.
    // Leaving these functions unpartitioned preserves the prior
    // behaviour while the audit catches up.
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
    assert_eq!(
        r,
        OptimizationResult::NoChange,
        "AliasSplit must bail (NoChange) on functions with an IndirectBranch in v1",
    );
    assert_eq!(
        count_reachable(&f, |k| matches!(k, NodeKind::MemPartition { .. })),
        0,
        "no MemPartition nodes should be inserted when the pass bails",
    );

    let entry = f.entry().unwrap();
    strider_ir::validate::validate(&f, entry)
        .expect("IR with IndirectBranch must still validate");
}

// ─── stack_offsets side-table tests ──────────────────────────────────────

/// After `AliasSplit` runs on a function with two Stores at `sp-4` and
/// `sp-8`, the pattern DSL's `.stack_offset(k)` filter must match only
/// the Store at the requested offset and not the other.
#[test]
fn store_stack_offset_filter_matches_exact() {
    use crate::pattern::{Matcher, store};

    let sp = sp_vn_x86();
    let mut f = make_sp_fn(sp, |b, sp_v| {
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let eight = b.build_int_const(8u64, NodeOutputType::U32)?;
        let addr4 = b.build_int_sub(sp_v, four, NodeOutputType::U32)?;
        let addr8 = b.build_int_sub(sp_v, eight, NodeOutputType::U32)?;
        let data = b.build_int_const(0x42u64, NodeOutputType::U32)?;
        b.build_store(addr4, data, rsleigh::VnSpace::RAM)?;
        b.build_store(addr8, data, rsleigh::VnSpace::RAM)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        b.build_return(None, &[])?;
        Ok(())
    })
    .unwrap();

    run_split(&mut f, sp);

    let entry = f.entry().unwrap();
    // Matches only the sp-4 store.
    let pat = store().stack_offset(-4).into();
    let hits = Matcher::for_graph(&f, entry).find_all(&pat);
    assert_eq!(hits.len(), 1, "exactly one Store at sp-4 expected");

    // Matches only the sp-8 store.
    let pat8 = store().stack_offset(-8).into();
    let hits8 = Matcher::for_graph(&f, entry).find_all(&pat8);
    assert_eq!(hits8.len(), 1, "exactly one Store at sp-8 expected");

    // A non-existent offset matches nothing.
    let pat_none = store().stack_offset(-16).into();
    let hits_none = Matcher::for_graph(&f, entry).find_all(&pat_none);
    assert!(hits_none.is_empty(), "no Store at sp-16 — filter must reject");
}

/// After `AliasSplit`, `.stack_offset_any(ks)` on `load()` must match
/// exactly the Loads whose offsets are in `ks` and reject the rest.
#[test]
fn load_stack_offset_any_filter_matches_set() {
    use crate::pattern::{Matcher, load};

    let sp = sp_vn_x86();
    let mut f = make_sp_fn(sp, |b, sp_v| {
        let zero = b.build_int_const(0u64, NodeOutputType::U32)?;
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let eight = b.build_int_const(8u64, NodeOutputType::U32)?;
        // Three Stores so the subsequent Loads have memory to read.
        let addr0 = b.build_int_binary_operation(
            sp_v, zero, strider_ir::IntBinaryOp::Add, NodeOutputType::U32)?;
        let addr4 = b.build_int_sub(sp_v, four, NodeOutputType::U32)?;
        let addr8 = b.build_int_sub(sp_v, eight, NodeOutputType::U32)?;
        let data = b.build_int_const(0x11u64, NodeOutputType::U32)?;
        b.build_store(addr0, data, rsleigh::VnSpace::RAM)?;
        b.build_store(addr4, data, rsleigh::VnSpace::RAM)?;
        b.build_store(addr8, data, rsleigh::VnSpace::RAM)?;
        // Three Loads at sp+0, sp-4, sp-8.
        let v0 = b.build_load(addr0, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        let v4 = b.build_load(addr4, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        let _v8 = b.build_load(addr8, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        // Return the sum of the first two to keep all nodes reachable.
        let sum = b.build_int_binary_operation(
            v0, v4, strider_ir::IntBinaryOp::Add, NodeOutputType::U32)?;
        b.build_return(Some(sum), &[])?;
        Ok(())
    })
    .unwrap();

    run_split(&mut f, sp);

    let entry = f.entry().unwrap();
    // Match Loads at sp+0 and sp-4 but NOT sp-8.
    let pat = load().stack_offset_any(vec![0i64, -4i64]).into();
    let hits = Matcher::for_graph(&f, entry).find_all(&pat);
    assert_eq!(hits.len(), 2, "Loads at sp+0 and sp-4 must match, sp-8 must not");
}

// ─── Pipeline integration smoke test ──────────────────────────────────────

#[test]
fn pipeline_with_alias_split_validates() {
    use crate::opt::{ConstantFold, OptimizerPipeline};

    let sp = sp_vn_x86();
    let mut f = stack_store_load_return(sp);
    let entry = f.entry().unwrap();

    let mut p = OptimizerPipeline::new();
    p.add(ConstantFold);
    p.add(AliasSplit::new(sp, ArchPreset::X86));
    p.run(&mut f, entry).expect("pipeline must converge & validate");
}
