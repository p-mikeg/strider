//! White-box tests for [`crate::opt::AliasSplit`] (the forked
//! per-partition Memory-SSA rewrite).
//!
//! Each test builds a small synthetic function exhibiting a specific
//! memory-chain shape, runs `AliasSplit` once, and pins the resulting
//! IR structure (presence/absence of `MemProject` / `MemUnion`
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

/// Helper: collect the AliasClass of every output slot of every reachable
/// MemProject node.
fn mem_project_classes(function: &Function) -> Vec<AliasClass> {
    use strider_ir::node::NodeOutputKind;
    function
        .preorder_kind(|k| matches!(k, NodeKind::MemProject))
        .flat_map(|n| {
            function
                .node_outputs(n)
                .iter()
                .filter_map(|&out| match function.output_kind(out) {
                    NodeOutputKind::Memory(Some(class)) => Some(class),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

// ─── Entry-partition shape ────────────────────────────────────────────────

#[test]
fn entry_projects_both_active_partitions() {
    // Stack-only function: the pass should still project Stack AND
    // Unknown from InitialMemory at entry — over-projection is sound
    // and lets a later pass add a non-stack consumer without re-running
    // AliasSplit.
    let sp = sp_vn_x86();
    let mut f = stack_store_load_return(sp);

    let r = run_split(&mut f, sp);
    assert_eq!(r, OptimizationResult::Changed);

    use std::collections::HashSet;
    let classes: HashSet<AliasClass> = mem_project_classes(&f).into_iter().collect();
    assert!(classes.contains(&AliasClass::Stack), "Stack partition projected");
    assert!(classes.contains(&AliasClass::Unknown), "Unknown partition projected");

    let entry = f.entry().unwrap();
    strider_ir::validate::validate(&f, entry)
        .expect("post-AliasSplit IR must validate");
}

#[test]
fn empty_chain_with_return_is_skipped() {
    // No Stores/Loads at all — bare Return only.  With 0 memory ops the
    // partition split is pure overhead, so the pass must bail (NoChange)
    // and emit neither MemProject nor MemUnion.  The Return reads unified
    // Memory(None) directly.
    let sp = sp_vn_x86();
    let mut f = empty_chain_return(sp);
    let r = run_split(&mut f, sp);
    assert_eq!(r, OptimizationResult::NoChange);

    let n_part = count_reachable(&f, |k| matches!(k, NodeKind::MemProject));
    let n_union = count_reachable(&f, |k| matches!(k, NodeKind::MemUnion));
    assert_eq!(n_part, 0, "no MemProject when 0 memory ops");
    assert_eq!(n_union, 0, "no MemUnion when 0 memory ops");

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
    // a Stack-typed MemPhi / MemProject), NEVER through the
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

// ─── Per-Call clobber: Call breaks the Stack chain ───────────────────────

#[test]
fn call_clobbers_stack_chain() {
    // store sp-4; call f; load sp-4 — Call clobbers [Stack, Unknown]
    // by default (sound floor: callee may hold &local_var and mutate
    // the caller's stack frame).  The Stack Load's mem-input must trace
    // through a Call-emitted MemProject[Stack] re-projection,
    // NOT directly back to the Store.
    let sp = sp_vn_x86();
    let mut f = stack_call_stack_return(sp);
    let r = run_split(&mut f, sp);
    assert_eq!(r, OptimizationResult::Changed);

    use strider_ir::node::NodeOutputKind;
    let load = unique_node(&f, |k| matches!(k, NodeKind::Load(_)));
    let mem_in = f.node_inputs(load).into_iter().next().unwrap();
    let producer = f.get_node_from_output(mem_in);
    let producer_kind = f.node_kind(producer);

    assert!(
        matches!(producer_kind, NodeKind::MemProject),
        "Stack load's mem-input must re-enter via a fresh MemProject \
         after the Call (not a direct edge back to the Store); got {producer_kind:?}",
    );
    assert!(
        matches!(f.output_kind(mem_in), NodeOutputKind::Memory(Some(AliasClass::Stack))),
        "the MemProject output feeding the Stack load must be Stack-tagged; \
         got {:?}", f.output_kind(mem_in),
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
    // CallOther-emitted MemProject[Stack], not directly back to
    // the Store.
    let sp = sp_vn_x86();
    let mut f = make_sp_fn(sp, |b, sp_v| {
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr = b.build_int_sub(sp_v, four, NodeOutputType::U32)?;
        let data = b.build_int_const(0x42u64, NodeOutputType::U32)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let (call_other, _v, _w) = b.build_call_other_modeled(
            0xCAFE, "software_interrupt", &[], None, &[], &[], &[],
        )?;
        // Model the strider lifter's post-CallOther step: advance the
        // region's memory token to the CallOther's mem-output so the
        // subsequent Load reads through the clobber.  The lifter does
        // this automatically when the user-op's `mem_clobbers` is
        // non-empty; this fixture skips the strider layer and so
        // must do it manually.
        let co_mem_out = b.graph().memory_output_of(call_other)?;
        b.advance_cur_region_memory(co_mem_out)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })
    .unwrap();

    let pass = AliasSplit::new(sp, ArchPreset::X86);
    let entry = f.entry().unwrap();
    let r = pass.optimize(&mut f, entry).expect("AliasSplit must not error");
    assert_eq!(r, OptimizationResult::Changed);

    use strider_ir::node::NodeOutputKind;
    let load = unique_node(&f, |k| matches!(k, NodeKind::Load(_)));
    let mem_in = f.node_inputs(load).into_iter().next().unwrap();
    let producer = f.get_node_from_output(mem_in);
    let producer_kind = f.node_kind(producer);
    assert!(
        matches!(producer_kind, NodeKind::MemProject),
        "after a full-clobber CallOther, the Stack load must re-enter via a fresh \
         MemProject; got {producer_kind:?}",
    );
    assert!(
        matches!(f.output_kind(mem_in), NodeOutputKind::Memory(Some(AliasClass::Stack))),
        "the MemProject output feeding the Stack load must be Stack-tagged; \
         got {:?}", f.output_kind(mem_in),
    );

    strider_ir::validate::validate(&f, entry)
        .expect("post-AliasSplit IR with CallOther full clobber must validate");
}

// ─── LOCK breaks the Stack chain (regression for widened clobber set) ────────

#[test]
fn lock_callother_breaks_stack_chain() {
    // store sp-4; LOCK (now FullClobber); load sp-4 —
    // LOCK's ABI is now MEM_CLOBBER_FULL (widened from MEM_CLOBBER_HEAP_UNKNOWN),
    // so the Stack chain IS broken across it.  The Load's mem-input must trace
    // through a CallOther-emitted MemProject[Stack], not directly back to the Store.
    //
    // Prior to the soundness fix, LOCK was PureMem (HEAP_UNKNOWN only) and the
    // Stack Load would forward past LOCK — incorrect in a concurrent / aliased
    // scenario where another CPU could have modified the stack slot through the
    // locked instruction.
    let sp = sp_vn_x86();
    let mut f = make_sp_fn(sp, |b, sp_v| {
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr = b.build_int_sub(sp_v, four, NodeOutputType::U32)?;
        let data = b.build_int_const(0x42u64, NodeOutputType::U32)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let (call_other, _v, _w) = b.build_call_other_modeled(
            0x1234, "LOCK", &[], None, &[], &[], &[],
        )?;
        let co_mem_out = b.graph().memory_output_of(call_other)?;
        b.advance_cur_region_memory(co_mem_out)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })
    .unwrap();

    let pass = AliasSplit::new(sp, ArchPreset::X86);
    let entry = f.entry().unwrap();
    let r = pass.optimize(&mut f, entry).expect("AliasSplit must not error");
    assert_eq!(r, OptimizationResult::Changed);

    use strider_ir::node::NodeOutputKind;
    let load = unique_node(&f, |k| matches!(k, NodeKind::Load(_)));
    let mem_in = f.node_inputs(load).into_iter().next().unwrap();
    let producer = f.get_node_from_output(mem_in);
    let producer_kind = f.node_kind(producer);
    assert!(
        matches!(producer_kind, NodeKind::MemProject),
        "after LOCK (full-clobber), the Stack load must re-enter via a fresh \
         MemProject; got {producer_kind:?}",
    );
    assert!(
        matches!(f.output_kind(mem_in), NodeOutputKind::Memory(Some(AliasClass::Stack))),
        "the MemProject output feeding the Stack load must be Stack-tagged; \
         got {:?}", f.output_kind(mem_in),
    );

    strider_ir::validate::validate(&f, entry)
        .expect("post-AliasSplit IR with LOCK full-clobber must validate");
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
        count_reachable(&f, |k| matches!(k, NodeKind::MemProject)),
        0,
        "no MemProject nodes should be inserted when the pass bails",
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

// ─── Multi-pred MemPhi (true CFG memory joins) ──────────────────────────────

/// Diamond CFG with disjoint stack writes on each branch — the
/// canonical multi-pred MemPhi shape.  After `AliasSplit` runs, the
/// join MemPhi MUST get a per-partition mirror for the Stack class
/// whose two pred inputs are the Stack-chain heads from each branch.
#[test]
fn diamond_cfg_per_partition_memphi() {
    use strider_ir::IntBinaryOp;
    use strider_ir_test_utils::RegisterSet;

    let sp = sp_vn_x86();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .build_fn()
        .expect("builder");
    let entry = b.create_region().unwrap();
    let true_br = b.create_region().unwrap();
    let false_br = b.create_region().unwrap();
    let join = b.create_region().unwrap();
    b.set_entry_region(entry).unwrap();

    // entry: if (true) goto true_br else false_br
    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, true_br, false_br).unwrap();

    // true_br: *(sp+0) = 0x11; goto join
    b.set_region(true_br);
    let sp_t = b.read_variable(&sp).unwrap();
    let d0 = b.build_int_const(0x11u64, NodeOutputType::U32).unwrap();
    b.build_store(sp_t, d0, rsleigh::VnSpace::RAM).unwrap();
    b.build_branch(join).unwrap();

    // false_br: *(sp+8) = 0x22; goto join
    b.set_region(false_br);
    let sp_f = b.read_variable(&sp).unwrap();
    let eight = b.build_int_const(8u64, NodeOutputType::U32).unwrap();
    let addr_f = b
        .build_int_binary_operation(sp_f, eight, IntBinaryOp::Add, NodeOutputType::U32)
        .unwrap();
    let d1 = b.build_int_const(0x22u64, NodeOutputType::U32).unwrap();
    b.build_store(addr_f, d1, rsleigh::VnSpace::RAM).unwrap();
    b.build_branch(join).unwrap();

    // join: return *(sp+0)
    b.set_region(join);
    let sp_j = b.read_variable(&sp).unwrap();
    let loaded = b
        .build_load(sp_j, rsleigh::VnSpace::RAM, NodeOutputType::U32)
        .unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    b.build_return(Some(loaded), &[]).unwrap();
    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    let _ = &mut f; // silence

    let r = run_split(&mut f, sp);
    assert_eq!(r, OptimizationResult::Changed);

    let entry_id = f.entry().unwrap();
    strider_ir::validate::validate(&f, entry_id).expect("post-AliasSplit IR must validate");

    // The Load's mem-input MUST trace to a Stack-partition-typed
    // value.  Specifically: the per-partition Stack mirror MemPhi at
    // the join region.
    let load = unique_node(&f, |k| matches!(k, NodeKind::Load(_)));
    let mem_in = f.node_inputs(load).into_iter().next().unwrap();
    assert_eq!(
        f.output_kind(mem_in).memory_partition(),
        Some(AliasClass::Stack),
        "Load's mem-input must be Stack-partition-typed at the join"
    );
    let producer = f.get_node_from_output(mem_in);
    assert!(
        matches!(f.node_kind(producer), NodeKind::MemPhi),
        "Load's mem-input must come from a per-partition Stack MemPhi at the join, got {:?}",
        f.node_kind(producer)
    );

    // Both Stores must be Stack-partition-typed.
    let stores: Vec<_> = f
        .all_node_ids()
        .filter(|&n| matches!(f.node_kind(n), NodeKind::Store(_)))
        .collect();
    assert_eq!(stores.len(), 2, "two Stores in fixture");
    for &s in &stores {
        let out = f.memory_output_of(s).unwrap();
        assert_eq!(
            f.output_kind(out).memory_partition(),
            Some(AliasClass::Stack),
            "Store at sp+K must be partition-typed Stack",
        );
    }
}

/// Loop-header MemPhi with a real back-edge from the body.  The
/// per-partition mirror MemPhi at the loop header MUST have its
/// back-edge input wired by pass-2 deferred sweep — both arms
/// (outer entry's chain, loop-body's chain) must be present.
#[test]
fn loop_back_edge_partition_memphi_closes_correctly() {
    use strider_ir_test_utils::RegisterSet;

    let sp = sp_vn_x86();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .build_fn()
        .unwrap();
    let entry = b.create_region().unwrap();
    let header = b.create_region().unwrap();
    let exit = b.create_region().unwrap();
    b.set_entry_region(entry).unwrap();

    // entry: goto header
    b.set_region(entry);
    b.build_branch(header).unwrap();

    // header: store *sp = 0x11; if (true) goto header else goto exit
    // (synthetic back-edge from header to itself: writes to sp+0 from
    // both the outer path and the back-edge path.)
    b.set_region(header);
    let sp_h = b.read_variable(&sp).unwrap();
    let v = b.build_int_const(0x11u64, NodeOutputType::U32).unwrap();
    b.build_store(sp_h, v, rsleigh::VnSpace::RAM).unwrap();
    let cond = b.build_boolean_const(true);
    b.build_if(cond, header, exit).unwrap();

    // exit: return *sp
    b.set_region(exit);
    let sp_e = b.read_variable(&sp).unwrap();
    let loaded = b
        .build_load(sp_e, rsleigh::VnSpace::RAM, NodeOutputType::U32)
        .unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    b.build_return(Some(loaded), &[]).unwrap();
    b.set_lift_addr(None);
    let mut f = b.build().unwrap();

    let r = run_split(&mut f, sp);
    assert_eq!(r, OptimizationResult::Changed);

    let entry_id = f.entry().unwrap();
    strider_ir::validate::validate(&f, entry_id).expect("post-AliasSplit IR must validate");

    // The loop-header MemPhi has 2 mem preds (outer entry + back-edge).
    // After AliasSplit, the per-partition Stack mirror MemPhi at the
    // header must also have 2 mem preds — and both must be
    // Memory(Some(Stack)).
    let mem_phis: Vec<_> = f
        .all_node_ids()
        .filter(|&n| matches!(f.node_kind(n), NodeKind::MemPhi))
        .collect();
    let stack_mirrors: Vec<_> = mem_phis
        .iter()
        .copied()
        .filter(|&n| {
            let out = f.memory_output_of(n).ok();
            out.is_some_and(|o| f.output_kind(o).memory_partition() == Some(AliasClass::Stack))
        })
        .collect();
    assert!(!stack_mirrors.is_empty(), "must have ≥1 Stack-partition MemPhi mirror");
    // The loop-header Stack mirror has 2 mem preds (i.e. 1 + 2 = 3 inputs).
    let has_multi_pred_stack_mirror = stack_mirrors.iter().any(|&n| {
        let inputs: Vec<_> = f.node_inputs(n).into_iter().collect();
        inputs.len() >= 3 // phi_token + ≥2 mem preds
    });
    assert!(
        has_multi_pred_stack_mirror,
        "loop-header Stack mirror MemPhi must have ≥2 mem preds (the back-edge was closed)"
    );

    // Every input of every Stack mirror must be Memory(Some(Stack)) or PhiToken.
    for &n in &stack_mirrors {
        let inputs: Vec<_> = f.node_inputs(n).into_iter().collect();
        for (i, &v) in inputs.iter().enumerate() {
            let kind = f.output_kind(v);
            if i == 0 {
                assert!(matches!(kind, NodeOutputKind::PhiToken),
                        "MemPhi mirror input[0] must be PhiToken, got {kind:?}");
            } else {
                assert_eq!(
                    kind.memory_partition(),
                    Some(AliasClass::Stack),
                    "Stack mirror MemPhi input[{i}] must be Memory(Some(Stack)), got {kind:?}",
                );
            }
        }
    }
}

/// Diamond CFG where only ONE branch writes to Stack; the other branch
/// has no Stack writes.  The Stack mirror MemPhi at the join must
/// still have a valid Memory(Some(Stack)) input for the inactive
/// branch — sourced from the function-entry MemProject[Stack] as
/// the canonical default.
#[test]
fn partition_inactive_on_branch_uses_entry_default() {
    use strider_ir_test_utils::RegisterSet;

    let sp = sp_vn_x86();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .build_fn()
        .unwrap();
    let entry = b.create_region().unwrap();
    let true_br = b.create_region().unwrap();
    let false_br = b.create_region().unwrap();
    let join = b.create_region().unwrap();
    b.set_entry_region(entry).unwrap();

    // entry: if (true) goto true_br else false_br
    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, true_br, false_br).unwrap();

    // true_br: *sp = 0x11; goto join   (writes Stack)
    b.set_region(true_br);
    let sp_t = b.read_variable(&sp).unwrap();
    let d = b.build_int_const(0x11u64, NodeOutputType::U32).unwrap();
    b.build_store(sp_t, d, rsleigh::VnSpace::RAM).unwrap();
    b.build_branch(join).unwrap();

    // false_br: empty branch — no Stack writes.
    b.set_region(false_br);
    b.build_branch(join).unwrap();

    // join: return *sp
    b.set_region(join);
    let sp_j = b.read_variable(&sp).unwrap();
    let loaded = b
        .build_load(sp_j, rsleigh::VnSpace::RAM, NodeOutputType::U32)
        .unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    b.build_return(Some(loaded), &[]).unwrap();
    b.set_lift_addr(None);
    let mut f = b.build().unwrap();

    let r = run_split(&mut f, sp);
    assert_eq!(r, OptimizationResult::Changed);

    let entry_id = f.entry().unwrap();
    strider_ir::validate::validate(&f, entry_id).expect("post-AliasSplit IR must validate");

    // The Stack mirror MemPhi at the join must have:
    //   inputs[0] = PhiToken
    //   inputs[1] = the Store's mem-output (true-branch's Stack tail)
    //   inputs[2] = MemProject[Stack] from entry  (false-branch's
    //               Stack head is the unmodified entry projection)
    // Both value inputs MUST be Memory(Some(Stack)) — the entry
    // MemProject[Stack] is the canonical default.
    let mem_phis: Vec<_> = f
        .all_node_ids()
        .filter(|&n| matches!(f.node_kind(n), NodeKind::MemPhi))
        .collect();
    let stack_mirrors: Vec<_> = mem_phis
        .iter()
        .copied()
        .filter(|&n| {
            let out = f.memory_output_of(n).ok();
            out.is_some_and(|o| f.output_kind(o).memory_partition() == Some(AliasClass::Stack))
        })
        .collect();
    // The join's Stack mirror has 1 + 2 = 3 inputs.
    let join_stack_mirror = stack_mirrors
        .iter()
        .copied()
        .find(|&n| f.node_inputs(n).into_iter().count() == 3)
        .expect("join Stack mirror MemPhi");
    // Every pred slot must be Memory(Some(Stack)).
    for (i, v) in f.node_inputs(join_stack_mirror).into_iter().enumerate() {
        let kind = f.output_kind(v);
        if i == 0 {
            assert!(matches!(kind, NodeOutputKind::PhiToken));
        } else {
            assert_eq!(
                kind.memory_partition(),
                Some(AliasClass::Stack),
                "join Stack mirror pred[{i}] must be Memory(Some(Stack)) (entry default for \
                 inactive branch), got {kind:?}",
            );
        }
    }
}

/// Sanity: a function with a multi-pred MemPhi (diamond CFG) with ≥2
/// reachable memory ops partitions successfully — `OptimizationResult::Changed`
/// and at least one `MemProject[Stack]` node appears.  The Load is wired to
/// the Return so it is reachable and counted toward the 2-op threshold.
#[test]
fn previously_bailed_functions_now_partition() {
    use strider_ir_test_utils::RegisterSet;

    let sp = sp_vn_x86();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .build_fn()
        .unwrap();
    let entry = b.create_region().unwrap();
    let true_br = b.create_region().unwrap();
    let false_br = b.create_region().unwrap();
    let join = b.create_region().unwrap();
    b.set_entry_region(entry).unwrap();
    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, true_br, false_br).unwrap();
    b.set_region(true_br);
    let sp_t = b.read_variable(&sp).unwrap();
    let d = b.build_int_const(1u64, NodeOutputType::U32).unwrap();
    b.build_store(sp_t, d, rsleigh::VnSpace::RAM).unwrap();
    b.build_branch(join).unwrap();
    b.set_region(false_br);
    b.build_branch(join).unwrap();
    b.set_region(join);
    let sp_j = b.read_variable(&sp).unwrap();
    // Wire the Load to the Return so it's reachable (addr_class.len() == 2
    // → 1 Store + 1 Load → partition split fires).
    let loaded = b
        .build_load(sp_j, rsleigh::VnSpace::RAM, NodeOutputType::U32)
        .unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    b.build_return(Some(loaded), &[]).unwrap();
    b.set_lift_addr(None);
    let mut f = b.build().unwrap();

    let r = run_split(&mut f, sp);
    assert_eq!(
        r,
        OptimizationResult::Changed,
        "AliasSplit must succeed on functions with a multi-pred MemPhi"
    );
    let n_part = count_reachable(&f, |k| matches!(k, NodeKind::MemProject));
    assert!(n_part >= 1, "≥1 MemProject node must be emitted");
}

// ─── ≤1-op skip: no MemProject/MemUnion for 0 or 1 memory ops ─────────────

/// 0 memory ops: a function with no Stores or Loads must emit no
/// MemProject and no MemUnion regardless of barriers.  The Return reads
/// unified Memory(None) directly.
#[test]
fn zero_memory_ops_emits_no_project_or_union() {
    let sp = sp_vn_x86();
    let mut f = empty_chain_return(sp);
    let r = run_split(&mut f, sp);
    assert_eq!(r, OptimizationResult::NoChange, "0-op function must not be partitioned");
    assert_eq!(
        count_reachable(&f, |k| matches!(k, NodeKind::MemProject)),
        0,
        "0-op: no MemProject expected",
    );
    assert_eq!(
        count_reachable(&f, |k| matches!(k, NodeKind::MemUnion)),
        0,
        "0-op: no MemUnion expected",
    );
    let entry = f.entry().unwrap();
    strider_ir::validate::validate(&f, entry).expect("must validate");
}

/// 1 memory op: a function with exactly one Store (no Load) must emit no
/// MemProject and no MemUnion.  The single Store sees unified Memory(None)
/// directly — one op can't alias with anything else.
#[test]
fn one_memory_op_emits_no_project_or_union() {
    let sp = sp_vn_x86();
    let mut f = make_sp_fn(sp, |b, sp_v| {
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr = b.build_int_sub(sp_v, four, NodeOutputType::U32)?;
        let data = b.build_int_const(0x42u64, NodeOutputType::U32)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        b.build_return(None, &[])?;
        Ok(())
    })
    .unwrap();

    let r = run_split(&mut f, sp);
    assert_eq!(r, OptimizationResult::NoChange, "1-op function must not be partitioned");
    assert_eq!(
        count_reachable(&f, |k| matches!(k, NodeKind::MemProject)),
        0,
        "1-op: no MemProject expected",
    );
    assert_eq!(
        count_reachable(&f, |k| matches!(k, NodeKind::MemUnion)),
        0,
        "1-op: no MemUnion expected",
    );

    // The single Store's mem-output must still be unified Memory(None).
    let store = unique_node(&f, |k| matches!(k, NodeKind::Store(_)));
    let mem_out = f.memory_output_of(store).unwrap();
    assert!(
        matches!(f.output_kind(mem_out), NodeOutputKind::Memory(None)),
        "1-op Store's mem-output must remain unified Memory(None); got {:?}",
        f.output_kind(mem_out),
    );

    let entry = f.entry().unwrap();
    strider_ir::validate::validate(&f, entry).expect("must validate");
}

/// 2 memory ops: a function with one Store + one Load (2 total) MUST emit
/// MemProject and MemUnion — this is the ≥2 baseline that verifies the
/// skip is conditional, not unconditional.
#[test]
fn two_memory_ops_does_emit_project_and_union() {
    let sp = sp_vn_x86();
    // stack_store_load_return has 1 Store + 1 Load = 2 addr_class entries.
    let mut f = stack_store_load_return(sp);
    let r = run_split(&mut f, sp);
    assert_eq!(r, OptimizationResult::Changed, "2-op function must be partitioned");
    assert!(
        count_reachable(&f, |k| matches!(k, NodeKind::MemProject)) >= 1,
        "2-op: ≥1 MemProject expected",
    );
    assert!(
        count_reachable(&f, |k| matches!(k, NodeKind::MemUnion)) >= 1,
        "2-op: ≥1 MemUnion expected",
    );
    let entry = f.entry().unwrap();
    strider_ir::validate::validate(&f, entry).expect("must validate");
}
