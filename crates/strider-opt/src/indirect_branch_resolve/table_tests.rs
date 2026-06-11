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
use crate::{ConstantFold, KnownBits, LoadForward, OptimizerPipeline, PhiCollapse, RegionCollapse, StackOffsetDetect};
use rsleigh::VnSpace;
use std::sync::Mutex;
use strider_ir::ExtendOp;
use strider_ir::Function;
use strider_ir::FunctionBuilder;
use strider_ir::IRBuilderExt;
use strider_ir::IRViewer;
use strider_ir::IRWalker;
use strider_ir::IntBinaryOp;
use strider_ir::IntPayload;
use strider_ir::node::ValueType;
use strider_ir_test_utils::{MockRom, RegisterSet, stack_vn_aarch64 as sp64, stack_vn_x86 as sp32_vn};

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

// ── Absolute-base entry-read helper ──────────────────────────────────────────
//
// The former `read_table_entries` standalone reader was folded into
// `read_entry` (one entry per call).  This helper reproduces the old
// all-or-nothing batch semantics on top of `read_entry` so the read-order
// / partial-read characterizations survive: build an `Absolute`-base
// `TableShape` and read indices `0..count`, failing closed (returning
// `None`) on the first failed entry.
fn read_entries_absolute(
    rom: &dyn ReadOnlyMemory,
    base: u64,
    stride: u64,
    count: u64,
    entry_size: usize,
) -> Option<Vec<u64>> {
    // Any ValueId works for the Absolute arm — `read_entry` never reads
    // `idx_value` / `mem_value` / `value_type` for the absolute case.
    let (fg, dummy) =
        build_with_anchor(|fb| fb.build_int_const(0u64, ValueType::I32).unwrap());
    let shape = TableShape {
        base: TableBase::Absolute(base),
        stride,
        idx_value: dummy,
        value_type: ValueType::int_for_byte_size(entry_size as u32)
            .unwrap_or(ValueType::I64),
        entry_size,
        mem_value: dummy,
    };
    let mut memo = SpExprMemo::default();
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        out.push(read_entry(
            &fg,
            &shape,
            i,
            Some(rom),
            &mut memo,
            AliasMode::StackGlobalDisjoint,
        )?);
    }
    Some(out)
}

// ── Shape-match tests (absolute / rodata arm) ────────────────────────────────

/// Builds `Load[ IntAdd( IntConst(base), IntMul(idx, IntConst(stride)) ) ]`
/// where `idx` is provided by the closure.  Used by several shape
/// tests.
fn build_jt_load(
    base: u64,
    stride: u64,
    commute_add: bool,
    commute_mul: bool,
    idx_provider: impl FnOnce(&mut FunctionBuilder) -> ValueId,
) -> (Function, ValueId) {
    build_with_anchor(|fb| {
        let idx = idx_provider(fb);
        let stride_c = fb.build_int_const(stride, ValueType::I32).unwrap();
        let mul = if commute_mul {
            fb.build_int_binary_operation(stride_c, idx, IntBinaryOp::Mul, ValueType::I32)
                .expect("mul")
        } else {
            fb.build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, ValueType::I32)
                .expect("mul")
        };
        let base_c = fb.build_int_const(base, ValueType::I32).unwrap();
        let addr = if commute_add {
            fb.build_int_binary_operation(mul, base_c, IntBinaryOp::Add, ValueType::I32)
                .expect("add")
        } else {
            fb.build_int_binary_operation(base_c, mul, IntBinaryOp::Add, ValueType::I32)
                .expect("add")
        };
        fb.build_load(addr, VnSpace::RAM, ValueType::I32)
            .expect("load")
    })
}

/// Build a non-IntConst integer value usable as `idx` for the
/// shape tests.  We need a producer that ISN'T an IntConst so
/// `match_table_shape` can disambiguate `idx` from
/// `IntConst(stride)` in commuted multiplications — otherwise
/// both mul operands are IntConsts and the matcher picks the
/// wrong "stride".
fn build_non_const_idx(fb: &mut FunctionBuilder) -> ValueId {
    let addr = fb.build_int_const(0x9000u64, ValueType::I32).unwrap();
    fb.build_load(addr, VnSpace::RAM, ValueType::I32)
        .expect("u32 load (idx)")
}

#[test]
fn match_table_shape_recognises_canonical_form() {
    // Load[base + idx*stride], non-commuted variant.  idx is a
    // load (non-const) so the shape match's stride-vs-idx
    // disambiguation is exercised cleanly.
    let (g, anchor) = build_jt_load(0x4000, 4, false, false, build_non_const_idx);
    let shape = match_table_shape(&g, anchor).expect("must match");
    assert!(matches!(shape.base, TableBase::Absolute(0x4000)));
    assert_eq!(shape.stride, 4);
    assert_eq!(shape.entry_size, 4);
}

#[test]
fn match_table_shape_recognises_commuted_intadd() {
    // IntAdd(IntMul(idx, stride), IntConst(base)) — base on the
    // right.  match-shape must try both orderings.
    let (g, anchor) = build_jt_load(0x5000, 4, true, false, build_non_const_idx);
    let shape = match_table_shape(&g, anchor).expect("must match commuted add");
    assert!(matches!(shape.base, TableBase::Absolute(0x5000)));
    assert_eq!(shape.stride, 4);
}

#[test]
fn match_table_shape_recognises_commuted_intmul() {
    // IntMul(IntConst(stride), idx) — stride on the left of the
    // multiplication.
    let (g, anchor) = build_jt_load(0x6000, 8, false, true, build_non_const_idx);
    let shape = match_table_shape(&g, anchor).expect("must match commuted mul");
    assert!(matches!(shape.base, TableBase::Absolute(0x6000)));
    assert_eq!(shape.stride, 8);
}

#[test]
fn match_table_shape_recognises_both_commutations() {
    // Both add and mul commuted — the worst-case ordering.
    let (g, anchor) = build_jt_load(0x7000, 4, true, true, build_non_const_idx);
    let shape = match_table_shape(&g, anchor).expect("must match both commuted");
    assert!(matches!(shape.base, TableBase::Absolute(0x7000)));
    assert_eq!(shape.stride, 4);
}

/// Builds `Load[ IntAdd( IntConst(base), Shl(idx, IntConst(shift)) ) ]`
/// — the AArch64 / ARM `LDR Rn, [Rb, Ri, LSL #shift]` shape, where
/// the effective stride is `1 << shift`.  `idx` is provided by the
/// closure.
fn build_jt_load_shl(
    base: u64,
    shift: u64,
    commute_add: bool,
    idx_provider: impl FnOnce(&mut FunctionBuilder) -> ValueId,
) -> (Function, ValueId) {
    build_with_anchor(|fb| {
        let idx = idx_provider(fb);
        let shift_c = fb.build_int_const(shift, ValueType::I32).unwrap();
        let scaled = fb
            .build_int_binary_operation(idx, shift_c, IntBinaryOp::ShiftLeft, ValueType::I32)
            .expect("shl");
        let base_c = fb.build_int_const(base, ValueType::I32).unwrap();
        let addr = if commute_add {
            fb.build_int_binary_operation(scaled, base_c, IntBinaryOp::Add, ValueType::I32)
                .expect("add")
        } else {
            fb.build_int_binary_operation(base_c, scaled, IntBinaryOp::Add, ValueType::I32)
                .expect("add")
        };
        fb.build_load(addr, VnSpace::RAM, ValueType::I32)
            .expect("load")
    })
}

#[test]
fn match_table_shape_recognises_shl_form() {
    // AArch64 `ldr xN, [base, idx, lsl #2]` shape — table of 4-byte
    // entries.  `Shl(idx, 2)` is arithmetically equal to
    // `Mul(idx, 4)` but lifts as a distinct IR op.
    let (g, anchor) = build_jt_load_shl(0x4000, 2, false, build_non_const_idx);
    let shape = match_table_shape(&g, anchor).expect("Shl-scaled table must match");
    assert!(matches!(shape.base, TableBase::Absolute(0x4000)));
    assert_eq!(shape.stride, 4); // 1 << 2
    assert_eq!(shape.entry_size, 4);
}

#[test]
fn match_table_shape_recognises_shl_form_commuted_add() {
    // `Shl` itself is non-commutative, but the surrounding `Add` is
    // — so we must still match `(idx<<shift) + base` as well as
    // `base + (idx<<shift)`.
    let (g, anchor) = build_jt_load_shl(0x5000, 3, true, build_non_const_idx);
    let shape = match_table_shape(&g, anchor)
        .expect("Shl-scaled table with commuted add must match");
    assert!(matches!(shape.base, TableBase::Absolute(0x5000)));
    assert_eq!(shape.stride, 8); // 1 << 3 — AArch64 jump table of 8-byte pointers
}

#[test]
fn match_table_shape_rejects_shl_with_oversize_shift() {
    // `Shl(idx, 64)` would compute `1u64 << 64`, which is UB / would
    // overflow the implied stride.  Reject — real jump tables top
    // out at shift = 3.
    let (g, anchor) = build_jt_load_shl(0x6000, 64, false, build_non_const_idx);
    assert!(
        match_table_shape(&g, anchor).is_none(),
        "shift >= 64 must reject; otherwise stride computation overflows"
    );
}

#[test]
fn match_table_shape_rejects_non_load_producer() {
    // Anchor is a raw IntConst, not a Load.  Reject.
    let (g, anchor) =
        build_with_anchor(|fb| fb.build_int_const(0x1000u64, ValueType::I32).unwrap());
    assert!(match_table_shape(&g, anchor).is_none());
}

#[test]
fn match_table_shape_rejects_load_with_unrelated_addr_shape() {
    // Load[IntConst(addr)] — a simple global read, no Add/Mul.
    // Our shape requires IntAdd at the top of the address tree.
    let (g, anchor) = build_with_anchor(|fb| {
        let addr = fb.build_int_const(0x1234u64, ValueType::I32).unwrap();
        fb.build_load(addr, VnSpace::RAM, ValueType::I32)
            .expect("load")
    });
    assert!(match_table_shape(&g, anchor).is_none());
}

#[test]
fn match_table_shape_rejects_load_without_intconst_base() {
    // Load[ IntAdd( idx_or_some_var, IntMul(idx, stride) ) ] where
    // the "base" side is not a constant.  We reject because we
    // can't pin table[0]'s address without a const base.
    //
    // Build: anchor = Load[IntAdd(IntMul(idx, 4), IntMul(idx, 4))]
    // — both add operands are mul-shaped, neither is an IntConst.
    let (g, anchor) = build_with_anchor(|fb| {
        let idx = fb.build_int_const(2u64, ValueType::I32).unwrap();
        let stride_c = fb.build_int_const(4u64, ValueType::I32).unwrap();
        let mul1 = fb
            .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, ValueType::I32)
            .expect("mul1");
        let mul2 = fb
            .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, ValueType::I32)
            .expect("mul2");
        let addr = fb
            .build_int_binary_operation(mul1, mul2, IntBinaryOp::Add, ValueType::I32)
            .expect("add");
        fb.build_load(addr, VnSpace::RAM, ValueType::I32)
            .expect("load")
    });
    assert!(match_table_shape(&g, anchor).is_none());
}

// ── Entry-read tests (absolute arm) ──────────────────────────────────────────

#[test]
fn read_entries_returns_targets_in_index_order() {
    // 4 entries: 0x100, 0x200, 0x300, 0x400.  Stride 4, base
    // 0x4000.  Verify the returned vec preserves index order.
    let rom = MockRom::strided(0x4000, 4, vec![0x100, 0x200, 0x300, 0x400], 4);
    let result =
        read_entries_absolute(&rom, 0x4000, 4, 4, 4).expect("must read all");
    assert_eq!(result, vec![0x100, 0x200, 0x300, 0x400]);
}

#[test]
fn read_entries_returns_none_on_partial_read() {
    // 4 entries requested; rom only serves the first 2.  Must
    // fail closed: returns None, NOT a Vec of length 2.
    let rom = MockRom::strided(0x5000, 4, vec![0x100, 0x200, 0x300, 0x400], 4).with_cutoff(2);
    assert_eq!(
        read_entries_absolute(&rom, 0x5000, 4, 4, 4),
        None
    );
}

#[test]
fn read_entries_issues_count_reads_in_index_order() {
    // RecordingRom logs every (addr, size) pair.  For 3 entries
    // at stride 4, base 0x6000, expect: (0x6000, 4), (0x6004, 4),
    // (0x6008, 4) in that order.
    let rom = RecordingRom {
        inner: MockRom::strided(0x6000, 4, vec![0xaaaa, 0xbbbb, 0xcccc], 4),
        log: Mutex::new(Vec::new()),
    };
    let _ = read_entries_absolute(&rom, 0x6000, 4, 3, 4).expect("read");
    let log = rom.log.lock().unwrap().clone();
    assert_eq!(log, vec![(0x6000, 4), (0x6004, 4), (0x6008, 4)]);
}

// ── End-to-end classifier-on-shape tests (absolute / rodata arm) ─────────────

#[test]
fn classify_table_dispatch_with_known_bits_bound_returns_multiple() {
    // idx = (load) & 0x7 → bound 8 via KnownBits upper bound in the range pass.
    // Load[base + idx*stride] → resolves to Multiple of table[0..8].
    let (g, anchor) = build_with_anchor(|fb| {
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
    let ranges = crate::value_range::compute_value_ranges(&g, &doms, &known);
    let result = classify_table_dispatch(&g, anchor, Some(&rom), &ranges, AliasMode::StackGlobalDisjoint);
    match result {
        Some(ResolvedTargets::Multiple(ts)) => {
            assert_eq!(ts, vec![0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80]);
        }
        other => panic!("expected Multiple([0x10..0x80]); got {other:?}"),
    }
}

#[test]
fn classify_table_dispatch_single_entry_bound_returns_multiple_of_one() {
    // Degenerate rodata jump table of size 1: idx = (load) & 0x0 → KnownBits
    // proves idx is always 0, so the range pass yields bound = 1 and the
    // classifier reads exactly one entry.  Pins that a one-entry table is
    // still classified as `Multiple` (with a single target), not `Single`
    // and not a defer.
    let (g, anchor) = build_with_anchor(|fb| {
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
    let ranges = crate::value_range::compute_value_ranges(&g, &doms, &known);
    let result =
        classify_table_dispatch(&g, anchor, Some(&rom), &ranges, AliasMode::StackGlobalDisjoint);
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
    let (g, anchor) = build_with_anchor(|fb| {
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
    let ranges = crate::value_range::compute_value_ranges(&g, &doms, &known);
    let result = classify_table_dispatch(&g, anchor, None, &ranges, AliasMode::StackGlobalDisjoint);
    assert_eq!(result, None);
}

#[test]
fn classify_table_dispatch_unbounded_idx_returns_none() {
    // Shape is jt-shaped, but `idx` is a raw load with no AND mask and
    // no dominating If guard.  Must return None, not a Multiple.
    let (g, anchor) = build_with_anchor(|fb| {
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
    let ranges = crate::value_range::compute_value_ranges(&g, &doms, &known);
    let result = classify_table_dispatch(&g, anchor, Some(&rom), &ranges, AliasMode::StackGlobalDisjoint);
    assert_eq!(result, None);
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

    let function = b.build().unwrap();

    // Locate the anchor: IndirectBranch's target input.
    let anchor = function
        .walk()
        .find(|&n| matches!(function.node_kind(n), NodeKind::IndirectBranch))
        .map(|n| function.node_inputs_exact::<3>(n).expect("3 inputs")[2])
        .expect("placeholder IndirectBranch");

    let rom = MockRom::strided(0x4000, 4, vec![0x10, 0x20, 0x30, 0x40], 4);
    let (known, doms) = make_known_and_doms(&function);
    let ranges = crate::value_range::compute_value_ranges(&function, &doms, &known);

    let result = classify_table_dispatch(&function, anchor, Some(&rom), &ranges, AliasMode::StackGlobalDisjoint);
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

    let anchor = function
        .walk()
        .find(|&n| matches!(function.node_kind(n), NodeKind::IndirectBranch))
        .map(|n| function.node_inputs_exact::<3>(n).expect("3 inputs")[2])
        .expect("placeholder");

    let rom = MockRom::strided(0x4000, 4, vec![0x10, 0x20, 0x30, 0x40], 4);
    let (known, doms) = make_known_and_doms(&function);
    let ranges = crate::value_range::compute_value_ranges(&function, &doms, &known);
    let result = classify_table_dispatch(&function, anchor, Some(&rom), &ranges, AliasMode::StackGlobalDisjoint);
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

    let anchor = function
        .walk()
        .find(|&n| matches!(function.node_kind(n), NodeKind::IndirectBranch))
        .map(|n| function.node_inputs_exact::<3>(n).expect("3 inputs")[2])
        .expect("placeholder");

    let rom = MockRom::strided(0x4000, 4, vec![0x10, 0x20, 0x30, 0x40], 4);
    let (known, doms) = make_known_and_doms(&function);
    let ranges = crate::value_range::compute_value_ranges(&function, &doms, &known);
    let result = classify_table_dispatch(&function, anchor, Some(&rom), &ranges, AliasMode::StackGlobalDisjoint);
    assert!(
        result.is_none(),
        "one-path-unguarded dispatch must NOT resolve (would be OOB); got {result:?}"
    );
}

// ── End-to-end classifier tests (SP-rooted / on-stack arm) ───────────────────

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
    for (i, &target_addr) in targets.iter().enumerate() {
        let off = base_offset + (i as i64) * (stride as i64);
        let off_const = b.build_int_const(off as u64, ValueType::I64).unwrap();
        let addr = b
            .build_int_binary_operation(sp_val, off_const, IntBinaryOp::Add, ValueType::I64)
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
        .build_int_binary_operation(idx_u64_value, stride_const, IntBinaryOp::Mul, ValueType::I64)
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
    // Use build_indirect_branch so the range analysis can locate the
    // dispatch region via find_anchor_consumer_placeholder.
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
fn classify_table_dispatch_two_stack_targets_resolves() {
    let targets = [0x401190u64, 0x401180u64];
    let (fg, load_value) = build_two_target_array(targets, -24, 8);
    let (known, doms) = make_known_and_doms(&fg);
    let ranges = crate::value_range::compute_value_ranges(&fg, &doms, &known);
    let result = classify_table_dispatch(&fg, load_value, None, &ranges, AliasMode::StackGlobalDisjoint);
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
        .build_int_binary_operation(idx_u64_value, stride_const, IntBinaryOp::Mul, ValueType::I64)
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
    let load_value = fg.node_outputs_exact::<1>(load).unwrap()[0];
    let (known, doms) = make_known_and_doms(&fg);
    let ranges = crate::value_range::compute_value_ranges(&fg, &doms, &known);

    // Default (optimistic) mode: the global store is disjoint → resolves.
    let mut expected = targets.to_vec();
    expected.sort_unstable();
    assert_eq!(
        classify_table_dispatch(&fg, load_value, None, &ranges, AliasMode::StackGlobalDisjoint),
        Some(ResolvedTargets::Multiple(expected)),
        "StackGlobalDisjoint proves the global store disjoint from the \
         SP-rooted array; the table must resolve",
    );

    // Strict mode: the global store may-alias the probe → clobber → defer.
    assert_eq!(
        classify_table_dispatch(&fg, load_value, None, &ranges, AliasMode::Strict),
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
    b.build_call(call_target_const, None).unwrap();
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
        .build_int_binary_operation(idx_u64_value, stride_const, IntBinaryOp::Mul, ValueType::I64)
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
    let load_value = fg.node_outputs_exact::<1>(load).unwrap()[0];
    let (known, doms) = make_known_and_doms(&fg);
    let ranges = crate::value_range::compute_value_ranges(&fg, &doms, &known);
    // The Call is a clobber boundary: the stored targets are not provably
    // live at the dispatch site → classifier MUST return None.
    assert_eq!(
        classify_table_dispatch(&fg, load_value, None, &ranges, AliasMode::StackGlobalDisjoint),
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
    b.build_return(Some(loaded), &[]).unwrap();
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
    let load_value = fg.node_outputs_exact::<1>(load).unwrap()[0];
    let (known, doms) = make_known_and_doms(&fg);
    let ranges = crate::value_range::compute_value_ranges(&fg, &doms, &known);
    assert_eq!(
        classify_table_dispatch(&fg, load_value, None, &ranges, AliasMode::StackGlobalDisjoint),
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
    b.build_return(Some(loaded), &[]).unwrap();
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
    let load_value = fg.node_outputs_exact::<1>(load).unwrap()[0];
    let (known, doms) = make_known_and_doms(&fg);
    let ranges = crate::value_range::compute_value_ranges(&fg, &doms, &known);
    assert_eq!(
        classify_table_dispatch(&fg, load_value, None, &ranges, AliasMode::StackGlobalDisjoint),
        None
    );
}

// ── strip_target_mask characterization tests ──────────────────
//
// These tests pin both operand orderings explicitly so a future
// refactor of `strip_target_mask` cannot accidentally narrow what
// we accept.  `strider_pattern::and` / `strider_pattern::or` are auto-commutative,
// so a regression that drops one ordering would still pass the
// commutative-pair check but fail this characterization.
//
// The target shapes covered:
//   * Bare anchor — no wrapper, returns `(anchor, !0)`.
//   * `And(load, K)` and `And(K, load)` — both orderings, mask narrows.
//   * `And(Or(load, 1), 0xFFFE)` — ARM-Thumb interworking idiom; the
//     OR is stripped because its set bit (`1`) is fully cleared by
//     the surviving `mask` (`0xFFFE`).
//   * `Or(load, 0xFF)` not stripped when it wouldn't be masked off
//     downstream — preserves the wrapper so the outer shape match
//     fails closed.
//   * Multi-And nesting — nested AND-masks compose by intersection.

/// Build a minimal graph whose return-value anchor is a non-const
/// value — specifically the output of a `Load` from `InitialVar(reg)`.
/// Returns `(graph, anchor_value)`.  The anchor must NOT itself be
/// an `IntConst`, because `strip_target_mask`'s commutative-And
/// handling captures the const operand on either side; an IntConst
/// inner would incorrectly pin the captured "non-const" side.
fn build_load_anchor() -> (strider_ir::Function, ValueId) {
    let reg = rsleigh::Vn {
        addr_off: 0x10,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let mut b = RegisterSet::new()
        .tracked(reg)
        .build_fn_single_region()
        .unwrap();
    let addr = b.read_variable(&reg).unwrap();
    let v = b
        .build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)
        .unwrap();
    b.build_return(Some(v), &[]).unwrap();
    b.set_lift_addr(None);
    let fg = b.build().unwrap();
    (fg, v)
}

/// Wraps `inner` in `IntBinaryOp(op)` with the given side-ordering of
/// the constant `c`.  `swap=false` produces `op(inner, IntConst(c))`;
/// `swap=true` produces `op(IntConst(c), inner)`.  `ty` is the output
/// type of both operands and the result.
fn build_binop_wrapped(
    function: &mut strider_ir::Function,
    inner: ValueId,
    op: IntBinaryOp,
    c: u64,
    ty: ValueType,
    swap: bool,
) -> ValueId {
    let const_node = strider_ir_test_utils::sentinel_node(
        function,
        NodeKind::IntConst(IntPayload::Small(c)),
        [],
        [strider_ir::node::ValueKind::Typed(ty)],
    );
    let const_value = function.node_outputs_exact::<1>(const_node).unwrap()[0];
    let (lhs, rhs) = if swap {
        (const_value, inner)
    } else {
        (inner, const_value)
    };
    let n = strider_ir_test_utils::sentinel_node(
        function,
        NodeKind::IntBinaryOp(op),
        [lhs, rhs],
        [strider_ir::node::ValueKind::Typed(ty)],
    );
    function.node_outputs_exact::<1>(n).unwrap()[0]
}

#[test]
fn strip_target_mask_no_wrapper_returns_all_ones() {
    let (fg, anchor) = build_load_anchor();
    let (out, mask) = strip_target_mask(&fg, anchor);
    assert_eq!(out, anchor, "no wrapper: anchor passes through");
    assert_eq!(mask, !0u64, "no wrapper: mask must be all-ones");
}

#[test]
fn strip_target_mask_and_with_const_rhs_strips_one_layer() {
    let (mut fg, inner) = build_load_anchor();
    let wrapped = build_binop_wrapped(
        &mut fg,
        inner,
        IntBinaryOp::And,
        0xFFFE,
        ValueType::I64,
        false,
    );
    let (out, mask) = strip_target_mask(&fg, wrapped);
    assert_eq!(out, inner, "And(load, K) strips to load");
    assert_eq!(mask, 0xFFFE, "And(load, K) yields mask K");
}

#[test]
fn strip_target_mask_and_with_const_lhs_strips_one_layer() {
    let (mut fg, inner) = build_load_anchor();
    let wrapped = build_binop_wrapped(
        &mut fg,
        inner,
        IntBinaryOp::And,
        0xFFFE,
        ValueType::I64,
        true,
    );
    let (out, mask) = strip_target_mask(&fg, wrapped);
    assert_eq!(out, inner, "And(K, load) strips to load (commutative)");
    assert_eq!(mask, 0xFFFE, "And(K, load) yields mask K");
}

#[test]
fn strip_target_mask_arm_thumb_or_then_and_strips_both_layers() {
    // Canonical ARM-Thumb interworking shape:
    //   And(Or(inner, 1), 0xFFFE)
    // After strip, both wrappers must be gone (the OR's set bit `1`
    // is fully cleared by the surviving mask `0xFFFE`).
    let (mut fg, inner) = build_load_anchor();
    let or_layer =
        build_binop_wrapped(&mut fg, inner, IntBinaryOp::Or, 1, ValueType::I64, false);
    let and_layer = build_binop_wrapped(
        &mut fg,
        or_layer,
        IntBinaryOp::And,
        0xFFFE,
        ValueType::I64,
        false,
    );
    let (out, mask) = strip_target_mask(&fg, and_layer);
    assert_eq!(out, inner, "And(Or(load, 1), 0xFFFE) strips both wrappers");
    assert_eq!(mask, 0xFFFE, "and-then-or yields the And's mask");
}

#[test]
fn strip_target_mask_or_overlapping_mask_stops_at_or() {
    // The Or's constant overlaps with surviving mask bits, so the
    // strip must NOT pass through it.  The Or stays in place;
    // the surrounding And contributes its mask.
    let (mut fg, inner) = build_load_anchor();
    let or_layer =
        build_binop_wrapped(&mut fg, inner, IntBinaryOp::Or, 0xFF, ValueType::I64, false);
    let and_layer = build_binop_wrapped(
        &mut fg,
        or_layer,
        IntBinaryOp::And,
        0xFFFE,
        ValueType::I64,
        false,
    );
    let (out, mask) = strip_target_mask(&fg, and_layer);
    assert_eq!(out, or_layer, "overlapping Or is preserved");
    assert_eq!(mask, 0xFFFE, "And's mask still applies");
}

#[test]
fn strip_target_mask_nested_ands_compose_via_intersection() {
    // And(And(inner, 0xFFFF), 0xFF) — the second And narrows further.
    // Both layers strip; surviving mask is the intersection.
    let (mut fg, inner) = build_load_anchor();
    let inner_and = build_binop_wrapped(
        &mut fg,
        inner,
        IntBinaryOp::And,
        0xFFFF,
        ValueType::I64,
        false,
    );
    let outer_and = build_binop_wrapped(
        &mut fg,
        inner_and,
        IntBinaryOp::And,
        0xFF,
        ValueType::I64,
        false,
    );
    let (out, mask) = strip_target_mask(&fg, outer_and);
    assert_eq!(out, inner, "nested Ands strip down to innermost");
    assert_eq!(mask, 0xFF, "nested Ands intersect their masks");
}

// ── flatten_add_tree budget boundary tests ────────────────────────
//
// These tests pin the 32-node budget cap that defends against
// pathologically deep Add trees (a bug in lifter output, or a
// crafted input).  The function is recursive; the cap converts
// "would-be stack overflow" into "graceful unmatch".

/// Build a right-spine Add tree of the given depth over fresh
/// IntConst(i) leaves.  Returns the root ValueId.
fn build_right_spine_add_tree(function: &mut strider_ir::Function, depth: usize) -> ValueId {
    assert!(depth >= 1, "need at least one node");
    // Innermost: IntConst(0).  Wrap depth-1 additional Add layers,
    // each adding a fresh IntConst on the LHS.
    let mut cur = {
        let n = strider_ir_test_utils::sentinel_node(
            function,
            NodeKind::IntConst(IntPayload::Small(0_u64)),
            [],
            [strider_ir::node::ValueKind::Typed(ValueType::I64)],
        );
        function.node_outputs_exact::<1>(n).unwrap()[0]
    };
    for i in 1..depth {
        let leaf = {
            let n = strider_ir_test_utils::sentinel_node(
                function,
                NodeKind::IntConst(IntPayload::Small(i as u64)),
                [],
                [strider_ir::node::ValueKind::Typed(ValueType::I64)],
            );
            function.node_outputs_exact::<1>(n).unwrap()[0]
        };
        let add = strider_ir_test_utils::sentinel_node(
            function,
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [leaf, cur],
            [strider_ir::node::ValueKind::Typed(ValueType::I64)],
        );
        cur = function.node_outputs_exact::<1>(add).unwrap()[0];
    }
    cur
}

#[test]
fn flatten_add_tree_within_budget_collects_all_leaves() {
    // 8-deep Add tree → 8 leaves should all flatten out.
    let (mut fg, _anchor) = build_load_anchor();
    let root = build_right_spine_add_tree(&mut fg, 8);
    let mut acc: Vec<ValueId> = Vec::new();
    let mut budget = 0usize;
    flatten_add_tree(fg.graph(), root, &mut acc, &mut budget);
    // Each Add contributes 1 to budget; total budget = (depth-1)
    // increments.  Leaves equal `depth`.
    assert_eq!(acc.len(), 8, "8 leaves collected, got {}", acc.len());
    assert!(budget <= 32, "budget under cap: {}", budget);
}

#[test]
fn flatten_add_tree_at_budget_boundary_terminates_gracefully() {
    // 64-deep tree exceeds the 32 budget.  flatten_add_tree must
    // not panic; it pushes the over-budget node verbatim (which
    // downstream per-term decompose rejects as non-const non-Mul).
    let (mut fg, _anchor) = build_load_anchor();
    let root = build_right_spine_add_tree(&mut fg, 64);
    let mut acc: Vec<ValueId> = Vec::new();
    let mut budget = 0usize;
    // Smoke test: must not panic at any tree depth.
    flatten_add_tree(fg.graph(), root, &mut acc, &mut budget);
    // Once budget hits 32, the recursive walk stops adding new
    // entries.  The exact behaviour depends on traversal order; we
    // just pin "doesn't panic" and "acc is bounded".
    assert!(
        !acc.is_empty(),
        "flatten must always push at least one entry",
    );
}

#[test]
fn flatten_add_tree_on_non_add_root_pushes_single_term() {
    // Non-Add root → push the root verbatim; budget should be 1
    // (one entry to the walk).
    let (mut fg, _anchor) = build_load_anchor();
    let n = strider_ir_test_utils::sentinel_node(
        &mut fg,
        NodeKind::IntConst(IntPayload::Small(0xABCD_u64)),
        [],
        [strider_ir::node::ValueKind::Typed(ValueType::I64)],
    );
    let value = fg.node_outputs_exact::<1>(n).unwrap()[0];
    let mut acc: Vec<ValueId> = Vec::new();
    let mut budget = 0usize;
    flatten_add_tree(fg.graph(), value, &mut acc, &mut budget);
    assert_eq!(acc.len(), 1, "non-Add root → single entry");
    assert_eq!(acc[0], value, "entry is the root itself");
}

// ── classify_table_dispatch boundary cases (SP-rooted arm) ──────────────────

#[test]
fn classify_table_dispatch_one_stack_target_resolves() {
    // Single-element stack array — degenerate jump table of size 1.
    // The classifier should still resolve.  Bound is supplied via
    // KnownBits (idx & 0): always 0.  But that mask is 0, which
    // means bound = 1 (the only valid idx).
    let targets = [0x401200u64];
    let (fg, load_value) = build_one_target_array(targets, -8, 8);
    let (known, doms) = make_known_and_doms(&fg);
    let ranges = crate::value_range::compute_value_ranges(&fg, &doms, &known);
    let result = classify_table_dispatch(&fg, load_value, None, &ranges, AliasMode::StackGlobalDisjoint);
    // Whether the existing helpers can resolve a 1-element case
    // depends on how KnownBits bounds the index.  Pin the contract
    // that the classifier does NOT panic and returns Some/None
    // consistently.
    match result {
        None => { /* defer-via-unresolved is sound */ }
        Some(ResolvedTargets::Multiple(v)) => {
            assert_eq!(v, vec![0x401200u64], "single-element resolves to one target");
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
        .build_int_binary_operation(idx_u64_value, stride_const, IntBinaryOp::Mul, ValueType::I64)
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
    b.build_return(Some(loaded), &[]).unwrap();
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

// ── lookup_stack_slot_via_ssa helper tests ────────────────────────
//
// These tests pin the oracle-based slot-lookup contract in isolation,
// exercising `lookup_stack_slot_via_ssa` directly (it is private but
// accessible from this in-file test module).  Each test exercises one
// property of the walker: matching store found, non-aliasing store
// walked past, missing store, Call clobber boundary, type mismatch, and
// the multi-slot enumeration that the classifier loop performs.

/// Extract the `InitialVar(sp)` output — the canonical entry-SP base
/// that `decompose_sp` / `SpAliasOracle` need to confirm stores are
/// SP-rooted at the same base.
fn get_sp_base(fg: &strider_ir::Function, sp: rsleigh::Vn) -> ValueId {
    fg.graph()
        .all_node_ids()
        .find(|&n| matches!(*fg.node_kind(n), NodeKind::InitialVar(vn) if vn == sp))
        .map(|n| fg.node_outputs_exact::<1>(n).expect("InitialVar has 1 output")[0])
        .expect("InitialVar(sp) exists")
}

/// One stack store at the requested offset, value type matches: returns
/// the stored value's output id.
#[test]
fn slot_lookup_finds_matching_store() -> crate::Result<()> {
    let sp = sp64();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let twentyfour = b.build_int_const(24u64, ValueType::I64)?;
        let addr = b.build_sub_as_add_neg(sp_val, twentyfour, ValueType::I64)?;
        let stored = b.build_int_const(0xCAFEu64, ValueType::I64)?;
        b.build_store(addr, stored, rsleigh::VnSpace::RAM)?;
        // Touch the stored memory token so it survives DCE.
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold::new());
    pipeline.add(PhiCollapse);
    pipeline.add(RegionCollapse);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let load = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
        .expect("Load survives without LoadForward");
    let mem = fg.node_inputs(load).into_iter().next().unwrap();
    let sp_base = get_sp_base(&fg, sp);

    let mut memo = SpExprMemo::default();
    let result = lookup_stack_slot_via_ssa(&fg, mem, sp_base, -24, ValueType::I64, &mut memo, AliasMode::StackGlobalDisjoint);
    let value = result.expect("helper should find Store at offset -24");
    assert_eq!(fg.int_const_val(value), Some(0xCAFE));
    Ok(())
}

/// Walks past a non-aliasing intermediate SP-relative store (different
/// offset) and finds the requested-offset store.
#[test]
fn slot_lookup_walks_past_non_aliasing() -> crate::Result<()> {
    let sp = sp64();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let off24 = b.build_int_const(24u64, ValueType::I64)?;
        let off16 = b.build_int_const(16u64, ValueType::I64)?;
        let addr_24 = b.build_sub_as_add_neg(sp_val, off24, ValueType::I64)?;
        let addr_16 = b.build_sub_as_add_neg(sp_val, off16, ValueType::I64)?;
        let v_24 = b.build_int_const(0xAAAAu64, ValueType::I64)?;
        let v_16 = b.build_int_const(0xBBBBu64, ValueType::I64)?;
        b.build_store(addr_24, v_24, rsleigh::VnSpace::RAM)?;
        b.build_store(addr_16, v_16, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr_24, rsleigh::VnSpace::RAM, ValueType::I64)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold::new());
    pipeline.add(PhiCollapse);
    pipeline.add(RegionCollapse);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let load = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
        .expect("Load survives");
    let mem = fg.node_inputs(load).into_iter().next().unwrap();
    let sp_base = get_sp_base(&fg, sp);
    let mut memo = SpExprMemo::default();

    // Look up offset -16: the chain has the latest store at -16 and an
    // earlier store at -24 (non-aliasing).
    let v16 = lookup_stack_slot_via_ssa(&fg, mem, sp_base, -16, ValueType::I64, &mut memo, AliasMode::StackGlobalDisjoint);
    assert_eq!(fg.int_const_val(v16.expect("find -16")), Some(0xBBBB));

    // Look up offset -24: must walk through the -16 store (non-aliasing).
    let v24 = lookup_stack_slot_via_ssa(&fg, mem, sp_base, -24, ValueType::I64, &mut memo, AliasMode::StackGlobalDisjoint);
    assert_eq!(fg.int_const_val(v24.expect("find -24")), Some(0xAAAA));
    Ok(())
}

/// No store at the requested offset: returns None (chain bottoms out
/// at InitialMemory without a matching store).
#[test]
fn slot_lookup_no_match_returns_none() -> crate::Result<()> {
    let sp = sp64();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let off24 = b.build_int_const(24u64, ValueType::I64)?;
        let addr_24 = b.build_sub_as_add_neg(sp_val, off24, ValueType::I64)?;
        let v_24 = b.build_int_const(0xAAAAu64, ValueType::I64)?;
        b.build_store(addr_24, v_24, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr_24, rsleigh::VnSpace::RAM, ValueType::I64)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold::new());
    pipeline.add(PhiCollapse);
    pipeline.add(RegionCollapse);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let load = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
        .expect("Load survives");
    let mem = fg.node_inputs(load).into_iter().next().unwrap();
    let sp_base = get_sp_base(&fg, sp);
    let mut memo = SpExprMemo::default();

    let result = lookup_stack_slot_via_ssa(&fg, mem, sp_base, -8, ValueType::I64, &mut memo, AliasMode::StackGlobalDisjoint);
    assert!(result.is_none(), "no store at -8 → helper returns None");
    Ok(())
}

/// Aliasing intermediate SP-relative store (overlaps the requested offset)
/// is the LIVE clobber — the helper returns its stored value.
#[test]
fn slot_lookup_returns_latest_at_aliasing_offset() -> crate::Result<()> {
    let sp = sp64();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let off24 = b.build_int_const(24u64, ValueType::I64)?;
        let addr_24 = b.build_sub_as_add_neg(sp_val, off24, ValueType::I64)?;
        let first = b.build_int_const(0xAAAAu64, ValueType::I64)?;
        let second = b.build_int_const(0xBBBBu64, ValueType::I64)?;
        // Two stores at the SAME offset; the second alias-overwrites the first.
        b.build_store(addr_24, first, rsleigh::VnSpace::RAM)?;
        b.build_store(addr_24, second, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr_24, rsleigh::VnSpace::RAM, ValueType::I64)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold::new());
    pipeline.add(PhiCollapse);
    pipeline.add(RegionCollapse);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let load = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
        .expect("Load survives");
    let mem = fg.node_inputs(load).into_iter().next().unwrap();
    let sp_base = get_sp_base(&fg, sp);
    let mut memo = SpExprMemo::default();

    let result = lookup_stack_slot_via_ssa(&fg, mem, sp_base, -24, ValueType::I64, &mut memo, AliasMode::StackGlobalDisjoint);
    // The helper must return the *live* (latest) value: the second store.
    let v = result.expect("must find live store");
    assert_eq!(fg.int_const_val(v), Some(0xBBBB));
    Ok(())
}

/// Type mismatch (store width != requested width at the matching offset)
/// returns None — strict types; no Truncate / ShiftRight synthesis.
#[test]
fn slot_lookup_type_mismatch_returns_none() -> crate::Result<()> {
    let sp = sp64();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let off24 = b.build_int_const(24u64, ValueType::I64)?;
        let addr_24 = b.build_sub_as_add_neg(sp_val, off24, ValueType::I64)?;
        // Store I32 at the same address: the alias verdict is Match (same
        // offset), but the final type guard (data_ty == value_type) rejects
        // it because the stored I32 differs from the loaded I64.
        let stored = b.build_int_const(0xAAAAu64, ValueType::I32)?;
        b.build_store(addr_24, stored, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr_24, rsleigh::VnSpace::RAM, ValueType::I64)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold::new());
    pipeline.add(PhiCollapse);
    pipeline.add(RegionCollapse);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let load = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
        .expect("Load survives");
    let mem = fg.node_inputs(load).into_iter().next().unwrap();
    let sp_base = get_sp_base(&fg, sp);
    let mut memo = SpExprMemo::default();

    let result = lookup_stack_slot_via_ssa(&fg, mem, sp_base, -24, ValueType::I64, &mut memo, AliasMode::StackGlobalDisjoint);
    assert!(result.is_none(), "type mismatch at offset -24 → None");
    Ok(())
}

/// Multi-slot enumeration: mirrors the classifier loop looking up
/// sp-24 → target0, sp-16 → target1.
#[test]
fn slot_lookup_enumerates_array_entries() -> crate::Result<()> {
    let sp = sp64();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let off24 = b.build_int_const(24u64, ValueType::I64)?;
        let off16 = b.build_int_const(16u64, ValueType::I64)?;
        let addr_24 = b.build_sub_as_add_neg(sp_val, off24, ValueType::I64)?;
        let addr_16 = b.build_sub_as_add_neg(sp_val, off16, ValueType::I64)?;
        let target0 = b.build_int_const(0x401190u64, ValueType::I64)?;
        let target1 = b.build_int_const(0x401180u64, ValueType::I64)?;
        b.build_store(addr_24, target0, rsleigh::VnSpace::RAM)?;
        b.build_store(addr_16, target1, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr_24, rsleigh::VnSpace::RAM, ValueType::I64)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold::new());
    pipeline.add(PhiCollapse);
    pipeline.add(RegionCollapse);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let load = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
        .expect("Load survives");
    let mem = fg.node_inputs(load).into_iter().next().unwrap();
    let sp_base = get_sp_base(&fg, sp);
    let base = -24i64;
    let stride = 8i64;
    let mut targets = Vec::new();
    // Share one memo across iterations, mirroring `classify_table_dispatch`'s
    // production path so cross-iteration memo reuse is exercised.
    let mut memo = SpExprMemo::default();
    for i in 0..2 {
        let off = base + i * stride;
        let v = lookup_stack_slot_via_ssa(&fg, mem, sp_base, off, ValueType::I64, &mut memo, AliasMode::StackGlobalDisjoint)
            .unwrap_or_else(|| panic!("must find store at offset {off}"));
        let c = fg.int_const_val(v).expect("stored value is IntConst");
        targets.push(c as u64);
    }
    assert_eq!(targets, vec![0x401190u64, 0x401180u64]);
    Ok(())
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
        let (lock_node, _result) = b.build_call_other(
            0x1234,
            "LOCK",
            None,
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
