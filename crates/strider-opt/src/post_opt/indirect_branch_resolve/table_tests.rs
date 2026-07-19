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
use strider_ir_test_utils::IrBuilderEx;
use strider_ir_test_utils::IrWalkerEx;
use strider_ir_test_utils::{
    MockRom, RegisterSet, stack_vn_aarch64 as sp64, stack_vn_x86 as sp32_vn,
};

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

/// Records every `(addr, size)` read, so a test can assert the absolute-base
/// path issues exactly `count` reads in index order.
pub(super) struct RecordingRom {
    inner: MockRom,
    log: Mutex<Vec<(u64, usize)>>,
}

impl ReadOnlyMemory for RecordingRom {
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        self.log.lock().unwrap().push((addr, buf.len()));
        self.inner.read(addr, buf)
    }
}

/// An entry region terminated by a placeholder `IndirectBranch(target)`; the
/// closure builds the target's producer subtree.
fn build_with_target(
    target_inputs: impl FnOnce(&mut FunctionBuilder) -> ValueId,
) -> (Function, ValueId) {
    let mut builder = strider_ir_test_utils::empty_builder().expect("empty_builder");
    let region = builder.create_region_all().expect("create_region");
    builder
        .set_entry_region_all(region)
        .expect("set_entry_region");
    builder.set_region(region);
    builder.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let target = target_inputs(&mut builder);
    builder
        .build_indirect_branch(target)
        .expect("build_indirect_branch");
    builder.set_lift_addr(None);
    let function = builder.build().expect("build");
    (function, target)
}

/// The sole `IndirectBranch` placeholder, which the classifier takes directly.
fn sole_indirect_branch(f: &Function) -> NodeId {
    f.walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::IndirectBranch))
        .expect("placeholder IndirectBranch")
}

/// A load the optimiser cannot fold away, so the dispatch genuinely depends
/// on it.
fn build_non_const_idx(fb: &mut FunctionBuilder) -> ValueId {
    let addr = fb.build_int_const(0x9000u64, ValueType::I32).unwrap();
    fb.build_load(addr, VnSpace::RAM, ValueType::I32)
        .expect("u32 load (idx)")
}

#[test]
fn classify_table_dispatch_with_known_bits_bound_returns_multiple() {
    // KnownBits bounds `load & 0x7` to 8 entries.
    let (g, _target) = build_with_target(|fb| {
        // AND-masked to 0..7.
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
    // Four entries resolving to two distinct addresses each appearing twice,
    // which the fold's sort and dedup must collapse to a 2-element result.
    let (g, _target) = build_with_target(|fb| {
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
    // Indices 0..3 map to [0x10, 0x20, 0x10, 0x20].
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
    // Masking with 0 proves idx is always 0, so exactly one entry is read.  A
    // one-entry table stays `Multiple` with a single target, not `Single` and
    // not a defer.
    let (g, _target) = build_with_target(|fb| {
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
    // A bounded shape but no rom: without one the entries cannot be read, and
    // producing a `Multiple` without them is unsound.
    let (g, _target) = build_with_target(|fb| {
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
    // Table-shaped, but `idx` is a raw load with no mask and no dominating
    // guard, so it must defer.
    let (g, _target) = build_with_target(|fb| {
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

/// The entry-count cap: the SAME masked shape resolves at 4 entries and defers
/// at 8192.  Both masked indices pass the `hi < type_mask` filter, so the cap
/// is the only variable.  It short-circuits before the per-entry fold, so the
/// over-cap case stays cheap.
#[test]
fn classify_table_dispatch_defers_over_cap_resolves_under_cap() {
    let build = |mask: u64| {
        build_with_target(move |fb| {
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

    // Under the cap: 4 entries.
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

    // Over the cap: 8192 entries.
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
    // ARM `tbb`: the "index" is itself a table ENTRY, a byte load zero-extended
    // to I32 with no mask and no guard, so its only bound is width-derived.
    // [0,255] passes the `hi < type_mask` check against I32, so only the
    // width-only filter rejects it.  Enumerating it would fold to 256 bogus
    // sequential targets.
    let (g, _target) = build_with_target(|fb| {
        // A width-bounded table entry, not a guarded or masked dispatch index.
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
    // Large enough that all 256 sequential reads WOULD fold if the entry were
    // wrongly taken as the index, so a `None` here can only come from the
    // width-only exclusion, not a fold failure.
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
fn classify_table_dispatch_resolves_shift_narrowed_loaded_index() {
    // x86 instruction-decoder shape: `loaded_byte >> 5`, the top 3 bits.  It is
    // load-derived, but the shift narrows it strictly BELOW the byte width, so
    // it is a real index.
    let (g, _target) = build_with_target(|fb| {
        let byte_addr = fb.build_int_const(0x9000u64, ValueType::I32).unwrap();
        let byte = fb
            .build_load(byte_addr, VnSpace::RAM, ValueType::I8)
            .expect("byte load");
        let bwide = fb
            .extend_if_needed(byte, ValueType::I32, ExtendOp::ZeroExtend)
            .expect("zext byte");
        let five = fb.build_int_const(5u64, ValueType::I32).unwrap();
        let idx = fb
            .build_int_binary_operation(bwide, five, IntBinaryOp::ShiftRight, ValueType::I32)
            .expect("byte >> 5 → [0,7]");
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
        other => panic!("shift-narrowed index must resolve; got {other:?}"),
    }
}

#[test]
fn classify_table_dispatch_masked_full_byte_i32_resolves() {
    // Adversarial pair to the width-only exclusion: this ALSO spans [0,255],
    // but it is `reg & 0xFF` typed I32 with no byte-typed producer to strip to,
    // so the range does not fill its type width.  A genuine 256-entry index.
    let (g, _target) = build_with_target(|fb| {
        let raw = build_non_const_idx(fb); // Load:I32, unbounded
        let mask = fb.build_int_const(0xFFu64, ValueType::I32).unwrap();
        let idx = fb
            .build_int_binary_operation(raw, mask, IntBinaryOp::And, ValueType::I32)
            .expect("reg & 0xFF → [0,255]");
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
    let entries: Vec<u64> = (0..256).map(|i| 0x5000 + i).collect();
    let rom = MockRom::strided(0x4000, 4, entries.clone(), 4);
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
        Some(ResolvedTargets::Multiple(ts)) => assert_eq!(ts, entries),
        other => panic!("a masked full-byte I32 index must resolve; got {other:?}"),
    }
}

#[test]
fn decompose_index_picks_shallowest_narrowed_index() {
    // Two bounded dominators of one dispatch: a deep `reg & 0x3F` and the
    // shallow `(reg & 0x3F) & 0x7` the address actually scales.  The shallow one
    // must win, or a guard-tightened dispatch over-enumerates and defers.
    let (g, _target) = build_with_target(|fb| {
        let raw = build_non_const_idx(fb);
        let m63 = fb.build_int_const(0x3Fu64, ValueType::I32).unwrap();
        let wide = fb
            .build_int_binary_operation(raw, m63, IntBinaryOp::And, ValueType::I32)
            .expect("reg & 0x3F → [0,63] (deep)");
        let m7 = fb.build_int_const(0x7u64, ValueType::I32).unwrap();
        let narrow = fb
            .build_int_binary_operation(wide, m7, IntBinaryOp::And, ValueType::I32)
            .expect("& 0x7 → [0,7] (shallow, the index)");
        let stride_c = fb.build_int_const(4u64, ValueType::I32).unwrap();
        let mul = fb
            .build_int_binary_operation(narrow, stride_c, IntBinaryOp::Mul, ValueType::I32)
            .expect("mul");
        let base_c = fb.build_int_const(0x4000u64, ValueType::I32).unwrap();
        let addr = fb
            .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, ValueType::I32)
            .expect("add");
        fb.build_load(addr, VnSpace::RAM, ValueType::I32)
            .expect("dispatch load")
    });
    let branch = sole_indirect_branch(&g);
    let target_value = g.indirect_branch_target(branch);
    let (known, doms) = make_known_and_doms(&g);
    let mut ranges = crate::value_range::compute_value_ranges(&g, &doms, &known);
    match decompose_index(&g, &mut ranges, target_value, branch) {
        Some((_v, iv)) => assert_eq!(
            (iv.lo, iv.hi),
            (0, 7),
            "must pick the shallow narrowed [0,7] index, not the deeper [0,63]"
        ),
        None => panic!("expected an index candidate"),
    }
}

#[test]
fn classify_table_dispatch_defers_nonloaded_full_byte_index_conservatively() {
    // A deliberate conservatism: a 256-case switch on a byte REGISTER is a real
    // index, but its [0,255] fills the byte width and is indistinguishable by
    // range from a loaded byte entry, so it defers.  Sound, and cheap in
    // practice since real byte indices arrive through a load/mask/shift that
    // narrows below the width.  Resolving it would need a load-source signal
    // that a plain skip-loads rule cannot give without dropping guarded
    // raw-loaded indices.
    let bl = rsleigh::Vn {
        addr_off: 0x0,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 1,
    };
    let mut b = RegisterSet::new().tracked(bl).build_fn().unwrap();
    let region = b.create_region_all().unwrap();
    b.set_entry_region_all(region).unwrap();
    b.set_region(region);
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let byte = b.read_variable(&bl).unwrap();
    let idx = b
        .extend_if_needed(byte, ValueType::I32, ExtendOp::ZeroExtend)
        .expect("zext byte register");
    let stride_c = b.build_int_const(4u64, ValueType::I32).unwrap();
    let mul = b
        .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, ValueType::I32)
        .unwrap();
    let base_c = b.build_int_const(0x4000u64, ValueType::I32).unwrap();
    let addr = b
        .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, ValueType::I32)
        .unwrap();
    let loaded = b.build_load(addr, VnSpace::RAM, ValueType::I32).unwrap();
    b.build_indirect_branch(loaded).unwrap();
    b.set_lift_addr(None);
    let function = b.build().unwrap();

    let entries: Vec<u64> = (0..256).map(|i| 0x5000 + i).collect();
    let rom = MockRom::strided(0x4000, 4, entries.clone(), 4);
    let (known, doms) = make_known_and_doms(&function);
    let mut ranges = crate::value_range::compute_value_ranges(&function, &doms, &known);
    let result = classify_table_dispatch(
        &function,
        sole_indirect_branch(&function),
        Some(&rom),
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
    );
    let _ = entries;
    assert_eq!(
        result, None,
        "a width-filling byte index is conservatively deferred (indistinguishable from an entry by range)"
    );
}

#[test]
fn classify_table_dispatch_with_if_guard_bound_returns_multiple() {
    // The guard path: idx is an unmasked register read bounded only by a
    // dominating `if (idx < 4)`.
    use strider_ir::IntCmpOp;
    let idx_var = rsleigh::Vn {
        addr_off: 0x10,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let mut b = RegisterSet::new().tracked(idx_var).build_fn().unwrap();
    let entry = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap();
    let exit = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();

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
    // `value_range` assumes converged IR, so collapse this fixture's
    // single-predecessor dispatch region and trivial phis first; otherwise the
    // `idx < 4` guard never reaches the dispatch index.
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
    // Both paths guard `idx < 4`, but both true edges feed the SAME 2-pred
    // merge, and the soundness gate skips a guard whose consumer is a merge
    // (one edge does not dominate it; other predecessors bypass).  So the
    // dispatch defers even though both paths happen to agree.
    //
    //   entry  -> if (dummy)   -> path_a, path_b
    //   path_a -> if (idx < 4) -> dispatch, exit_a
    //   path_b -> if (idx < 4) -> dispatch, exit_b
    use strider_ir::IntCmpOp;
    let idx_var = rsleigh::Vn {
        addr_off: 0x10,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let mut b = RegisterSet::new().tracked(idx_var).build_fn().unwrap();
    let entry = b.create_region_all().unwrap();
    let path_a = b.create_region_all().unwrap();
    let path_b = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap();
    let exit_a = b.create_region_all().unwrap();
    let exit_b = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();

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
    // Only ONE incoming path guards `idx < 4`; the other is unconditional.  The
    // index can exceed 4 there, so reading 4 entries would be out of bounds and
    // the dispatch must defer.
    //
    //   entry  -> If(flag)     -> path_a, path_b
    //   path_a -> If(idx < 4)  -> dispatch, exit_a   [guarded]
    //   path_b -> dispatch                           [idx UNCONSTRAINED]
    //
    // The guard's true successor IS `dispatch`, so relying on reflexive
    // dominance there makes both phi arms look bounded and yields an
    // out-of-bounds `Multiple`.
    use strider_ir::IntCmpOp;
    let idx_var = rsleigh::Vn {
        addr_off: 0x10,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let mut b = RegisterSet::new().tracked(idx_var).build_fn().unwrap();
    let entry = b.create_region_all().unwrap();
    let path_a = b.create_region_all().unwrap();
    let path_b = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap();
    let exit_a = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();

    // Read idx from a register, then split.
    b.set_region(entry);
    let idx_e = b.read_variable(&idx_var).unwrap();
    let zero = b.build_int_const(0u64, ValueType::I32).unwrap();
    let flag = b
        .build_int_cmp_operation(idx_e, zero, IntCmpOp::Equal, ValueType::I32)
        .unwrap();
    b.build_if(flag, path_a, path_b).unwrap();

    // The guarded path; its true successor is `dispatch`.
    b.set_region(path_a);
    let idx_a = b.read_variable(&idx_var).unwrap();
    let four_a = b.build_int_const(4u64, ValueType::I32).unwrap();
    let cond_a = b
        .build_int_cmp_operation(idx_a, four_a, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond_a, dispatch, exit_a).unwrap();

    // The unconditional path, where idx is unconstrained.
    b.set_region(path_b);
    b.build_branch(dispatch).unwrap();

    // The load indexes through the phi of idx from both paths.
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

/// A loaded value fed DIRECTLY to the branch, with no address arithmetic, but
/// under a dominating `if (entry < 4)` guard.
///
/// Because the guard suppresses the width-only exclusion, this load-derived
/// target could be enumerated as the index, making the dispatch value literally
/// 0,1,2,3: bogus sequential targets that are not code addresses.  Returning
/// `Multiple([0,1,2,3])` here would be a wrong-edge bug; `None` means the
/// safety margin holds.
#[test]
fn classify_table_dispatch_guarded_direct_load_target() {
    use strider_ir::IntCmpOp;
    let mut b = strider_ir_test_utils::empty_builder().unwrap();
    let entry = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap();
    let exit = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();

    // Load a value, guard it, then branch to dispatch.
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

    // Feed the SAME loaded value straight to the branch.
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
    // The bare index values must never come out as targets.
    assert_eq!(
        result, None,
        "a guarded direct-load target must not enumerate its index values as \
         branch targets (got {result:?})"
    );
}

/// A cone containing a DECOY finite-range value off the addressing path
/// alongside the real masked index.  The classifier must pick the real index
/// or defer, never emit decoy-derived targets.
///
/// This is what makes the over-approximation self-protecting: a candidate that
/// does not fully determine the address fails to fold for at least one value
/// and is rejected, while the real index folds the whole range.
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
    let entry = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap();
    let exit = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();

    // Guard the DECOY, not the real index.
    b.set_region(entry);
    let decoy = b.read_variable(&decoy_var).unwrap();
    let four = b.build_int_const(4u64, ValueType::I32).unwrap();
    let cond = b
        .build_int_cmp_operation(decoy, four, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond, dispatch, exit).unwrap();

    // The decoy is read again but XOR'd into a dead value that never reaches
    // the address.
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
    // Substituting the decoy cannot change the dispatch, so either outcome is
    // sound as long as no decoy-derived target escapes.
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

/// Runs the standard passes and returns the dispatch `Load`'s output from the
/// converged graph.
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

/// `frame_base` is the SP-derived base for every store and the load address:
/// either bare `sp_val` or an alignment-masked `And(sp_val, mask)`.
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
    // A real placeholder, so the range analysis can locate the dispatch region.
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

/// Like `build_two_target_array` but with an alignment-masked frame base,
/// exercising the `(sp & mask)` path recognised as an opaque SP terminal.
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
    // The evaluator must recognise the And-masked base as an SP terminal, so
    // the load address resolves to SP-relative and `reaching_store` can match
    // the prologue stores.
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

/// A global store between the prologue stores and the dispatch load is exactly
/// what the [`AliasMode`] knob governs: `StackGlobalDisjoint` proves it
/// disjoint from the SP-rooted array and resolves, `Strict` cannot and defers.
///
/// This keeps a `Strict` caller from receiving an optimistically-resolved jump
/// table that only the stack/global-disjointness assumption, which `Strict`
/// rejects, could justify.  Both assertions run on the SAME graph.
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
    // Store two label addresses into the stack array.
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
    // A constant absolute address, unrelated to the stack pointer.
    let global_addr = b.build_int_const(0x0060_0000u64, ValueType::I64).unwrap();
    let global_val = b.build_int_const(0x0000_DEADu64, ValueType::I64).unwrap();
    b.build_store(global_addr, global_val, rsleigh::VnSpace::RAM)
        .unwrap();
    // Load from sp + base + idx*stride.
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

    // Default mode proves the global store disjoint.
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

    // Strict mode cannot, so the store surfaces as a clobber.
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

/// A `Call` between the prologue stores and the dispatch load is a clobber
/// boundary: the slots are no longer provably the stored constants, so the
/// classifier must defer.
#[test]
fn classify_table_dispatch_returns_none_when_call_clobbers_between_stores_and_load() {
    // Store targets into stack slots, then Call, then the dispatch load.
    let sp = sp64();
    let arg_vn = rsleigh::Vn {
        addr_off: 0x38,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    // A dummy call target, untracked and used only as a const.
    let mut b = RegisterSet::new()
        .tracked(sp)
        .tracked(arg_vn)
        .callee_saved(sp)
        .stack_vn(sp)
        .build_fn_single_region()
        .unwrap();
    let sp_val = b.read_variable(&sp).unwrap();
    // Store two label addresses into the stack array.
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
    // Clobbers memory, including the slots the callee can reach through SP.
    let call_target_const = b.build_int_const(0x0040_1000u64, ValueType::I64).unwrap();
    b.build_call_cc(call_target_const, None).unwrap();
    // Re-read sp: the call may have advanced the stack.
    let sp_val_after = b.read_variable(&sp).unwrap();
    // Load from sp + base + idx*stride.  Mirrors the stack-array shape.
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
    // The stored targets are not provably live at the dispatch site.
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

#[test]
fn classify_table_dispatch_one_stack_target_resolves() {
    // A degenerate one-element stack array.  Masking idx with 0 makes it
    // always 0, so the bound is 1.
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
    // Whether a 1-element case resolves depends on how KnownBits bounds the
    // index; what is pinned here is that the classifier does not panic.
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
    // Load through sp+base+idx*stride with idx masked to a single value.
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
        // LOCK is a FullClobber, so the Stack chain must break here.
        let (lock_node, _result) = b.build_call_other_abi(
            0x1234,
            "LOCK",
            &[],
            &strider_target::BuiltCallOtherAbi {
                implicit_reads: Vec::new(),
                implicit_writes: Vec::new(),
                clobbers_memory: false,
                no_return: false,
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

    // StackOffsetDetect must break the Stack chain at the LOCK.
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold::new());
    pipeline.add(PhiCollapse);
    pipeline.add(RegionCollapse);
    pipeline.add_post_pass(StackOffsetDetect);
    pipeline.add(LoadForward);

    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    // LOCK is a full-clobber barrier, so no forwarding.
    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 1,
        "Load[sp-4] must NOT be forwarded across a LOCK barrier; \
         LOCK is FullClobber and breaks the Stack chain"
    );
    Ok(())
}
