//! Unit tests for the jump-table classifier.
//!
//! Each test builds a minimal [`Graph`] via
//! [`strider_ir::FunctionBuilder::new_raw`] (and `graph.create_node` for
//! shapes the validator otherwise rejects), then invokes the
//! piece-under-test in isolation.  Helpers are scoped to the
//! module rather than promoted to `indirect_resolve_helpers.rs` so the
//! unit tests stay self-contained.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

use super::*;
use std::sync::Mutex;
use strider_ir::Function;
use strider_ir::FunctionBuilder;
use strider_ir::IRBuilderExt;
use strider_ir::IRWalker;
use strider_ir::IntBinaryOp;
use strider_ir::node::ValueType;
use strider_ir_test_utils::{MockRom, RegisterSet};

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
/// services.  Used to assert `read_table_entries` issues exactly
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
/// region terminated by a placeholder `Return(anchor)`.  The
/// caller-supplied closure builds the anchor's producer subtree.
fn build_with_anchor(
    anchor_inputs: impl FnOnce(&mut FunctionBuilder) -> ValueId,
) -> (Function, ValueId) {
    let mut builder = strider_ir_test_utils::empty_builder().expect("FunctionBuilder::new_raw");
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

// ── Shape-match tests ────────────────────────────────────────────────────

/// Build a non-IntConst integer value usable as `idx` for the
/// shape tests.  We need a producer that ISN'T an IntConst so
/// `match_jump_table_shape` can disambiguate `idx` from
/// `IntConst(stride)` in commuted multiplications — otherwise
/// both mul operands are IntConsts and the matcher picks the
/// wrong "stride".
fn build_non_const_idx(fb: &mut FunctionBuilder) -> ValueId {
    let addr = fb.build_int_const(0x9000u64, ValueType::I32).unwrap();
    fb.build_load(addr, VnSpace::RAM, ValueType::I32)
        .expect("u32 load (idx)")
}

#[test]
fn match_jump_table_shape_recognises_canonical_form() {
    // Load[base + idx*stride], non-commuted variant.  idx is a
    // load (non-const) so the shape match's stride-vs-idx
    // disambiguation is exercised cleanly.
    let (g, anchor) = build_jt_load(0x4000, 4, false, false, build_non_const_idx);
    let shape = match_jump_table_shape(&g, anchor).expect("must match");
    assert_eq!(shape.base, 0x4000);
    assert_eq!(shape.stride, 4);
    assert_eq!(shape.entry_size, 4);
}

#[test]
fn match_jump_table_shape_recognises_commuted_intadd() {
    // IntAdd(IntMul(idx, stride), IntConst(base)) — base on the
    // right.  match-shape must try both orderings.
    let (g, anchor) = build_jt_load(0x5000, 4, true, false, build_non_const_idx);
    let shape = match_jump_table_shape(&g, anchor).expect("must match commuted add");
    assert_eq!(shape.base, 0x5000);
    assert_eq!(shape.stride, 4);
}

#[test]
fn match_jump_table_shape_recognises_commuted_intmul() {
    // IntMul(IntConst(stride), idx) — stride on the left of the
    // multiplication.
    let (g, anchor) = build_jt_load(0x6000, 8, false, true, build_non_const_idx);
    let shape = match_jump_table_shape(&g, anchor).expect("must match commuted mul");
    assert_eq!(shape.base, 0x6000);
    assert_eq!(shape.stride, 8);
}

#[test]
fn match_jump_table_shape_recognises_both_commutations() {
    // Both add and mul commuted — the worst-case ordering.
    let (g, anchor) = build_jt_load(0x7000, 4, true, true, build_non_const_idx);
    let shape = match_jump_table_shape(&g, anchor).expect("must match both commuted");
    assert_eq!(shape.base, 0x7000);
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
fn match_jump_table_shape_recognises_shl_form() {
    // AArch64 `ldr xN, [base, idx, lsl #2]` shape — table of 4-byte
    // entries.  `Shl(idx, 2)` is arithmetically equal to
    // `Mul(idx, 4)` but lifts as a distinct IR op.
    let (g, anchor) = build_jt_load_shl(0x4000, 2, false, build_non_const_idx);
    let shape = match_jump_table_shape(&g, anchor).expect("Shl-scaled table must match");
    assert_eq!(shape.base, 0x4000);
    assert_eq!(shape.stride, 4); // 1 << 2
    assert_eq!(shape.entry_size, 4);
}

#[test]
fn match_jump_table_shape_recognises_shl_form_commuted_add() {
    // `Shl` itself is non-commutative, but the surrounding `Add` is
    // — so we must still match `(idx<<shift) + base` as well as
    // `base + (idx<<shift)`.
    let (g, anchor) = build_jt_load_shl(0x5000, 3, true, build_non_const_idx);
    let shape =
        match_jump_table_shape(&g, anchor).expect("Shl-scaled table with commuted add must match");
    assert_eq!(shape.base, 0x5000);
    assert_eq!(shape.stride, 8); // 1 << 3 — AArch64 jump table of 8-byte pointers
}

#[test]
fn match_jump_table_shape_rejects_shl_with_oversize_shift() {
    // `Shl(idx, 64)` would compute `1u64 << 64`, which is UB / would
    // overflow the implied stride.  Reject — real jump tables top
    // out at shift = 3.
    let (g, anchor) = build_jt_load_shl(0x6000, 64, false, build_non_const_idx);
    assert!(
        match_jump_table_shape(&g, anchor).is_none(),
        "shift >= 64 must reject; otherwise stride computation overflows"
    );
}

#[test]
fn match_jump_table_shape_rejects_non_load_producer() {
    // Anchor is a raw IntConst, not a Load.  Reject.
    let (g, anchor) =
        build_with_anchor(|fb| fb.build_int_const(0x1000u64, ValueType::I32).unwrap());
    assert!(match_jump_table_shape(&g, anchor).is_none());
}

#[test]
fn match_jump_table_shape_rejects_load_with_unrelated_addr_shape() {
    // Load[IntConst(addr)] — a simple global read, no Add/Mul.
    // Our shape requires IntAdd at the top of the address tree.
    let (g, anchor) = build_with_anchor(|fb| {
        let addr = fb.build_int_const(0x1234u64, ValueType::I32).unwrap();
        fb.build_load(addr, VnSpace::RAM, ValueType::I32)
            .expect("load")
    });
    assert!(match_jump_table_shape(&g, anchor).is_none());
}

#[test]
fn match_jump_table_shape_rejects_load_without_intconst_base() {
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
    assert!(match_jump_table_shape(&g, anchor).is_none());
}


// ── Read-table-entries tests ─────────────────────────────────────────────

#[test]
fn read_table_entries_returns_targets_in_index_order() {
    // 4 entries: 0x100, 0x200, 0x300, 0x400.  Stride 4, base
    // 0x4000.  Verify the returned vec preserves index order.
    let rom = MockRom::strided(0x4000, 4, vec![0x100, 0x200, 0x300, 0x400], 4);
    let result = read_table_entries(&rom, 0x4000, 4, 4, 4, strider_target::Endianness::Little)
        .expect("must read all");
    assert_eq!(result, vec![0x100, 0x200, 0x300, 0x400]);
}

#[test]
fn read_table_entries_returns_none_on_partial_read() {
    // 4 entries requested; rom only serves the first 2.  Must
    // fail closed: returns None, NOT a Vec of length 2.
    let rom = MockRom::strided(0x5000, 4, vec![0x100, 0x200, 0x300, 0x400], 4).with_cutoff(2);
    assert_eq!(
        read_table_entries(&rom, 0x5000, 4, 4, 4, strider_target::Endianness::Little),
        None
    );
}

#[test]
fn read_table_entries_issues_count_reads_in_index_order() {
    // RecordingRom logs every (addr, size) pair.  For 3 entries
    // at stride 4, base 0x6000, expect: (0x6000, 4), (0x6004, 4),
    // (0x6008, 4) in that order.
    let rom = RecordingRom {
        inner: MockRom::strided(0x6000, 4, vec![0xaaaa, 0xbbbb, 0xcccc], 4),
        log: Mutex::new(Vec::new()),
    };
    let _ = read_table_entries(&rom, 0x6000, 4, 3, 4, strider_target::Endianness::Little)
        .expect("read");
    let log = rom.log.lock().unwrap().clone();
    assert_eq!(log, vec![(0x6000, 4), (0x6004, 4), (0x6008, 4)]);
}

// ── End-to-end classifier-on-shape tests ────────────────────────────────

#[test]
fn classify_jump_table_with_known_bits_bound_returns_multiple() {
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
    let result = classify_jump_table(
        &g,
        anchor,
        Some(&rom),
        strider_target::Endianness::Little,
        &ranges,
    );
    match result {
        Some(ResolvedTargets::Multiple(ts)) => {
            assert_eq!(ts, vec![0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80]);
        }
        other => panic!("expected Multiple([0x10..0x80]); got {other:?}"),
    }
}

#[test]
fn classify_jump_table_no_rom_returns_none() {
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
    let result =
        classify_jump_table(&g, anchor, None, strider_target::Endianness::Little, &ranges);
    assert_eq!(result, None);
}

#[test]
fn classify_jump_table_unbounded_idx_returns_none() {
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
    let result = classify_jump_table(
        &g,
        anchor,
        Some(&rom),
        strider_target::Endianness::Little,
        &ranges,
    );
    assert_eq!(result, None);
}

#[test]
fn classify_jump_table_with_if_guard_bound_returns_multiple() {
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

    let result = classify_jump_table(
        &function,
        anchor,
        Some(&rom),
        strider_target::Endianness::Little,
        &ranges,
    );
    match result {
        Some(ResolvedTargets::Multiple(ts)) => {
            assert_eq!(ts, vec![0x10, 0x20, 0x30, 0x40]);
        }
        other => panic!("expected Multiple([0x10..0x40]); got {other:?}"),
    }
}

#[test]
fn classify_jump_table_diamond_both_paths_guarded_resolves() {
    // A dispatch with two predecessor paths, both guarded `idx < 4`.
    // The range pass's union-of-arm approach in `resolve_phi` (querying
    // each arm in the joining region rather than the predecessor) handles
    // multi-input phi dispatches correctly.
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
    let result = classify_jump_table(
        &function,
        anchor,
        Some(&rom),
        strider_target::Endianness::Little,
        &ranges,
    );
    match result {
        Some(ResolvedTargets::Multiple(ts)) => {
            assert_eq!(
                ts,
                vec![0x10, 0x20, 0x30, 0x40],
                "diamond with both arms guarded must resolve"
            );
        }
        other => panic!("expected Multiple([0x10..0x40]) from diamond; got {other:?}"),
    }
}

