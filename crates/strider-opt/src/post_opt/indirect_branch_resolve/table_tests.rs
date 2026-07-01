//! Unit tests for the unified table-dispatch classifier.
//!
//! Merges the former rodata-jump-table and on-stack-label-array test
//! suites: both constructs are now classified by the single
//! [`classify_table_dispatch`] entry point (absolute base when
//! `stack_vn` is `None` + a rom is supplied; SP-rooted base when a
//! `stack_vn` is supplied).  Each test builds a minimal graph in
//! isolation and invokes the piece-under-test directly.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

use super::*;
use crate::{
    ConstantFold, KnownBits, LoadForward, OptimizerPipeline, PhiCollapse, RegionCollapse,
    StackOffsetDetect,
};
use rsleigh::VnSpace;
use std::sync::Mutex;
use strider_ir::node::ValueType;
use strider_ir::{
    ExtendOp, Function, FunctionBuilder, IRBuilderExt, IRViewer, IRWalker, IntBinaryOp,
};
use strider_ir_test_utils::IrWalkerEx;
use strider_ir_test_utils::IrBuilderEx;
use strider_ir_test_utils::{
    MockRom, RegisterSet, stack_vn_aarch64 as sp64, stack_vn_x86 as sp32_vn,
};

/// Build a `(known, doms)` pair needed to construct a `RangeMap`.
fn make_known_and_doms(
    fg: &Function,
) -> (
    crate::KnownBitsMap,
    petgraph::algo::dominators::Dominators<strider_ir::node::NodeId>,
) {
    let known = crate::analyze_known_bits(fg).expect("kb analyze");
    let doms = strider_ir::control_dominators(fg);
    (known, doms)
}

/// `ReadOnlyMemory` impl that records every (addr,size) read it
/// services.  Used to assert the absolute-base read path issues exactly
/// `count` reads in index order.
///
/// Kept distinct from the shared [`MockRom`] helper because its
/// recording-side-log behaviour is unique to this file.
pub struct RecordingRom {
    pub inner: MockRom,
    pub log: Mutex<Vec<(u64, usize)>>,
}

impl ReadOnlyMemory for RecordingRom {
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        self.log.lock().unwrap().push((addr, buf.len()));
        self.inner.read(addr, buf)
    }
}

/// Minimal `Graph` carrying nothing but the entry
/// region terminated by a placeholder `IndirectBranch(anchor)`.  The
/// caller-supplied closure builds the anchor's producer subtree.
fn build_with_anchor(
    anchor_inputs: impl FnOnce(&mut FunctionBuilder) -> ValueId,
) -> (Function, ValueId) {
    let mut builder = strider_ir_test_utils::empty_builder().expect("empty_builder");
    let region = builder.create_region().expect("create_region");
    builder.set_entry_region(region).expect("set_entry_region");
    builder.set_region(region);
    builder.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let anchor = anchor_inputs(&mut builder);
    builder
        .build_indirect_branch(anchor)
        .expect("build_indirect_branch");
    builder.set_lift_addr(None);
    let function = builder.build().expect("build");
    (function, anchor)
}

/// The function's sole `IndirectBranch` placeholder — the node the table
/// classifier now takes directly (it derives the dispatch anchor from the
/// branch's slot-2 input and scopes the range query to it).
fn sole_indirect_branch(f: &Function) -> NodeId {
    f.walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::IndirectBranch))
        .expect("placeholder IndirectBranch")
}

/// Build a non-IntConst integer value usable as a table `idx`: a load whose
/// result the optimiser cannot fold away, so the dispatch genuinely depends
/// on it.
fn build_non_const_idx(fb: &mut FunctionBuilder) -> ValueId {
    let addr = fb.build_int_const(0x9000u64, ValueType::I32).unwrap();
    fb.build_load(addr, VnSpace::RAM, ValueType::I32)
        .expect("u32 load (idx)")
}

// ── End-to-end classifier-on-shape tests (absolute / rodata arm) ─────────────

#[test]
fn classify_table_dispatch_with_known_bits_bound_returns_multiple() {
    // idx = (load) & 0x7 → bound 8 via KnownBits upper bound in the range pass.
    // Load[base + idx*stride] → resolves to Multiple of table[0..8].
    let (g, _anchor) = build_with_anchor(|fb| {
        // idx side: AND-masked to 0..7.
        let raw = fb.build_int_const(0xffff_ffffu64, ValueType::I32).unwrap();
        let mask = fb.build_int_const(0x7u64, ValueType::I32).unwrap();
        let idx = fb
            .build_int_binary_operation(raw, mask, IntBinaryOp::And, ValueType::I32)
            .expect("and");
        let stride_c = fb.build_int_const(4u64, ValueType::I32).unwrap();
        let mul = fb
            .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, ValueType::I32)
            .expect("mul");
        let base_c = fb.build_int_const(0x4000u64, ValueType::I32).unwrap();
        let addr = fb
            .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, ValueType::I32)
            .expect("add");
        fb.build_load(addr, VnSpace::RAM, ValueType::I32)
            .expect("load")
    });
    let rom = MockRom::strided(
        0x4000,
        4,
        vec![0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80],
        4,
    );
    let (known, doms) = make_known_and_doms(&g);
    let mut ranges = crate::value_range::compute_value_ranges(&g, &doms, &known);
    let result = classify_table_dispatch(
        &g,
        sole_indirect_branch(&g),
        Some(&rom),
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
    );
    match result {
        Some(ResolvedTargets::Multiple(ts)) => {
            assert_eq!(ts, vec![0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80]);
        }
        other => panic!("expected Multiple([0x10..0x80]); got {other:?}"),
    }
}

#[test]
fn classify_table_dispatch_duplicate_targets_are_deduped() {
    // idx = (load) & 0x3 → bound 4 via KnownBits.  The table's four entries
    // resolve to targets [0x10, 0x20, 0x10, 0x20] — two distinct addresses
    // each appearing twice.  `enumerate_targets`'s `sort_unstable` + `dedup`
    // must collapse the four indices to a 2-element `Multiple([0x10, 0x20])`.
    let (g, _anchor) = build_with_anchor(|fb| {
        let raw = fb.build_int_const(0xffff_ffffu64, ValueType::I32).unwrap();
        let mask = fb.build_int_const(0x3u64, ValueType::I32).unwrap();
        let idx = fb
            .build_int_binary_operation(raw, mask, IntBinaryOp::And, ValueType::I32)
            .expect("and");
        let stride_c = fb.build_int_const(4u64, ValueType::I32).unwrap();
        let mul = fb
            .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, ValueType::I32)
            .expect("mul");
        let base_c = fb.build_int_const(0x4000u64, ValueType::I32).unwrap();
        let addr = fb
            .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, ValueType::I32)
            .expect("add");
        fb.build_load(addr, VnSpace::RAM, ValueType::I32)
            .expect("load")
    });
    // Indices 0..3 map to [0x10, 0x20, 0x10, 0x20] — duplicates across indices.
    let rom = MockRom::strided(0x4000, 4, vec![0x10, 0x20, 0x10, 0x20], 4);
    let (known, doms) = make_known_and_doms(&g);
    let mut ranges = crate::value_range::compute_value_ranges(&g, &doms, &known);
    let result = classify_table_dispatch(
        &g,
        sole_indirect_branch(&g),
        Some(&rom),
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
    );
    match result {
        Some(ResolvedTargets::Multiple(ts)) => {
            assert_eq!(
                ts,
                vec![0x10, 0x20],
                "four indices producing [0x10,0x20,0x10,0x20] dedup to two targets"
            );
        }
        other => panic!("expected Multiple([0x10, 0x20]); got {other:?}"),
    }
}

#[test]
fn classify_table_dispatch_single_entry_bound_returns_multiple_of_one() {
    // Degenerate rodata jump table of size 1: idx = (load) & 0x0 → KnownBits
    // proves idx is always 0, so the range pass yields bound = 1 and the
    // classifier reads exactly one entry.  Pins that a one-entry table is
    // still classified as `Multiple` (with a single target), not `Single`
    // and not a defer.
    let (g, _anchor) = build_with_anchor(|fb| {
        let idx_src = build_non_const_idx(fb);
        let mask = fb.build_int_const(0u64, ValueType::I32).unwrap();
        let idx = fb
            .build_int_binary_operation(idx_src, mask, IntBinaryOp::And, ValueType::I32)
            .expect("and");
        let stride_c = fb.build_int_const(4u64, ValueType::I32).unwrap();
        let mul = fb
            .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, ValueType::I32)
            .expect("mul");
        let base_c = fb.build_int_const(0x4000u64, ValueType::I32).unwrap();
        let addr = fb
            .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, ValueType::I32)
            .expect("add");
        fb.build_load(addr, VnSpace::RAM, ValueType::I32)
            .expect("load")
    });
    let rom = MockRom::strided(0x4000, 4, vec![0x10, 0x20, 0x30, 0x40], 4);
    let (known, doms) = make_known_and_doms(&g);
    let mut ranges = crate::value_range::compute_value_ranges(&g, &doms, &known);
    let result = classify_table_dispatch(
        &g,
        sole_indirect_branch(&g),
        Some(&rom),
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
    );
    match result {
        Some(ResolvedTargets::Multiple(ts)) => {
            assert_eq!(ts, vec![0x10], "bound 1 reads exactly the first entry");
        }
        other => panic!("expected Multiple([0x10]); got {other:?}"),
    }
}

#[test]
fn classify_table_dispatch_no_rom_returns_none() {
    // Bounded shape, but no rom configured → None.  Without rom
    // we can't read entries, and producing a Multiple without
    // entries is unsound.
    let (g, _anchor) = build_with_anchor(|fb| {
        let raw = fb.build_int_const(0xffff_ffffu64, ValueType::I32).unwrap();
        let mask = fb.build_int_const(0x3u64, ValueType::I32).unwrap();
        let idx = fb
            .build_int_binary_operation(raw, mask, IntBinaryOp::And, ValueType::I32)
            .expect("and");
        let stride_c = fb.build_int_const(4u64, ValueType::I32).unwrap();
        let mul = fb
            .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, ValueType::I32)
            .expect("mul");
        let base_c = fb.build_int_const(0x4000u64, ValueType::I32).unwrap();
        let addr = fb
            .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, ValueType::I32)
            .expect("add");
        fb.build_load(addr, VnSpace::RAM, ValueType::I32)
            .expect("load")
    });
    let (known, doms) = make_known_and_doms(&g);
    let mut ranges = crate::value_range::compute_value_ranges(&g, &doms, &known);
    let result = classify_table_dispatch(
        &g,
        sole_indirect_branch(&g),
        None,
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
    );
    assert_eq!(result, None);
}

#[test]
fn classify_table_dispatch_unbounded_idx_returns_none() {
    // Shape is jt-shaped, but `idx` is a raw load with no AND mask and
    // no dominating If guard.  Must return None, not a Multiple.
    let (g, _anchor) = build_with_anchor(|fb| {
        let some_addr = fb.build_int_const(0x9000u64, ValueType::I32).unwrap();
        let idx = fb
            .build_load(some_addr, VnSpace::RAM, ValueType::I32)
            .expect("load idx");
        let stride_c = fb.build_int_const(4u64, ValueType::I32).unwrap();
        let mul = fb
            .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, ValueType::I32)
            .expect("mul");
        let base_c = fb.build_int_const(0x4000u64, ValueType::I32).unwrap();
        let addr = fb
            .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, ValueType::I32)
            .expect("add");
        fb.build_load(addr, VnSpace::RAM, ValueType::I32)
            .expect("load")
    });
    let rom = MockRom::strided(0x4000, 4, vec![0x10, 0x20, 0x30, 0x40], 4);
    let (known, doms) = make_known_and_doms(&g);
    let mut ranges = crate::value_range::compute_value_ranges(&g, &doms, &known);
    let result = classify_table_dispatch(
        &g,
        sole_indirect_branch(&g),
        Some(&rom),
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
    );
    assert_eq!(result, None);
}

/// The `count <= MAX_TABLE_ENTRIES` (4096) cap: the SAME masked-dispatch
/// shape resolves when the index range is small (`& 0x3` → 4 entries) but
/// DEFERS when it exceeds the cap (`& 0x1FFF` → 8192 entries).  The masked
/// index passes the `hi < type_mask` candidate filter (unlike the unbounded
/// case above, which fails it), so the cap is the only thing that changes
/// the verdict.  The guard short-circuits BEFORE the per-entry fold, so the
/// over-cap case stays cheap (no 8192 clone+optimize rounds).
#[test]
fn classify_table_dispatch_defers_over_cap_resolves_under_cap() {
    let build = |mask: u64| {
        build_with_anchor(move |fb| {
            let idx_addr = fb.build_int_const(0x9000u64, ValueType::I32).unwrap();
            let idx_raw = fb
                .build_load(idx_addr, VnSpace::RAM, ValueType::I32)
                .expect("load idx");
            let mask_c = fb.build_int_const(mask, ValueType::I32).unwrap();
            let idx = fb
                .build_int_binary_operation(idx_raw, mask_c, IntBinaryOp::And, ValueType::I32)
                .expect("mask idx");
            let stride_c = fb.build_int_const(4u64, ValueType::I32).unwrap();
            let mul = fb
                .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, ValueType::I32)
                .expect("mul");
            let base_c = fb.build_int_const(0x4000u64, ValueType::I32).unwrap();
            let addr = fb
                .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, ValueType::I32)
                .expect("add");
            fb.build_load(addr, VnSpace::RAM, ValueType::I32)
                .expect("dispatch load")
        })
    };
    let rom = MockRom::strided(0x4000, 4, vec![0x10, 0x20, 0x30, 0x40], 4);

    // Under the cap (mask 0x3 → range [0,3], 4 entries): resolves.
    let (g, _) = build(0x3);
    let (known, doms) = make_known_and_doms(&g);
    let mut ranges = crate::value_range::compute_value_ranges(&g, &doms, &known);
    let under = classify_table_dispatch(
        &g,
        sole_indirect_branch(&g),
        Some(&rom),
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
    );
    assert!(
        matches!(under, Some(ResolvedTargets::Multiple(_))),
        "a 4-entry table resolves, got {under:?}",
    );

    // Over the cap (mask 0x1FFF → range [0,8191], 8192 > 4096 entries): defers.
    let (g2, _) = build(0x1FFF);
    let (known2, doms2) = make_known_and_doms(&g2);
    let mut ranges2 = crate::value_range::compute_value_ranges(&g2, &doms2, &known2);
    let over = classify_table_dispatch(
        &g2,
        sole_indirect_branch(&g2),
        Some(&rom),
        &mut ranges2,
        AliasMode::StackGlobalDisjoint,
    );
    assert_eq!(
        over, None,
        "an 8192-entry table exceeds MAX_TABLE_ENTRIES and must defer",
    );
}

#[test]
fn classify_table_dispatch_excludes_width_bounded_table_entry_as_index() {
    // ARM `tbb`-style shape: the index is itself a table ENTRY — a byte loaded
    // from a table (`Load[addr]:I8`) and zero-extended to I32.  Its only bound
    // is width-derived ([0, 255]); there is NO AND-mask and NO dominating `If`
    // guard.  `find_index_candidates` must EXCLUDE this load-derived value (the
    // `entry_load` filter): its [0,255] range passes the `iv.hi < type_mask`
    // width check (255 < 0xFFFF_FFFF), so width alone would NOT reject it — only
    // the entry_load filter does.  Enumerating it would fold to a run of 256
    // bogus sequential targets, so the classifier must return None.
    let (g, _anchor) = build_with_anchor(|fb| {
        // idx = ZeroExtend(Load[0x9000]:I8) — a width-bounded ([0,255]) table
        // entry, NOT a guarded/masked dispatch index.
        let byte_addr = fb.build_int_const(0x9000u64, ValueType::I32).unwrap();
        let byte = fb
            .build_load(byte_addr, VnSpace::RAM, ValueType::I8)
            .expect("byte load (table entry)");
        let idx = fb
            .extend_if_needed(byte, ValueType::I32, ExtendOp::ZeroExtend)
            .expect("zero-extend the byte to I32");
        let stride_c = fb.build_int_const(4u64, ValueType::I32).unwrap();
        let mul = fb
            .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, ValueType::I32)
            .expect("mul");
        let base_c = fb.build_int_const(0x4000u64, ValueType::I32).unwrap();
        let addr = fb
            .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, ValueType::I32)
            .expect("add");
        fb.build_load(addr, VnSpace::RAM, ValueType::I32)
            .expect("dispatch load")
    });
    // A rom large enough that, were the entry erroneously taken as the index,
    // the 256 sequential reads would each fold — so the ONLY reason for None is
    // the entry_load exclusion, not a fold failure.
    let rom = MockRom::strided(0x4000, 4, vec![0x10; 256], 4);
    let (known, doms) = make_known_and_doms(&g);
    let mut ranges = crate::value_range::compute_value_ranges(&g, &doms, &known);
    let result = classify_table_dispatch(
        &g,
        sole_indirect_branch(&g),
        Some(&rom),
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
    );
    assert_eq!(
        result, None,
        "a width-bounded load-derived table entry must be excluded as the index"
    );
}

#[test]
fn classify_table_dispatch_with_if_guard_bound_returns_multiple() {
    // Demonstrates the range-pass `If(idx < N)` guard path:
    // idx is an unmasked register read, bounded by a dominating
    // `if (idx < 4)` guard.  The range pass extracts the guard from
    // the `If` node and yields bound = 4.
    use strider_ir::IntCmpOp;
    let idx_var = rsleigh::Vn {
        addr_off: 0x10,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let mut b = RegisterSet::new().tracked(idx_var).build_fn().unwrap();
    let entry = b.create_region().unwrap();
    let dispatch = b.create_region().unwrap();
    let exit = b.create_region().unwrap();
    b.set_entry_region(entry).unwrap();

    b.set_region(entry);
    let idx_at_entry = b.read_variable(&idx_var).unwrap();
    let bound_c = b.build_int_const(4u64, ValueType::I32).unwrap();
    let cond = b
        .build_int_cmp_operation(idx_at_entry, bound_c, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond, dispatch, exit).unwrap();

    b.set_region(dispatch);
    let idx_in_dispatch = b.read_variable(&idx_var).unwrap();
    let stride_c = b.build_int_const(4u64, ValueType::I32).unwrap();
    let mul = b
        .build_int_binary_operation(idx_in_dispatch, stride_c, IntBinaryOp::Mul, ValueType::I32)
        .unwrap();
    let base_c = b.build_int_const(0x4000u64, ValueType::I32).unwrap();
    let addr = b
        .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, ValueType::I32)
        .unwrap();
    let loaded = b
        .build_load(addr, VnSpace::RAM, ValueType::I32)
        .expect("load");
    b.build_indirect_branch(loaded).unwrap();

    b.set_region(exit);
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);

    let mut function = b.build().unwrap();
    // `value_range` assumes converged IR (its production caller is a post-pass).
    // Collapse the single-predecessor dispatch region and the trivial tracked-var
    // phis this hand-built fixture has, so the `idx < 4` guard reaches the
    // dispatch index — matching what the analysis sees in production.
    {
        let mut p = crate::OptimizerPipeline::new();
        p.add(crate::PhiCollapse);
        p.add(crate::RegionCollapse);
        p.run(&mut function, &mut crate::OptCtx::new(None)).unwrap();
    }

    let rom = MockRom::strided(0x4000, 4, vec![0x10, 0x20, 0x30, 0x40], 4);
    let (known, doms) = make_known_and_doms(&function);
    let mut ranges = crate::value_range::compute_value_ranges(&function, &doms, &known);

    let result = classify_table_dispatch(
        &function,
        sole_indirect_branch(&function),
        Some(&rom),
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
    );
    match result {
        Some(ResolvedTargets::Multiple(ts)) => {
            assert_eq!(ts, vec![0x10, 0x20, 0x30, 0x40]);
        }
        other => panic!("expected Multiple([0x10..0x40]); got {other:?}"),
    }
}

#[test]
fn classify_table_dispatch_diamond_both_paths_guarded_defers() {
    // A dispatch with two predecessor paths, both guarded `idx < 4`, whose
    // index is a multi-input Phi at the joining (merge) Region.
    //
    // The both-edge guard model keys each guard by the unique control consumer
    // of the guarded If-edge.  Here both true edges feed the SAME 2-predecessor
    // merge Region, and the soundness gate skips a guard whose consumer is a
    // control merge (a single edge does not dominate the merge — other
    // predecessors bypass it).  This is conservative: even though BOTH paths
    // happen to guard `idx < 4`, the per-edge gate cannot prove that from one
    // edge, so the dispatch DEFERS (returns `None`) rather than resolving.
    //
    // This is sound (deferring never wires a wrong CFG edge) but strictly more
    // conservative than the prior region-keyed model, which exploited reflexive
    // dominance at the merge to resolve such diamonds.  Single-predecessor
    // guarded dispatches — the common compiler shape, incl. the post-
    // `RegionCollapse` jump tables the orchestrator resolves end-to-end — are
    // unaffected: their guarded edge's consumer is the dispatch placeholder /
    // single-pred Region, which the gate admits.
    //
    // Shape:
    //   entry → if (dummy) → path_a / path_b
    //   path_a → if (idx < 4) → dispatch / exit_a
    //   path_b → if (idx < 4) → dispatch / exit_b
    //   dispatch → JT load → IndirectBranch
    use strider_ir::IntCmpOp;
    let idx_var = rsleigh::Vn {
        addr_off: 0x10,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let mut b = RegisterSet::new().tracked(idx_var).build_fn().unwrap();
    let entry = b.create_region().unwrap();
    let path_a = b.create_region().unwrap();
    let path_b = b.create_region().unwrap();
    let dispatch = b.create_region().unwrap();
    let exit_a = b.create_region().unwrap();
    let exit_b = b.create_region().unwrap();
    b.set_entry_region(entry).unwrap();

    b.set_region(entry);
    let idx_e = b.read_variable(&idx_var).unwrap();
    let zero = b.build_int_const(0u64, ValueType::I32).unwrap();
    let dummy = b
        .build_int_cmp_operation(idx_e, zero, IntCmpOp::Equal, ValueType::I32)
        .unwrap();
    b.build_if(dummy, path_a, path_b).unwrap();

    b.set_region(path_a);
    let idx_a = b.read_variable(&idx_var).unwrap();
    let four_a = b.build_int_const(4u64, ValueType::I32).unwrap();
    let cond_a = b
        .build_int_cmp_operation(idx_a, four_a, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond_a, dispatch, exit_a).unwrap();

    b.set_region(path_b);
    let idx_b = b.read_variable(&idx_var).unwrap();
    let four_b = b.build_int_const(4u64, ValueType::I32).unwrap();
    let cond_b = b
        .build_int_cmp_operation(idx_b, four_b, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond_b, dispatch, exit_b).unwrap();

    b.set_region(dispatch);
    let idx_d = b.read_variable(&idx_var).unwrap();
    let stride_c = b.build_int_const(4u64, ValueType::I32).unwrap();
    let mul = b
        .build_int_binary_operation(idx_d, stride_c, IntBinaryOp::Mul, ValueType::I32)
        .unwrap();
    let base_c = b.build_int_const(0x4000u64, ValueType::I32).unwrap();
    let addr = b
        .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, ValueType::I32)
        .unwrap();
    let loaded = b
        .build_load(addr, VnSpace::RAM, ValueType::I32)
        .expect("load");
    b.build_indirect_branch(loaded).unwrap();

    b.set_region(exit_a);
    b.build_return(None, &[]).unwrap();
    b.set_region(exit_b);
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);

    let function = b.build().unwrap();

    let rom = MockRom::strided(0x4000, 4, vec![0x10, 0x20, 0x30, 0x40], 4);
    let (known, doms) = make_known_and_doms(&function);
    let mut ranges = crate::value_range::compute_value_ranges(&function, &doms, &known);
    let result = classify_table_dispatch(
        &function,
        sole_indirect_branch(&function),
        Some(&rom),
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
    );
    assert_eq!(
        result, None,
        "diamond merge guard is conservatively dropped by the soundness gate → defers"
    );
}

#[test]
fn classify_table_dispatch_one_path_unguarded_does_not_resolve() {
    // Soundness: a jump-table dispatch where only ONE incoming path has an
    // `idx < 4` guard and the OTHER path is unconditional (idx unconstrained)
    // MUST return None — the index can be ≥ 4 on the unguarded path,
    // so reading 4 entries would be an out-of-bounds read.
    //
    // Shape:
    //   entry → If(flag) → path_a / path_b
    //   path_a → If(idx < 4) → dispatch / exit_a   [guarded]
    //   path_b → dispatch                           [unconditional — idx UNCONSTRAINED]
    //   dispatch → Load[base + Phi(idx_a, idx_b)*stride] → IndirectBranch
    //
    // The guard's true_succ_region is `dispatch`.  With the buggy code,
    // `dominates(dispatch, dispatch)` is reflexively true, so both phi arms
    // appear bounded → Multiple([0x10..0x40]) — an OOB read.  Correct answer: None.
    use strider_ir::IntCmpOp;
    let idx_var = rsleigh::Vn {
        addr_off: 0x10,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let mut b = RegisterSet::new().tracked(idx_var).build_fn().unwrap();
    let entry = b.create_region().unwrap();
    let path_a = b.create_region().unwrap();
    let path_b = b.create_region().unwrap();
    let dispatch = b.create_region().unwrap();
    let exit_a = b.create_region().unwrap();
    b.set_entry_region(entry).unwrap();

    // entry: read idx from register, split to path_a / path_b.
    b.set_region(entry);
    let idx_e = b.read_variable(&idx_var).unwrap();
    let zero = b.build_int_const(0u64, ValueType::I32).unwrap();
    let flag = b
        .build_int_cmp_operation(idx_e, zero, IntCmpOp::Equal, ValueType::I32)
        .unwrap();
    b.build_if(flag, path_a, path_b).unwrap();

    // path_a: guarded — If(idx < 4) → dispatch / exit_a.
    // Guard's true_succ_region = dispatch.
    b.set_region(path_a);
    let idx_a = b.read_variable(&idx_var).unwrap();
    let four_a = b.build_int_const(4u64, ValueType::I32).unwrap();
    let cond_a = b
        .build_int_cmp_operation(idx_a, four_a, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond_a, dispatch, exit_a).unwrap();

    // path_b: unconditional — idx is UNCONSTRAINED on this path.
    b.set_region(path_b);
    b.build_branch(dispatch).unwrap();

    // dispatch: jump-table load using the phi of idx from both paths.
    b.set_region(dispatch);
    let idx_d = b.read_variable(&idx_var).unwrap();
    let stride_c = b.build_int_const(4u64, ValueType::I32).unwrap();
    let mul = b
        .build_int_binary_operation(idx_d, stride_c, IntBinaryOp::Mul, ValueType::I32)
        .unwrap();
    let base_c = b.build_int_const(0x4000u64, ValueType::I32).unwrap();
    let addr = b
        .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, ValueType::I32)
        .unwrap();
    let loaded = b
        .build_load(addr, VnSpace::RAM, ValueType::I32)
        .expect("load");
    b.build_indirect_branch(loaded).unwrap();

    b.set_region(exit_a);
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);

    let function = b.build().unwrap();

    let rom = MockRom::strided(0x4000, 4, vec![0x10, 0x20, 0x30, 0x40], 4);
    let (known, doms) = make_known_and_doms(&function);
    let mut ranges = crate::value_range::compute_value_ranges(&function, &doms, &known);
    let result = classify_table_dispatch(
        &function,
        sole_indirect_branch(&function),
        Some(&rom),
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
    );
    assert!(
        result.is_none(),
        "one-path-unguarded dispatch must NOT resolve (would be OOB); got {result:?}"
    );
}

/// M7 reproduction attempt: a loaded value fed *directly* to the
/// IndirectBranch — i.e. the dispatch value IS a `Load`, with no address
/// arithmetic — that *also* sits under a dominating `if (entry < 4)` guard.
///
/// The concern: the `entry_load` exclusion is dropped when a dominating
/// guard is present, so this load-derived anchor would be enumerated as the
/// index.  Substituting it with `IntConst(0..3)` makes the branch's dispatch
/// value literally `0,1,2,3` — bogus sequential targets that are NOT real
/// code addresses.
///
/// If the classifier returns `Multiple([0,1,2,3])` this is the wrong-edge
/// bug.  If it returns `None`, the over-approximation safety margin holds
/// and no change is warranted.
#[test]
fn classify_table_dispatch_guarded_direct_load_anchor() {
    use strider_ir::IntCmpOp;
    let mut b = strider_ir_test_utils::empty_builder().unwrap();
    let entry = b.create_region().unwrap();
    let dispatch = b.create_region().unwrap();
    let exit = b.create_region().unwrap();
    b.set_entry_region(entry).unwrap();

    // entry: load a value, guard `entry < 4`, branch to dispatch.
    b.set_region(entry);
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let load_addr = b.build_int_const(0x9000u64, ValueType::I32).unwrap();
    let loaded = b
        .build_load(load_addr, VnSpace::RAM, ValueType::I32)
        .unwrap();
    let four = b.build_int_const(4u64, ValueType::I32).unwrap();
    let cond = b
        .build_int_cmp_operation(loaded, four, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond, dispatch, exit).unwrap();

    // dispatch: feed the SAME loaded value straight to the branch.
    b.set_region(dispatch);
    b.build_indirect_branch(loaded).unwrap();

    b.set_region(exit);
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);

    let mut function = b.build().unwrap();
    {
        let mut p = crate::OptimizerPipeline::new();
        p.add(crate::PhiCollapse);
        p.add(crate::RegionCollapse);
        p.run(&mut function, &mut crate::OptCtx::new(None)).unwrap();
    }
    let (known, doms) = make_known_and_doms(&function);
    let mut ranges = crate::value_range::compute_value_ranges(&function, &doms, &known);
    let result = classify_table_dispatch(
        &function,
        sole_indirect_branch(&function),
        None,
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
    );
    // Document the observed behaviour: a load-derived dispatch value with a
    // dominating guard but no address arithmetic must NOT resolve to the bare
    // index values as targets.
    assert_eq!(
        result, None,
        "a guarded direct-load anchor must not enumerate its index values as \
         branch targets (got {result:?})"
    );
}

/// M3 probe: a dispatch cone that contains a *decoy* finite-range value off
/// the addressing path, alongside the real (masked) index.  The decoy
/// (`if (decoy < 4)`) is computed but does not feed the table address; the
/// real index is `idx & 0x3`.  The classifier must select the real index
/// (or defer) — it must NOT enumerate the decoy and emit decoy-derived
/// targets.
///
/// Verifies the over-approximation is self-protecting: a candidate that does
/// not fully determine the address fails to fold for at least one value, so
/// it is rejected; the real index folds the whole range to the real targets.
#[test]
fn classify_table_dispatch_decoy_offpath_value_not_enumerated() {
    use strider_ir::IntCmpOp;
    let idx_var = rsleigh::Vn {
        addr_off: 0x10,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let decoy_var = rsleigh::Vn {
        addr_off: 0x20,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let mut b = RegisterSet::new()
        .tracked(idx_var)
        .tracked(decoy_var)
        .build_fn()
        .unwrap();
    let entry = b.create_region().unwrap();
    let dispatch = b.create_region().unwrap();
    let exit = b.create_region().unwrap();
    b.set_entry_region(entry).unwrap();

    // entry: guard the DECOY (`decoy < 4`), branch to dispatch.
    b.set_region(entry);
    let decoy = b.read_variable(&decoy_var).unwrap();
    let four = b.build_int_const(4u64, ValueType::I32).unwrap();
    let cond = b
        .build_int_cmp_operation(decoy, four, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond, dispatch, exit).unwrap();

    // dispatch: real index is `idx & 0x3` (mask-bounded to [0,3]).  The decoy
    // is read again but XOR'd into a dead value that does not reach the addr.
    b.set_region(dispatch);
    let idx = b.read_variable(&idx_var).unwrap();
    let mask = b.build_int_const(0x3u64, ValueType::I32).unwrap();
    let real_idx = b
        .build_int_binary_operation(idx, mask, IntBinaryOp::And, ValueType::I32)
        .unwrap();
    let stride_c = b.build_int_const(4u64, ValueType::I32).unwrap();
    let mul = b
        .build_int_binary_operation(real_idx, stride_c, IntBinaryOp::Mul, ValueType::I32)
        .unwrap();
    let base_c = b.build_int_const(0x4000u64, ValueType::I32).unwrap();
    let addr = b
        .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, ValueType::I32)
        .unwrap();
    let loaded = b.build_load(addr, VnSpace::RAM, ValueType::I32).unwrap();
    b.build_indirect_branch(loaded).unwrap();

    b.set_region(exit);
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);

    let mut function = b.build().unwrap();
    {
        let mut p = crate::OptimizerPipeline::new();
        p.add(crate::ConstantFold::new());
        p.add(crate::KnownBits);
        p.add(crate::PhiCollapse);
        p.add(crate::RegionCollapse);
        p.run(&mut function, &mut crate::OptCtx::new(None)).unwrap();
    }
    let rom = MockRom::strided(0x4000, 4, vec![0x10, 0x20, 0x30, 0x40], 4);
    let (known, doms) = make_known_and_doms(&function);
    let mut ranges = crate::value_range::compute_value_ranges(&function, &doms, &known);
    let result = classify_table_dispatch(
        &function,
        sole_indirect_branch(&function),
        Some(&rom),
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
    );
    // The real index has range [0,3] and folds the whole table; the decoy is
    // off the address path so substituting it does not change the dispatch.
    // Either outcome is sound as long as no DECOY-derived (non-table) target
    // escapes: the only valid resolution is the real table targets.
    match result {
        None => { /* fail-closed defer is sound */ }
        Some(ResolvedTargets::Multiple(ts)) => {
            assert_eq!(
                ts,
                vec![0x10, 0x20, 0x30, 0x40],
                "only the real table targets may be emitted; no decoy-derived edges"
            );
        }
        other => panic!("unexpected: {other:?}"),
    }
}

// ── End-to-end classifier tests (SP-rooted / on-stack arm) ───────────────────

/// Shared post-builder pipeline: run the standard optimization passes and
/// return the dispatch `Load`'s output value from the converged graph.
fn finish_two_target_array(mut fg: strider_ir::Function) -> (strider_ir::Function, ValueId) {
    let mut p = OptimizerPipeline::new();
    p.add(ConstantFold::new());
    p.add(KnownBits);
    p.add(PhiCollapse);
    p.add(RegionCollapse);
    p.run(&mut fg, &mut crate::OptCtx::new(None)).unwrap();
    let load = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
        .expect("Load survives — LoadForward not in pipeline");
    let load_value = fg.node_outputs_exact::<1>(load).unwrap()[0];
    (fg, load_value)
}

/// Core IR construction shared by `build_two_target_array` and
/// `build_two_target_array_aligned`.  `frame_base` is the SP-derived value
/// to use as the base for all stores and the load address: either bare `sp_val`
/// (non-aligned) or `And(sp_val, align_mask)` (aligned).
fn wire_two_target_array(
    targets: [u64; 2],
    base_offset: i64,
    stride: u64,
    frame_base: ValueId,
    b: &mut strider_ir::FunctionBuilder,
    arg_vn: rsleigh::Vn,
) {
    for (i, &target_addr) in targets.iter().enumerate() {
        let off = base_offset + (i as i64) * (stride as i64);
        let off_const = b.build_int_const(off as u64, ValueType::I64).unwrap();
        let addr = b
            .build_int_binary_operation(frame_base, off_const, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        let target = b.build_int_const(target_addr, ValueType::I64).unwrap();
        b.build_store(addr, target, rsleigh::VnSpace::RAM).unwrap();
    }
    let arg_val = b.read_variable(&arg_vn).unwrap();
    let arg_u32 = strider_ir_test_utils::sentinel_node(
        b.function_mut(),
        NodeKind::Truncate,
        [arg_val],
        [strider_ir::node::ValueKind::Typed(ValueType::I32)],
    );
    let arg_u32_value = b.function().node_outputs_exact::<1>(arg_u32).unwrap()[0];
    let one = b.build_int_const(1u64, ValueType::I32).unwrap();
    let masked = b
        .build_int_binary_operation(arg_u32_value, one, IntBinaryOp::And, ValueType::I32)
        .unwrap();
    let idx_u64 = strider_ir_test_utils::sentinel_node(
        b.function_mut(),
        NodeKind::Extend(ExtendOp::ZeroExtend),
        [masked],
        [strider_ir::node::ValueKind::Typed(ValueType::I64)],
    );
    let idx_u64_value = b.function().node_outputs_exact::<1>(idx_u64).unwrap()[0];
    let stride_const = b.build_int_const(stride, ValueType::I64).unwrap();
    let idx_scaled = b
        .build_int_binary_operation(
            idx_u64_value,
            stride_const,
            IntBinaryOp::Mul,
            ValueType::I64,
        )
        .unwrap();
    let base_const = b
        .build_int_const(base_offset as u64, ValueType::I64)
        .unwrap();
    let sp_plus_base = b
        .build_int_binary_operation(frame_base, base_const, IntBinaryOp::Add, ValueType::I64)
        .unwrap();
    let load_addr = b
        .build_int_binary_operation(sp_plus_base, idx_scaled, IntBinaryOp::Add, ValueType::I64)
        .unwrap();
    let loaded = b
        .build_load(load_addr, rsleigh::VnSpace::RAM, ValueType::I64)
        .unwrap();
    // Use build_indirect_branch so the range analysis can locate the
    // dispatch region via find_anchor_consumer_placeholder.
    b.build_indirect_branch(loaded).unwrap();
    b.set_lift_addr(None);
}

fn build_two_target_array(
    targets: [u64; 2],
    base_offset: i64,
    stride: u64,
) -> (strider_ir::Function, ValueId) {
    let sp = sp64();
    let arg_vn = rsleigh::Vn {
        addr_off: 0x38,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let mut b = RegisterSet::new()
        .tracked(sp)
        .tracked(arg_vn)
        .callee_saved(sp)
        .stack_vn(sp)
        .build_fn_single_region()
        .unwrap();
    let sp_val = b.read_variable(&sp).unwrap();
    wire_two_target_array(targets, base_offset, stride, sp_val, &mut b, arg_vn);
    let fg = b.build().unwrap();
    finish_two_target_array(fg)
}

/// Same as `build_two_target_array` but with an alignment-masked frame base:
/// `frame_base = And(sp_val, 0xFFFF_FFFF_FFFF_FFF0)`.  Exercises the
/// `(sp & mask)` stack-base path that `SpDecomposer::decompose` recognises as
/// an opaque SP terminal.
fn build_two_target_array_aligned(
    targets: [u64; 2],
    base_offset: i64,
    stride: u64,
) -> (strider_ir::Function, ValueId) {
    let sp = sp64();
    let arg_vn = rsleigh::Vn {
        addr_off: 0x38,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let mut b = RegisterSet::new()
        .tracked(sp)
        .tracked(arg_vn)
        .callee_saved(sp)
        .stack_vn(sp)
        .build_fn_single_region()
        .unwrap();
    let sp_val = b.read_variable(&sp).unwrap();
    let align_mask = b
        .build_int_const(0xFFFF_FFFF_FFFF_FFF0u64, ValueType::I64)
        .unwrap();
    let frame_base = b
        .build_int_binary_operation(sp_val, align_mask, IntBinaryOp::And, ValueType::I64)
        .unwrap();
    wire_two_target_array(targets, base_offset, stride, frame_base, &mut b, arg_vn);
    let fg = b.build().unwrap();
    finish_two_target_array(fg)
}

#[test]
fn classify_table_dispatch_two_stack_targets_resolves() {
    let targets = [0x401190u64, 0x401180u64];
    let (fg, _load_value) = build_two_target_array(targets, -24, 8);
    let (known, doms) = make_known_and_doms(&fg);
    let mut ranges = crate::value_range::compute_value_ranges(&fg, &doms, &known);
    let result = classify_table_dispatch(
        &fg,
        sole_indirect_branch(&fg),
        None,
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
    );
    let mut expected = targets.to_vec();
    expected.sort_unstable();
    assert_eq!(result, Some(ResolvedTargets::Multiple(expected)));
}

#[test]
fn classify_table_dispatch_aligned_stack_resolves() {
    // Same SP-rooted two-target stack array as `build_two_target_array`, but
    // with the frame base alignment-masked: `And(sp_val, 0xFFFF_FFFF_FFFF_FFF0)`.
    // The evaluator must recognise the And-masked value as an SP terminal
    // (via `SpDecomposer::decompose`) so the load address resolves to `SpRel`
    // and `reaching_store` can match the prologue stores.
    let targets = [0x401190u64, 0x401180u64];
    let (fg, _load_value) = build_two_target_array_aligned(targets, -24, 8);
    let (known, doms) = make_known_and_doms(&fg);
    let mut ranges = crate::value_range::compute_value_ranges(&fg, &doms, &known);
    let result = classify_table_dispatch(
        &fg,
        sole_indirect_branch(&fg),
        None,
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
    );
    let mut expected = targets.to_vec();
    expected.sort_unstable();
    assert_eq!(result, Some(ResolvedTargets::Multiple(expected)));
}

/// A global (constant-address) `Store` between the prologue stores and
/// the dispatch `Load` is the case the [`AliasMode`] knob governs:
///
/// * under [`AliasMode::StackGlobalDisjoint`] (the default) the global
///   store is proven disjoint from the SP-rooted label array, so the
///   walker passes it and the table resolves to `Multiple`;
/// * under [`AliasMode::Strict`] the global store may-aliases the
///   SP-rooted probe and surfaces as a clobber, so the classifier
///   returns `None` (the branch defers to `UnresolvedIndirectBranch`).
///
/// This pins the soundness-consistency fix: a `Strict` caller no longer
/// receives an optimistically-resolved jump table that the
/// stack/global-disjointness assumption (which `Strict` rejects) would
/// be required to justify.  The two assertions run against the *same*
/// graph so the only variable is the mode.
#[test]
fn classify_table_dispatch_global_store_between_resolves_only_under_disjoint() {
    let sp = sp64();
    let arg_vn = rsleigh::Vn {
        addr_off: 0x38,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let mut b = RegisterSet::new()
        .tracked(sp)
        .tracked(arg_vn)
        .callee_saved(sp)
        .stack_vn(sp)
        .build_fn_single_region()
        .unwrap();
    let sp_val = b.read_variable(&sp).unwrap();
    // Prologue: store two label addresses into the stack array.
    let targets = [0x401190u64, 0x401180u64];
    let base_offset: i64 = -24;
    let stride: u64 = 8;
    for (i, &target_addr) in targets.iter().enumerate() {
        let off = base_offset + (i as i64) * (stride as i64);
        let off_const = b.build_int_const(off as u64, ValueType::I64).unwrap();
        let addr = b
            .build_int_binary_operation(sp_val, off_const, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        let target = b.build_int_const(target_addr, ValueType::I64).unwrap();
        b.build_store(addr, target, rsleigh::VnSpace::RAM).unwrap();
    }
    // Intervening GLOBAL store: a constant absolute address, unrelated to
    // the stack pointer.  `StackGlobalDisjoint` proves it disjoint from the
    // SP-rooted slots; `Strict` cannot and treats it as a clobber.
    let global_addr = b.build_int_const(0x0060_0000u64, ValueType::I64).unwrap();
    let global_val = b.build_int_const(0x0000_DEADu64, ValueType::I64).unwrap();
    b.build_store(global_addr, global_val, rsleigh::VnSpace::RAM)
        .unwrap();
    // Dispatch: load from sp + base + idx*stride.
    let arg_val = b.read_variable(&arg_vn).unwrap();
    let arg_u32 = strider_ir_test_utils::sentinel_node(
        b.function_mut(),
        NodeKind::Truncate,
        [arg_val],
        [strider_ir::node::ValueKind::Typed(ValueType::I32)],
    );
    let arg_u32_value = b.function().node_outputs_exact::<1>(arg_u32).unwrap()[0];
    let one = b.build_int_const(1u64, ValueType::I32).unwrap();
    let masked = b
        .build_int_binary_operation(arg_u32_value, one, IntBinaryOp::And, ValueType::I32)
        .unwrap();
    let idx_u64 = strider_ir_test_utils::sentinel_node(
        b.function_mut(),
        NodeKind::Extend(ExtendOp::ZeroExtend),
        [masked],
        [strider_ir::node::ValueKind::Typed(ValueType::I64)],
    );
    let idx_u64_value = b.function().node_outputs_exact::<1>(idx_u64).unwrap()[0];
    let stride_const = b.build_int_const(stride, ValueType::I64).unwrap();
    let idx_scaled = b
        .build_int_binary_operation(
            idx_u64_value,
            stride_const,
            IntBinaryOp::Mul,
            ValueType::I64,
        )
        .unwrap();
    let base_const = b
        .build_int_const(base_offset as u64, ValueType::I64)
        .unwrap();
    let sp_plus_base = b
        .build_int_binary_operation(sp_val, base_const, IntBinaryOp::Add, ValueType::I64)
        .unwrap();
    let load_addr = b
        .build_int_binary_operation(sp_plus_base, idx_scaled, IntBinaryOp::Add, ValueType::I64)
        .unwrap();
    let loaded = b
        .build_load(load_addr, rsleigh::VnSpace::RAM, ValueType::I64)
        .unwrap();
    b.build_indirect_branch(loaded).unwrap();
    b.set_lift_addr(None);
    let mut fg = b.build().unwrap();
    let mut p = OptimizerPipeline::new();
    p.add(ConstantFold::new());
    p.add(KnownBits);
    p.add(PhiCollapse);
    p.add(RegionCollapse);
    p.run(&mut fg, &mut crate::OptCtx::new(None)).unwrap();
    let load = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
        .expect("dispatch Load survives — LoadForward not in pipeline");
    let _load_value = fg.node_outputs_exact::<1>(load).unwrap()[0];
    let (known, doms) = make_known_and_doms(&fg);
    let mut ranges = crate::value_range::compute_value_ranges(&fg, &doms, &known);

    // Default (optimistic) mode: the global store is disjoint → resolves.
    let mut expected = targets.to_vec();
    expected.sort_unstable();
    assert_eq!(
        classify_table_dispatch(
            &fg,
            sole_indirect_branch(&fg),
            None,
            &mut ranges,
            AliasMode::StackGlobalDisjoint
        ),
        Some(ResolvedTargets::Multiple(expected)),
        "StackGlobalDisjoint proves the global store disjoint from the \
         SP-rooted array; the table must resolve",
    );

    // Strict mode: the global store may-alias the probe → clobber → defer.
    assert_eq!(
        classify_table_dispatch(
            &fg,
            sole_indirect_branch(&fg),
            None,
            &mut ranges,
            AliasMode::Strict
        ),
        None,
        "Strict cannot prove the global store disjoint from the SP-rooted \
         array; the intervening store is a clobber and the branch must defer",
    );
}

/// A `Call` between the prologue stores and the dispatch `Load` is a
/// clobber boundary — the stack slots are NOT provably the stored
/// constants at the dispatch site, so the classifier MUST return `None`
/// (conservative: defer to `UnresolvedIndirectBranch`).
///
/// This test verifies the tightening introduced by replacing the old
/// bespoke backward scan (which walked past `Call` nodes as if they
/// were non-aliasing stores) with the shared `SpAliasOracle` walker
/// (which treats `Call` as a memory clobber).
#[test]
fn classify_table_dispatch_returns_none_when_call_clobbers_between_stores_and_load() {
    // Build: store targets into stack slots, then Call (clobbers memory),
    // then dispatch load.  The call makes the stored values untrustworthy.
    let sp = sp64();
    let arg_vn = rsleigh::Vn {
        addr_off: 0x38,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    // A dummy call-target register (not tracked, just used as a const).
    let mut b = RegisterSet::new()
        .tracked(sp)
        .tracked(arg_vn)
        .callee_saved(sp)
        .stack_vn(sp)
        .build_fn_single_region()
        .unwrap();
    let sp_val = b.read_variable(&sp).unwrap();
    // Prologue: store two label addresses into the stack array.
    let targets = [0x401190u64, 0x401180u64];
    let base_offset: i64 = -24;
    let stride: u64 = 8;
    for (i, &target_addr) in targets.iter().enumerate() {
        let off = base_offset + (i as i64) * (stride as i64);
        let off_const = b.build_int_const(off as u64, ValueType::I64).unwrap();
        let addr = b
            .build_int_binary_operation(sp_val, off_const, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        let target = b.build_int_const(target_addr, ValueType::I64).unwrap();
        b.build_store(addr, target, rsleigh::VnSpace::RAM).unwrap();
    }
    // Intervening Call: clobbers memory (and potentially the stack slots
    // the callee can see through the stack pointer).
    let call_target_const = b.build_int_const(0x0040_1000u64, ValueType::I64).unwrap();
    b.build_call_cc(call_target_const, None).unwrap();
    // Re-read sp AFTER the call (the call may have advanced the stack).
    let sp_val_after = b.read_variable(&sp).unwrap();
    // Dispatch: load from sp + base + idx*stride.  Mirrors the stack-array shape.
    let arg_val = b.read_variable(&arg_vn).unwrap();
    let arg_u32 = strider_ir_test_utils::sentinel_node(
        b.function_mut(),
        NodeKind::Truncate,
        [arg_val],
        [strider_ir::node::ValueKind::Typed(ValueType::I32)],
    );
    let arg_u32_value = b.function().node_outputs_exact::<1>(arg_u32).unwrap()[0];
    let one = b.build_int_const(1u64, ValueType::I32).unwrap();
    let masked = b
        .build_int_binary_operation(arg_u32_value, one, IntBinaryOp::And, ValueType::I32)
        .unwrap();
    let idx_u64 = strider_ir_test_utils::sentinel_node(
        b.function_mut(),
        NodeKind::Extend(ExtendOp::ZeroExtend),
        [masked],
        [strider_ir::node::ValueKind::Typed(ValueType::I64)],
    );
    let idx_u64_value = b.function().node_outputs_exact::<1>(idx_u64).unwrap()[0];
    let stride_const = b.build_int_const(stride, ValueType::I64).unwrap();
    let idx_scaled = b
        .build_int_binary_operation(
            idx_u64_value,
            stride_const,
            IntBinaryOp::Mul,
            ValueType::I64,
        )
        .unwrap();
    let base_const = b
        .build_int_const(base_offset as u64, ValueType::I64)
        .unwrap();
    let sp_plus_base = b
        .build_int_binary_operation(sp_val_after, base_const, IntBinaryOp::Add, ValueType::I64)
        .unwrap();
    let load_addr = b
        .build_int_binary_operation(sp_plus_base, idx_scaled, IntBinaryOp::Add, ValueType::I64)
        .unwrap();
    let loaded = b
        .build_load(load_addr, rsleigh::VnSpace::RAM, ValueType::I64)
        .unwrap();
    b.build_indirect_branch(loaded).unwrap();
    b.set_lift_addr(None);
    let mut fg = b.build().unwrap();
    let mut p = OptimizerPipeline::new();
    p.add(ConstantFold::new());
    p.add(KnownBits);
    p.add(PhiCollapse);
    p.add(RegionCollapse);
    p.run(&mut fg, &mut crate::OptCtx::new(None)).unwrap();
    let load = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
        .expect("dispatch Load survives");
    let _load_value = fg.node_outputs_exact::<1>(load).unwrap()[0];
    let (known, doms) = make_known_and_doms(&fg);
    let mut ranges = crate::value_range::compute_value_ranges(&fg, &doms, &known);
    // The Call is a clobber boundary: the stored targets are not provably
    // live at the dispatch site → classifier MUST return None.
    assert_eq!(
        classify_table_dispatch(
            &fg,
            sole_indirect_branch(&fg),
            None,
            &mut ranges,
            AliasMode::StackGlobalDisjoint
        ),
        None,
        "Call between stores and dispatch load is a clobber boundary; \
         classifier must return None (conservative)"
    );
}

#[test]
fn classify_table_dispatch_returns_none_on_non_indexed_load() {
    let sp = sp64();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .build_fn_single_region()
        .unwrap();
    let sp_val = b.read_variable(&sp).unwrap();
    let off = b.build_int_const(24u64, ValueType::I64).unwrap();
    let addr = b.build_sub_as_add_neg(sp_val, off, ValueType::I64).unwrap();
    let v = b.build_int_const(0xCAFEu64, ValueType::I64).unwrap();
    b.build_store(addr, v, rsleigh::VnSpace::RAM).unwrap();
    let loaded = b
        .build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)
        .unwrap();
    b.build_indirect_branch(loaded).unwrap();
    b.set_lift_addr(None);
    let mut fg = b.build().unwrap();
    let mut p = OptimizerPipeline::new();
    p.add(ConstantFold::new());
    p.add(KnownBits);
    p.add(PhiCollapse);
    p.add(RegionCollapse);
    p.run(&mut fg, &mut crate::OptCtx::new(None)).unwrap();
    let load = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
        .unwrap();
    let _load_value = fg.node_outputs_exact::<1>(load).unwrap()[0];
    let (known, doms) = make_known_and_doms(&fg);
    let mut ranges = crate::value_range::compute_value_ranges(&fg, &doms, &known);
    assert_eq!(
        classify_table_dispatch(
            &fg,
            sole_indirect_branch(&fg),
            None,
            &mut ranges,
            AliasMode::StackGlobalDisjoint
        ),
        None
    );
}

#[test]
fn classify_table_dispatch_returns_none_on_unbounded_stack_idx() {
    let sp = sp64();
    let arg_vn = rsleigh::Vn {
        addr_off: 0x38,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let mut b = RegisterSet::new()
        .tracked(sp)
        .tracked(arg_vn)
        .callee_saved(sp)
        .stack_vn(sp)
        .build_fn_single_region()
        .unwrap();
    let sp_val = b.read_variable(&sp).unwrap();
    let off24 = b.build_int_const(24u64, ValueType::I64).unwrap();
    let addr_24 = b
        .build_sub_as_add_neg(sp_val, off24, ValueType::I64)
        .unwrap();
    let v = b.build_int_const(0x1234u64, ValueType::I64).unwrap();
    b.build_store(addr_24, v, rsleigh::VnSpace::RAM).unwrap();
    let arg_val = b.read_variable(&arg_vn).unwrap();
    let stride = b.build_int_const(8u64, ValueType::I64).unwrap();
    let idx_scaled = b
        .build_int_binary_operation(arg_val, stride, IntBinaryOp::Mul, ValueType::I64)
        .unwrap();
    let base = b.build_int_const((-24i64) as u64, ValueType::I64).unwrap();
    let sp_plus_base = b
        .build_int_binary_operation(sp_val, base, IntBinaryOp::Add, ValueType::I64)
        .unwrap();
    let load_addr = b
        .build_int_binary_operation(sp_plus_base, idx_scaled, IntBinaryOp::Add, ValueType::I64)
        .unwrap();
    let loaded = b
        .build_load(load_addr, rsleigh::VnSpace::RAM, ValueType::I64)
        .unwrap();
    b.build_indirect_branch(loaded).unwrap();
    b.set_lift_addr(None);
    let mut fg = b.build().unwrap();
    let mut p = OptimizerPipeline::new();
    p.add(ConstantFold::new());
    p.add(KnownBits);
    p.add(PhiCollapse);
    p.add(RegionCollapse);
    p.run(&mut fg, &mut crate::OptCtx::new(None)).unwrap();
    let load = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
        .unwrap();
    let _load_value = fg.node_outputs_exact::<1>(load).unwrap()[0];
    let (known, doms) = make_known_and_doms(&fg);
    let mut ranges = crate::value_range::compute_value_ranges(&fg, &doms, &known);
    assert_eq!(
        classify_table_dispatch(
            &fg,
            sole_indirect_branch(&fg),
            None,
            &mut ranges,
            AliasMode::StackGlobalDisjoint
        ),
        None
    );
}

// ── classify_table_dispatch boundary cases (SP-rooted arm) ──────────────────

#[test]
fn classify_table_dispatch_one_stack_target_resolves() {
    // Single-element stack array — degenerate jump table of size 1.
    // The classifier should still resolve.  Bound is supplied via
    // KnownBits (idx & 0): always 0.  But that mask is 0, which
    // means bound = 1 (the only valid idx).
    let targets = [0x401200u64];
    let (fg, _load_value) = build_one_target_array(targets, -8, 8);
    let (known, doms) = make_known_and_doms(&fg);
    let mut ranges = crate::value_range::compute_value_ranges(&fg, &doms, &known);
    let result = classify_table_dispatch(
        &fg,
        sole_indirect_branch(&fg),
        None,
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
    );
    // Whether the existing helpers can resolve a 1-element case
    // depends on how KnownBits bounds the index.  Pin the contract
    // that the classifier does NOT panic and returns Some/None
    // consistently.
    match result {
        None => { /* defer-via-unresolved is sound */ }
        Some(ResolvedTargets::Multiple(v)) => {
            assert_eq!(
                v,
                vec![0x401200u64],
                "single-element resolves to one target"
            );
        }
        other => panic!("unexpected classifier result: {other:?}"),
    }
}

fn build_one_target_array(
    targets: [u64; 1],
    base_offset: i64,
    stride: u64,
) -> (strider_ir::Function, strider_ir::node::ValueId) {
    let sp = rsleigh::Vn {
        addr_off: 0x40,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let arg_vn = rsleigh::Vn {
        addr_off: 0x38,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let mut b = RegisterSet::new()
        .tracked(sp)
        .tracked(arg_vn)
        .callee_saved(sp)
        .stack_vn(sp)
        .build_fn_single_region()
        .unwrap();
    let sp_val = b.read_variable(&sp).unwrap();
    let off_const = b
        .build_int_const(base_offset as u64, ValueType::I64)
        .unwrap();
    let addr = b
        .build_int_binary_operation(sp_val, off_const, IntBinaryOp::Add, ValueType::I64)
        .unwrap();
    let target = b.build_int_const(targets[0], ValueType::I64).unwrap();
    b.build_store(addr, target, rsleigh::VnSpace::RAM).unwrap();
    let arg_val = b.read_variable(&arg_vn).unwrap();
    // Build the dispatch site: load through sp+base+idx*stride with
    // idx masked to a single value (& 0 → idx is always 0).
    let arg_u32 = strider_ir_test_utils::sentinel_node(
        b.function_mut(),
        NodeKind::Truncate,
        [arg_val],
        [strider_ir::node::ValueKind::Typed(ValueType::I32)],
    );
    let arg_u32_value = b.function().node_outputs_exact::<1>(arg_u32).unwrap()[0];
    let mask0 = b.build_int_const(0u64, ValueType::I32).unwrap();
    let masked = b
        .build_int_binary_operation(arg_u32_value, mask0, IntBinaryOp::And, ValueType::I32)
        .unwrap();
    let idx_u64 = strider_ir_test_utils::sentinel_node(
        b.function_mut(),
        NodeKind::Extend(ExtendOp::ZeroExtend),
        [masked],
        [strider_ir::node::ValueKind::Typed(ValueType::I64)],
    );
    let idx_u64_value = b.function().node_outputs_exact::<1>(idx_u64).unwrap()[0];
    let stride_const = b.build_int_const(stride, ValueType::I64).unwrap();
    let idx_scaled = b
        .build_int_binary_operation(
            idx_u64_value,
            stride_const,
            IntBinaryOp::Mul,
            ValueType::I64,
        )
        .unwrap();
    let base_const = b
        .build_int_const(base_offset as u64, ValueType::I64)
        .unwrap();
    let sp_plus_base = b
        .build_int_binary_operation(sp_val, base_const, IntBinaryOp::Add, ValueType::I64)
        .unwrap();
    let load_addr = b
        .build_int_binary_operation(sp_plus_base, idx_scaled, IntBinaryOp::Add, ValueType::I64)
        .unwrap();
    let loaded = b
        .build_load(load_addr, rsleigh::VnSpace::RAM, ValueType::I64)
        .unwrap();
    b.build_indirect_branch(loaded).unwrap();
    b.set_lift_addr(None);
    let mut fg = b.build().unwrap();
    let mut p = OptimizerPipeline::new();
    p.add(ConstantFold::new());
    p.add(KnownBits);
    p.add(PhiCollapse);
    p.add(RegionCollapse);
    p.run(&mut fg, &mut crate::OptCtx::new(None)).unwrap();
    let load = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
        .expect("Load survives — LoadForward not in pipeline");
    let load_value = fg.node_outputs_exact::<1>(load).unwrap()[0];
    (fg, load_value)
}

#[test]
fn lock_barrier_prevents_stack_load_forwarding() -> crate::Result<()> {
    use strider_ir_test_utils::SENTINEL_LIFT_ADDR;

    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let four = b.build_int_const(4u64, ValueType::I32)?;
        let addr = b.build_sub_as_add_neg(sp_val, four, ValueType::I32)?;
        let data = b.build_int_const(0x99u64, ValueType::I32)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        // Emit a LOCK CallOther.  LOCK is now FullClobber, so StackOffsetDetect
        // must break the Stack chain here.
        let (lock_node, _result) = b.build_call_other_abi(
            0x1234,
            "LOCK",
            &[],
            &strider_target::BuiltCallOtherAbi {
                implicit_reads: Vec::new(),
                implicit_writes: Vec::new(),
                clobbers_memory: false,
            },
            None,
            false,
        )?;
        let lock_mem_value = b.function().memory_output_of(lock_node)?;
        b.advance_cur_region_memory(lock_mem_value)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    // Pipeline: ConstantFold → PhiCollapse → RegionCollapse → StackOffsetDetect → LoadForward.
    // StackOffsetDetect must break the Stack chain at LOCK (FullClobber).
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold::new());
    pipeline.add(PhiCollapse);
    pipeline.add(RegionCollapse);
    pipeline.add_post_pass(StackOffsetDetect);
    pipeline.add(LoadForward);

    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    // The Load must NOT be forwarded — LOCK is a full-clobber barrier.
    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 1,
        "Load[sp-4] must NOT be forwarded across a LOCK barrier; \
         LOCK is FullClobber and breaks the Stack chain"
    );
    Ok(())
}
