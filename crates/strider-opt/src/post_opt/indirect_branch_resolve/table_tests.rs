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
    // KnownBits bounds the `& 0x7` index to 8 entries.
    let (g, _target) = build_with_target(|fb| {
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
        None,
    );
    match result {
        Some(ResolvedTargets::Multiple(ts)) => {
            assert_eq!(
                ts.iter().map(|t| t.addr).collect::<Vec<_>>(),
                vec![0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80]
            );
        }
        other => panic!("expected Multiple([0x10..0x80]); got {other:?}"),
    }
}

#[test]
fn classify_table_dispatch_duplicate_targets_are_deduped() {
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
    let rom = MockRom::strided(0x4000, 4, vec![0x10, 0x20, 0x10, 0x20], 4);
    let (known, doms) = make_known_and_doms(&g);
    let mut ranges = crate::value_range::compute_value_ranges(&g, &doms, &known);
    let result = classify_table_dispatch(
        &g,
        sole_indirect_branch(&g),
        Some(&rom),
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
        None,
    );
    match result {
        Some(ResolvedTargets::Multiple(ts)) => {
            assert_eq!(
                ts.iter().map(|t| t.addr).collect::<Vec<_>>(),
                vec![0x10, 0x20],
                "four indices producing [0x10,0x20,0x10,0x20] dedup to two targets"
            );
        }
        other => panic!("expected Multiple([0x10, 0x20]); got {other:?}"),
    }
}

#[test]
fn classify_table_dispatch_single_entry_bound_returns_multiple_of_one() {
    // Masking with 0 proves idx is always 0, so exactly one entry is read.
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
        None,
    );
    match result {
        Some(ResolvedTargets::Multiple(ts)) => {
            assert_eq!(
                ts.iter().map(|t| t.addr).collect::<Vec<_>>(),
                vec![0x10],
                "bound 1 reads exactly the first entry"
            );
        }
        other => panic!("expected Multiple([0x10]); got {other:?}"),
    }
}

#[test]
fn classify_table_dispatch_no_rom_returns_none() {
    // A bounded shape, but with no rom the entries cannot be read.
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
        None,
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
        None,
    );
    assert_eq!(result, None);
}

/// The entry-count cap: the SAME masked shape resolves at 4 entries and defers
/// at 8192 (`MAX_TABLE_ENTRIES` is 4096).  Both masked indices pass the
/// `hi < type_mask` filter, so the cap is the only variable.
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
        None,
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
        None,
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
    // width-only filter rejects it.  Enumerating it would read 256 cells of
    // table DATA and emit them as targets.
    let (g, _target) = build_with_target(|fb| {
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
        None,
    );
    assert_eq!(
        result, None,
        "a width-bounded load-derived table entry must be excluded as the index"
    );
}

#[test]
fn classify_table_dispatch_resolves_guarded_shift_narrowed_loaded_index() {
    // x86 instruction-decoder shape: `loaded_byte >> 5`, the top 3 bits, under
    // `if (field < 6)`.  The shift alone is a scaling of the raw cell; the GUARD
    // is what narrows it below the cell's 8-value image and makes it a real
    // index.
    use strider_ir::IntCmpOp;
    let mut b = strider_ir_test_utils::empty_builder().expect("empty_builder");
    let entry = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap();
    let exit = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));

    b.set_region(entry);
    let byte_addr = b.build_int_const(0x9000u64, ValueType::I32).unwrap();
    let byte = b
        .build_load(byte_addr, VnSpace::RAM, ValueType::I8)
        .expect("byte load");
    let bwide = b
        .extend_if_needed(byte, ValueType::I32, ExtendOp::ZeroExtend)
        .expect("zext byte");
    let five = b.build_int_const(5u64, ValueType::I32).unwrap();
    let idx = b
        .build_int_binary_operation(bwide, five, IntBinaryOp::ShiftRight, ValueType::I32)
        .expect("byte >> 5 → [0,7]");
    let bound_c = b.build_int_const(6u64, ValueType::I32).unwrap();
    let cond = b
        .build_int_cmp_operation(idx, bound_c, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond, dispatch, exit).unwrap();

    b.set_region(dispatch);
    let stride_c = b.build_int_const(4u64, ValueType::I32).unwrap();
    let mul = b
        .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, ValueType::I32)
        .expect("mul");
    let base_c = b.build_int_const(0x4000u64, ValueType::I32).unwrap();
    let addr = b
        .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, ValueType::I32)
        .expect("add");
    let loaded = b
        .build_load(addr, VnSpace::RAM, ValueType::I32)
        .expect("dispatch load");
    b.build_indirect_branch(loaded).unwrap();

    b.set_region(exit);
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);
    let mut g = b.build().unwrap();
    // `value_range` assumes converged IR.
    {
        let mut p = crate::OptimizerPipeline::new();
        p.add(crate::PhiCollapse);
        p.add(crate::RegionCollapse);
        p.run(&mut g, &mut crate::OptCtx::new(None)).unwrap();
    }

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
        None,
    );
    match result {
        Some(ResolvedTargets::Multiple(ts)) => {
            assert_eq!(
                ts.iter().map(|t| t.addr).collect::<Vec<_>>(),
                vec![0x10, 0x20, 0x30, 0x40, 0x50, 0x60]
            );
        }
        other => panic!("a guarded shift-narrowed index must resolve; got {other:?}"),
    }
}

#[test]
fn classify_table_dispatch_masked_full_byte_i32_resolves() {
    // Adversarial pair to the width-only exclusion: this ALSO spans [0,255],
    // but it is `x & 0xFF` typed I32 with no byte-typed producer to strip to,
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
        None,
    );
    match result {
        Some(ResolvedTargets::Multiple(ts)) => {
            assert_eq!(ts.iter().map(|t| t.addr).collect::<Vec<_>>(), entries)
        }
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
    // A 256-case switch on a byte REGISTER is a real index, but its [0,255]
    // fills the byte width and is indistinguishable by range from a loaded byte
    // entry, so it defers.  Real byte indices arrive
    // through a load/mask/shift that narrows below the width.
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
        None,
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
        None,
    );
    match result {
        Some(ResolvedTargets::Multiple(ts)) => {
            assert_eq!(
                ts.iter().map(|t| t.addr).collect::<Vec<_>>(),
                vec![0x10, 0x20, 0x30, 0x40]
            );
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
        None,
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

    b.set_region(entry);
    let idx_e = b.read_variable(&idx_var).unwrap();
    let zero = b.build_int_const(0u64, ValueType::I32).unwrap();
    let flag = b
        .build_int_cmp_operation(idx_e, zero, IntCmpOp::Equal, ValueType::I32)
        .unwrap();
    b.build_if(flag, path_a, path_b).unwrap();

    b.set_region(path_a);
    let idx_a = b.read_variable(&idx_var).unwrap();
    let four_a = b.build_int_const(4u64, ValueType::I32).unwrap();
    let cond_a = b
        .build_int_cmp_operation(idx_a, four_a, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond_a, dispatch, exit_a).unwrap();

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
        None,
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

    // The SAME loaded value goes straight to the branch.
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
        None,
    );
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
/// A candidate that does not fully determine the address fails to fold for at
/// least one value and is rejected.
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
        None,
    );
    // Substituting the decoy cannot change the dispatch, so either outcome is
    // sound as long as no decoy-derived target escapes.
    match result {
        None => { /* fail-closed defer is sound */ }
        Some(ResolvedTargets::Multiple(ts)) => {
            assert_eq!(
                ts.iter().map(|t| t.addr).collect::<Vec<_>>(),
                vec![0x10, 0x20, 0x30, 0x40],
                "only the real table targets may be emitted; no decoy-derived edges"
            );
        }
        other => panic!("unexpected: {other:?}"),
    }
}

fn finish_stack_array(mut fg: strider_ir::Function) -> (strider_ir::Function, ValueId) {
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
        .expect("Load survives; LoadForward is out of this pipeline");
    let load_value = fg.node_outputs_exact::<1>(load).unwrap()[0];
    (fg, load_value)
}

/// `frame_base` is the SP-derived base for every store and the load address:
/// either bare `sp_val` or an alignment-masked `And(sp_val, mask)`.
///
/// `targets.len()` must be a power of two: the index is masked with
/// `len - 1`, which is what bounds it to the table.
fn wire_stack_array(
    targets: &[u64],
    base_offset: i64,
    stride: u64,
    frame_base: ValueId,
    b: &mut strider_ir::FunctionBuilder,
    arg_vn: rsleigh::Vn,
) {
    wire_stack_stores(targets, base_offset, stride, frame_base, b);
    wire_stack_dispatch(targets.len(), base_offset, stride, frame_base, b, arg_vn);
}

fn wire_stack_stores(
    targets: &[u64],
    base_offset: i64,
    stride: u64,
    frame_base: ValueId,
    b: &mut strider_ir::FunctionBuilder,
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
}

/// `branch *(frame_base + base_offset + (arg & (entries - 1)) * stride)`,
/// terminating the current region.
fn wire_stack_dispatch(
    entries: usize,
    base_offset: i64,
    stride: u64,
    frame_base: ValueId,
    b: &mut strider_ir::FunctionBuilder,
    arg_vn: rsleigh::Vn,
) {
    let arg_val = b.read_variable(&arg_vn).unwrap();
    let arg_u32 = strider_ir_test_utils::sentinel_node(
        b.function_mut(),
        NodeKind::Truncate,
        [arg_val],
        [strider_ir::node::ValueKind::Typed(ValueType::I32)],
    );
    let arg_u32_value = b.function().node_outputs_exact::<1>(arg_u32).unwrap()[0];
    let mask = b
        .build_int_const(entries as u64 - 1, ValueType::I32)
        .unwrap();
    let masked = b
        .build_int_binary_operation(arg_u32_value, mask, IntBinaryOp::And, ValueType::I32)
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
    b.build_indirect_branch(loaded).unwrap();
    b.set_lift_addr(None);
}

fn build_stack_array(
    targets: &[u64],
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
    wire_stack_array(targets, base_offset, stride, sp_val, &mut b, arg_vn);
    let fg = b.build().unwrap();
    finish_stack_array(fg)
}

/// Like `build_stack_array` but with an alignment-masked frame base, which
/// `decompose` treats as a fresh opaque stack base.
fn build_stack_array_aligned(
    targets: &[u64],
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
    wire_stack_array(targets, base_offset, stride, frame_base, &mut b, arg_vn);
    let fg = b.build().unwrap();
    finish_stack_array(fg)
}

#[test]
fn classify_table_dispatch_two_stack_targets_resolves() {
    let targets = [0x401190u64, 0x401180u64];
    let (fg, _load_value) = build_stack_array(&targets, -24, 8);
    let (known, doms) = make_known_and_doms(&fg);
    let mut ranges = crate::value_range::compute_value_ranges(&fg, &doms, &known);
    let result = classify_table_dispatch(
        &fg,
        sole_indirect_branch(&fg),
        None,
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
        None,
    );
    let mut expected = targets.to_vec();
    expected.sort_unstable();
    assert_eq!(
        result,
        Some(ResolvedTargets::Multiple(
            expected.into_iter().map(Into::into).collect()
        ))
    );
}

/// A stack label array whose dispatch load sits below a `MemPhi`: at a join the
/// reaching store is path-dependent, so the straight-line segment ends at the
/// phi and the map has to descend into its arms.
fn build_stack_array_across_mem_phi(
    targets: &[u64],
    base_offset: i64,
    stride: u64,
) -> (strider_ir::Function, ValueId) {
    use strider_ir::IntCmpOp;
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
        .build_fn()
        .unwrap();
    let entry = b.create_region_all().unwrap();
    let arm = b.create_region_all().unwrap();
    let join = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let sp_entry = b.read_variable(&sp).unwrap();
    wire_stack_stores(targets, base_offset, stride, sp_entry, &mut b);
    let arg_entry = b.read_variable(&arg_vn).unwrap();
    let zero = b.build_int_const(0u64, ValueType::I64).unwrap();
    let flag = b
        .build_int_cmp_operation(arg_entry, zero, IntCmpOp::Equal, ValueType::I64)
        .unwrap();
    b.build_if(flag, arm, join).unwrap();

    // One arm writes the slot just below the array, so the arms disagree and
    // the `MemPhi` survives while both still reach the label stores.
    b.set_region(arm);
    let sp_arm = b.read_variable(&sp).unwrap();
    let spill_off = b
        .build_int_const((base_offset - 8) as u64, ValueType::I64)
        .unwrap();
    let spill_addr = b
        .build_int_binary_operation(sp_arm, spill_off, IntBinaryOp::Add, ValueType::I64)
        .unwrap();
    let junk = b.build_int_const(0xDEADu64, ValueType::I64).unwrap();
    b.build_store(spill_addr, junk, rsleigh::VnSpace::RAM)
        .unwrap();
    b.build_branch(join).unwrap();

    b.set_region(join);
    let sp_join = b.read_variable(&sp).unwrap();
    wire_stack_dispatch(targets.len(), base_offset, stride, sp_join, &mut b, arg_vn);
    b.set_lift_addr(None);

    let (fg, load_value) = finish_stack_array(b.build().unwrap());
    assert!(
        mem_chain_crosses_phi(&fg, fg.producer(load_value)),
        "the dispatch load must sit below a MemPhi for this to test the fallback"
    );
    (fg, load_value)
}

/// The resolved set must not move when the map has to cross the join.
#[test]
fn classify_table_dispatch_across_mem_phi_resolves() {
    let targets = [0x401190u64, 0x401180u64];
    let (fg, _load_value) = build_stack_array_across_mem_phi(&targets, -24, 8);
    let (known, doms) = make_known_and_doms(&fg);
    let mut ranges = crate::value_range::compute_value_ranges(&fg, &doms, &known);
    let result = classify_table_dispatch(
        &fg,
        sole_indirect_branch(&fg),
        None,
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
        None,
    );
    let mut expected = targets.to_vec();
    expected.sort_unstable();
    assert_eq!(
        result,
        Some(ResolvedTargets::Multiple(
            expected.into_iter().map(Into::into).collect()
        ))
    );
}

fn mem_chain_crosses_phi(fg: &Function, load: NodeId) -> bool {
    let mut cur = fg.memory_input_of(load);
    while let Some(mem) = cur {
        let node = fg.producer(mem);
        match fg.node_kind(node) {
            NodeKind::MemPhi => return true,
            NodeKind::InitialMemory => return false,
            _ => cur = fg.memory_input_of(node),
        }
    }
    false
}

#[test]
fn classify_table_dispatch_aligned_stack_resolves() {
    // The evaluator must recognise the And-masked base as an SP terminal, so
    // the load address resolves to SP-relative and `reaching_store` can match
    // the prologue stores.
    let targets = [0x401190u64, 0x401180u64];
    let (fg, _load_value) = build_stack_array_aligned(&targets, -24, 8);
    let (known, doms) = make_known_and_doms(&fg);
    let mut ranges = crate::value_range::compute_value_ranges(&fg, &doms, &known);
    let result = classify_table_dispatch(
        &fg,
        sole_indirect_branch(&fg),
        None,
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
        None,
    );
    let mut expected = targets.to_vec();
    expected.sort_unstable();
    assert_eq!(
        result,
        Some(ResolvedTargets::Multiple(
            expected.into_iter().map(Into::into).collect()
        ))
    );
}

/// A global store between the prologue stores and the dispatch load is exactly
/// what the [`AliasMode`] knob governs: `StackGlobalDisjoint` proves it
/// disjoint from the SP-rooted array and resolves, `Strict` cannot and defers.
///
/// Both assertions run on the SAME graph.
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
        .expect("dispatch Load survives; LoadForward is out of this pipeline");
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
            AliasMode::StackGlobalDisjoint,
            None,
        ),
        Some(ResolvedTargets::Multiple(
            expected.into_iter().map(Into::into).collect()
        )),
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
            AliasMode::Strict,
            None,
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
    assert_eq!(
        classify_table_dispatch(
            &fg,
            sole_indirect_branch(&fg),
            None,
            &mut ranges,
            AliasMode::StackGlobalDisjoint,
            None,
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
            AliasMode::StackGlobalDisjoint,
            None,
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
            AliasMode::StackGlobalDisjoint,
            None,
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
        None,
    );
    // A 1-element table may defer; a resolution must name the single target.
    match result {
        None => { /* defer-via-unresolved is sound */ }
        Some(ResolvedTargets::Multiple(v)) => {
            assert_eq!(
                v.iter().map(|t| t.addr).collect::<Vec<_>>(),
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
        .expect("Load survives; LoadForward is out of this pipeline");
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

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold::new());
    pipeline.add(PhiCollapse);
    pipeline.add(RegionCollapse);
    pipeline.add_post_pass(StackOffsetDetect);
    pipeline.add(LoadForward::default());

    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 1,
        "Load[sp-4] must NOT be forwarded across a LOCK barrier; \
         LOCK is FullClobber and breaks the Stack chain"
    );
    Ok(())
}

/// The interval carrier is a `u128`, so a wider type's "top" is not top and
/// every interval at that type looks narrowed.  Enumerating one would pin an
/// index whose real value set the range never covered.
#[test]
fn index_bound_ok_rejects_types_past_the_u128_carrier() {
    let iv = crate::value_range::Interval::dense(0, 7);
    assert!(index_bound_ok(ValueType::I32, iv));
    assert!(!index_bound_ok(ValueType::I256, iv));
    assert!(!index_bound_ok(ValueType::I512, iv));
}

#[test]
fn classify_table_dispatch_excludes_right_shifted_table_entry_as_index() {
    // As the `tbb` case above, with the entry scaled by `>> 2` before the `* 4`
    // address arithmetic.  A right shift is still a scaling of the raw cell, so
    // [0,63] is the whole cell's image, not a narrowing: table DATA.
    let (g, _target) = build_with_target(|fb| {
        let byte_addr = fb.build_int_const(0x9000u64, ValueType::I32).unwrap();
        let byte = fb
            .build_load(byte_addr, VnSpace::RAM, ValueType::I8)
            .expect("byte load (table entry)");
        let wide = fb
            .extend_if_needed(byte, ValueType::I32, ExtendOp::ZeroExtend)
            .expect("zero-extend the byte to I32");
        let two = fb.build_int_const(2u64, ValueType::I32).unwrap();
        let shifted = fb
            .build_int_binary_operation(wide, two, IntBinaryOp::ShiftRight, ValueType::I32)
            .expect("entry >> 2");
        let stride_c = fb.build_int_const(4u64, ValueType::I32).unwrap();
        let mul = fb
            .build_int_binary_operation(shifted, stride_c, IntBinaryOp::Mul, ValueType::I32)
            .expect("mul");
        let base_c = fb.build_int_const(0x4000u64, ValueType::I32).unwrap();
        let addr = fb
            .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, ValueType::I32)
            .expect("add");
        fb.build_load(addr, VnSpace::RAM, ValueType::I32)
            .expect("dispatch load")
    });
    // Every one of the 64 reads folds to a DISTINCT address, so a `None` can
    // only come from the width-only exclusion.
    let rom = MockRom::strided(0x4000, 4, (0..64).map(|i| 0x5000 + i).collect(), 4);
    let (known, doms) = make_known_and_doms(&g);
    let mut ranges = crate::value_range::compute_value_ranges(&g, &doms, &known);
    let result = classify_table_dispatch(
        &g,
        sole_indirect_branch(&g),
        Some(&rom),
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
        None,
    );
    assert_eq!(
        result, None,
        "a right-shifted table entry must be excluded as the index"
    );
}

#[test]
fn classify_table_dispatch_excludes_widened_then_shifted_table_entry_as_index() {
    // `(entry << 2) >> 1` is `entry * 2`: the widening runs BELOW the lossy
    // shift, so the shift collapses nothing and all 256 cell values survive.
    // Counting only the lossy factor expects 128 and reads the mismatch as a
    // narrowing, enumerating table DATA as the index.
    let (g, _target) = build_with_target(|fb| {
        let byte_addr = fb.build_int_const(0x9000u64, ValueType::I32).unwrap();
        let byte = fb
            .build_load(byte_addr, VnSpace::RAM, ValueType::I8)
            .expect("byte load (table entry)");
        let wide = fb
            .extend_if_needed(byte, ValueType::I32, ExtendOp::ZeroExtend)
            .expect("zero-extend the byte to I32");
        let two = fb.build_int_const(2u64, ValueType::I32).unwrap();
        let up = fb
            .build_int_binary_operation(wide, two, IntBinaryOp::ShiftLeft, ValueType::I32)
            .expect("entry << 2");
        let one = fb.build_int_const(1u64, ValueType::I32).unwrap();
        let shifted = fb
            .build_int_binary_operation(up, one, IntBinaryOp::ShiftRight, ValueType::I32)
            .expect("(entry << 2) >> 1");
        let stride_c = fb.build_int_const(4u64, ValueType::I32).unwrap();
        let mul = fb
            .build_int_binary_operation(shifted, stride_c, IntBinaryOp::Mul, ValueType::I32)
            .expect("mul");
        let base_c = fb.build_int_const(0x4000u64, ValueType::I32).unwrap();
        let addr = fb
            .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, ValueType::I32)
            .expect("add");
        fb.build_load(addr, VnSpace::RAM, ValueType::I32)
            .expect("dispatch load")
    });
    // Covers every address the enumeration would touch, so a `None` can only
    // come from the width-only exclusion, not a fold failure.
    let rom = MockRom::strided(0x4000, 4, (0..512).map(|i| 0x5000 + i).collect(), 4);
    let (known, doms) = make_known_and_doms(&g);
    let mut ranges = crate::value_range::compute_value_ranges(&g, &doms, &known);
    let result = classify_table_dispatch(
        &g,
        sole_indirect_branch(&g),
        Some(&rom),
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
        None,
    );
    assert_eq!(
        result, None,
        "a widened-then-shifted table entry must be excluded as the index"
    );
}

#[test]
fn classify_table_dispatch_conflicting_isa_modes_on_one_addr_defers() {
    // An interworking table whose two words are `X` and `X | 1`: the target is
    // the mode-bit-masked word, so both entries land on ONE address carrying
    // opposite committed modes.  Keeping either would decode the arm in a mode
    // half the table contradicts, and no mode at all is the FLOWING mode, which
    // a mode-switching branch contradicts by construction.
    let mut mode_value = None;
    let (g, _target) = build_with_target(|fb| {
        let raw = build_non_const_idx(fb);
        let one = fb.build_int_const(1u64, ValueType::I32).unwrap();
        let idx = fb
            .build_int_binary_operation(raw, one, IntBinaryOp::And, ValueType::I32)
            .expect("idx = raw & 1 → [0,1]");
        let stride_c = fb.build_int_const(4u64, ValueType::I32).unwrap();
        let mul = fb
            .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, ValueType::I32)
            .expect("mul");
        let base_c = fb.build_int_const(0x4000u64, ValueType::I32).unwrap();
        let addr = fb
            .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, ValueType::I32)
            .expect("add");
        let word = fb
            .build_load(addr, VnSpace::RAM, ValueType::I32)
            .expect("dispatch load");
        let mode_mask = fb.build_int_const(1u64, ValueType::I32).unwrap();
        mode_value = Some(
            fb.build_int_binary_operation(word, mode_mask, IntBinaryOp::And, ValueType::I32)
                .expect("word & 1"),
        );
        let addr_mask = fb.build_int_const(0xFFFF_FFFEu64, ValueType::I32).unwrap();
        fb.build_int_binary_operation(word, addr_mask, IntBinaryOp::And, ValueType::I32)
            .expect("word & ~1")
    });
    let rom = MockRom::strided(0x4000, 4, vec![0x1000, 0x1001], 4);
    let (known, doms) = make_known_and_doms(&g);
    let mut ranges = crate::value_range::compute_value_ranges(&g, &doms, &known);
    let result = classify_table_dispatch(
        &g,
        sole_indirect_branch(&g),
        Some(&rom),
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
        mode_value,
    );
    assert_eq!(
        result, None,
        "conflicting modes on one address must defer the whole site"
    );
}

#[test]
fn classify_table_dispatch_carries_each_arms_own_isa_mode() {
    // An interworking table over words `X` and `Y | 1`: the mode is folded per
    // INDEX off that index's own word, so the two arms land on distinct
    // addresses carrying opposite modes.
    let mut mode_value = None;
    let (g, _target) = build_with_target(|fb| {
        let raw = build_non_const_idx(fb);
        let one = fb.build_int_const(1u64, ValueType::I32).unwrap();
        let idx = fb
            .build_int_binary_operation(raw, one, IntBinaryOp::And, ValueType::I32)
            .expect("idx = raw & 1 → [0,1]");
        let stride_c = fb.build_int_const(4u64, ValueType::I32).unwrap();
        let mul = fb
            .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, ValueType::I32)
            .expect("mul");
        let base_c = fb.build_int_const(0x4000u64, ValueType::I32).unwrap();
        let addr = fb
            .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, ValueType::I32)
            .expect("add");
        let word = fb
            .build_load(addr, VnSpace::RAM, ValueType::I32)
            .expect("dispatch load");
        let mode_mask = fb.build_int_const(1u64, ValueType::I32).unwrap();
        mode_value = Some(
            fb.build_int_binary_operation(word, mode_mask, IntBinaryOp::And, ValueType::I32)
                .expect("word & 1"),
        );
        let addr_mask = fb.build_int_const(0xFFFF_FFFEu64, ValueType::I32).unwrap();
        fb.build_int_binary_operation(word, addr_mask, IntBinaryOp::And, ValueType::I32)
            .expect("word & ~1")
    });
    let rom = MockRom::strided(0x4000, 4, vec![0x1000, 0x2001], 4);
    let (known, doms) = make_known_and_doms(&g);
    let mut ranges = crate::value_range::compute_value_ranges(&g, &doms, &known);
    let result = classify_table_dispatch(
        &g,
        sole_indirect_branch(&g),
        Some(&rom),
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
        mode_value,
    );
    match result {
        Some(ResolvedTargets::Multiple(ts)) => assert_eq!(
            ts.iter().map(|t| (t.addr, t.isa_bit)).collect::<Vec<_>>(),
            vec![(0x1000, Some(false)), (0x2000, Some(true))],
        ),
        other => panic!("expected two arms with their own modes; got {other:?}"),
    }
}

/// A seated `Switch` is re-derived from its retained selector, reporting no
/// per-arm ISA mode: the node carries no mode input to fold.
#[test]
fn classify_pass_rederives_a_seated_switch_from_its_selector() {
    let mut b = strider_ir_test_utils::empty_builder().expect("empty_builder");
    let entry = b.create_region_all().unwrap();
    let a0 = b.create_region_all().unwrap();
    let a1 = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let raw = build_non_const_idx(&mut b);
    let one = b.build_int_const(1u64, ValueType::I32).unwrap();
    let idx = b
        .build_int_binary_operation(raw, one, IntBinaryOp::And, ValueType::I32)
        .unwrap();
    let stride_c = b.build_int_const(4u64, ValueType::I32).unwrap();
    let mul = b
        .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, ValueType::I32)
        .unwrap();
    let base_c = b.build_int_const(0x4000u64, ValueType::I32).unwrap();
    let addr = b
        .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, ValueType::I32)
        .unwrap();
    let selector = b.build_load(addr, VnSpace::RAM, ValueType::I32).unwrap();
    // Seated on only ONE of the two table slots, as a site resolved before the
    // CFG finished growing would be.
    let switch = b.build_switch(selector, &[(a0, 0x1000)]).unwrap();
    b.set_region(a0);
    b.build_return(None, &[]).unwrap();
    b.set_region(a1);
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);
    let mut g = b.build().unwrap();

    let rom = MockRom::strided(0x4000, 4, vec![0x1000, 0x2000], 4);
    let mut ctx = crate::OptCtx::new(Some(&rom));
    crate::run_post(&super::super::IndirectBranchClassify, &mut g, &mut ctx).expect("classify");

    assert_eq!(
        ctx.indirect_resolutions.get(&switch),
        Some(&Some(ResolvedTargets::Multiple(vec![
            strider_cfg::ResolvedTarget::new(0x1000, None),
            strider_cfg::ResolvedTarget::new(0x2000, None),
        ]))),
        "the seated switch widens to both slots, with no ISA mode"
    );
}

#[test]
fn classify_table_dispatch_evaluates_each_dispatch_load_once_per_index() {
    // The mode root `And(word, 1)` and the target root `And(word, ~1)` share
    // every node below `word`.  One memo per INDEX evaluates the dispatch load
    // once; one per ROOT re-walks it, doubling the folding work (and, on a
    // stack table, the store walk behind it).
    let mut mode_value = None;
    let (g, _target) = build_with_target(|fb| {
        let raw = build_non_const_idx(fb);
        let mask = fb.build_int_const(3u64, ValueType::I32).unwrap();
        let idx = fb
            .build_int_binary_operation(raw, mask, IntBinaryOp::And, ValueType::I32)
            .expect("idx = raw & 3 → [0,3]");
        let stride_c = fb.build_int_const(4u64, ValueType::I32).unwrap();
        let mul = fb
            .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, ValueType::I32)
            .expect("mul");
        let base_c = fb.build_int_const(0x4000u64, ValueType::I32).unwrap();
        let addr = fb
            .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, ValueType::I32)
            .expect("add");
        let word = fb
            .build_load(addr, VnSpace::RAM, ValueType::I32)
            .expect("dispatch load");
        let mode_mask = fb.build_int_const(1u64, ValueType::I32).unwrap();
        mode_value = Some(
            fb.build_int_binary_operation(word, mode_mask, IntBinaryOp::And, ValueType::I32)
                .expect("word & 1"),
        );
        let addr_mask = fb.build_int_const(0xFFFF_FFFEu64, ValueType::I32).unwrap();
        fb.build_int_binary_operation(word, addr_mask, IntBinaryOp::And, ValueType::I32)
            .expect("word & ~1")
    });
    let rom = RecordingRom {
        inner: MockRom::strided(0x4000, 4, vec![0x1000, 0x1004, 0x1008, 0x100C], 4),
        log: Mutex::new(Vec::new()),
    };
    let (known, doms) = make_known_and_doms(&g);
    let mut ranges = crate::value_range::compute_value_ranges(&g, &doms, &known);
    let result = classify_table_dispatch(
        &g,
        sole_indirect_branch(&g),
        Some(&rom),
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
        mode_value,
    );
    assert!(matches!(result, Some(ResolvedTargets::Multiple(ref ts)) if ts.len() == 4));
    assert_eq!(
        rom.log.lock().unwrap().len(),
        4,
        "one dispatch-load read per index, not one per (index, root)"
    );
}

/// An on-stack label array's memory chain is as long as the table itself, so a
/// per-index memory walk costs O(entries^2).
mod stack_table_cost {
    use super::*;

    /// Targets and the memory-chain steps `classify_table_dispatch` spends on
    /// an `entries`-slot stack array, counting both the per-probe walks and the
    /// slot-map builds that replace them.  Asserts the resolved set, so the cost
    /// measurement can never be met by resolving less.
    fn resolve(entries: usize) -> (Vec<u64>, u64) {
        let targets: Vec<u64> = (0..entries as u64).map(|i| 0x401000 + i * 0x10).collect();
        let (fg, _load) = build_stack_array(&targets, -8 * entries as i64, 8);
        let (known, doms) = make_known_and_doms(&fg);
        let mut ranges = crate::value_range::compute_value_ranges(&fg, &doms, &known);
        crate::mem_analysis::WALK_STEPS.with(|c| c.set(0));
        super::super::super::eval::SLOT_MAP_STEPS.with(|c| c.set(0));
        let result = classify_table_dispatch(
            &fg,
            sole_indirect_branch(&fg),
            None,
            &mut ranges,
            AliasMode::StackGlobalDisjoint,
            None,
        );
        let steps = crate::mem_analysis::WALK_STEPS.with(std::cell::Cell::get)
            + super::super::super::eval::SLOT_MAP_STEPS.with(std::cell::Cell::get);
        let Some(ResolvedTargets::Multiple(ts)) = result else {
            panic!("an {entries}-entry stack array must resolve, got {result:?}");
        };
        let resolved: Vec<u64> = ts.iter().map(|t| t.addr).collect();
        assert_eq!(resolved, targets, "every slot resolves to its own label");
        (resolved, steps)
    }

    /// As [`resolve`], for an array whose dispatch load sits below a `MemPhi`,
    /// where the straight-line segment above the probe is empty.
    fn resolve_across_mem_phi(entries: usize) -> (Vec<u64>, u64) {
        let targets: Vec<u64> = (0..entries as u64).map(|i| 0x401000 + i * 0x10).collect();
        let (fg, _load) = build_stack_array_across_mem_phi(&targets, -8 * entries as i64, 8);
        let (known, doms) = make_known_and_doms(&fg);
        let mut ranges = crate::value_range::compute_value_ranges(&fg, &doms, &known);
        crate::mem_analysis::WALK_STEPS.with(|c| c.set(0));
        super::super::super::eval::SLOT_MAP_STEPS.with(|c| c.set(0));
        let result = classify_table_dispatch(
            &fg,
            sole_indirect_branch(&fg),
            None,
            &mut ranges,
            AliasMode::StackGlobalDisjoint,
            None,
        );
        let steps = crate::mem_analysis::WALK_STEPS.with(std::cell::Cell::get)
            + super::super::super::eval::SLOT_MAP_STEPS.with(std::cell::Cell::get);
        let Some(ResolvedTargets::Multiple(ts)) = result else {
            panic!("an {entries}-entry stack array must resolve, got {result:?}");
        };
        let resolved: Vec<u64> = ts.iter().map(|t| t.addr).collect();
        assert_eq!(resolved, targets, "every slot resolves to its own label");
        (resolved, steps)
    }

    #[test]
    fn is_not_quadratic_in_entries() {
        let (small_targets, small) = resolve(64);
        let (big_targets, big) = resolve(128);
        assert_eq!(small_targets.len(), 64);
        assert_eq!(big_targets.len(), 128);
        assert!(
            big <= small * 3,
            "doubling the entry count must not quadruple the memory walk: \
             64 entries took {small} steps, 128 took {big}"
        );
    }

    /// A `MemPhi` above the probe empties the straight-line segment, so every
    /// probe falls off it. Without a memo behind the phi that is one full chain
    /// walk per entry.
    #[test]
    fn is_not_quadratic_across_a_mem_phi() {
        let (small_targets, small) = resolve_across_mem_phi(64);
        let (big_targets, big) = resolve_across_mem_phi(128);
        assert_eq!(small_targets.len(), 64);
        assert_eq!(big_targets.len(), 128);
        assert!(
            big <= small * 3,
            "doubling the entry count must not quadruple the memory walk: \
             64 entries took {small} steps, 128 took {big}"
        );
    }
}
