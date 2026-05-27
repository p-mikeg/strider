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
use crate::opt::analyze_known_bits;
use strider_ir::Function;
use strider_ir::FunctionBuilder;
use strider_ir::IntBinaryOp;
use strider_ir::node::NodeOutputType;
use strider_ir_test_utils::{MockRom, RegisterSet};
use std::sync::Mutex;

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
    fn read(&self, addr: u64, size: usize) -> Option<u64> {
        self.log.lock().unwrap().push((addr, size));
        self.inner.read(addr, size)
    }
}

/// Minimal `Graph` carrying nothing but the entry
/// region terminated by a placeholder `Return(anchor)`.  The
/// caller-supplied closure builds the anchor's producer subtree.
fn build_with_anchor(
    anchor_inputs: impl FnOnce(&mut FunctionBuilder) -> NodeOutputId,
) -> (Function, NodeOutputId) {
    let mut builder = FunctionBuilder::empty()
        .expect("FunctionBuilder::new_raw");
    let region = builder.create_region().expect("create_region");
    builder.set_entry_region(region).expect("set_entry_region");
    builder.set_region(region);
    builder.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let anchor = anchor_inputs(&mut builder);
    builder.build_indirect_branch(anchor).expect("build_indirect_branch");
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
    idx_provider: impl FnOnce(&mut FunctionBuilder) -> NodeOutputId,
) -> (Function, NodeOutputId) {
    build_with_anchor(|fb| {
        let idx = idx_provider(fb);
        let stride_c = fb.build_int_const(stride, NodeOutputType::I32).unwrap();
        let mul = if commute_mul {
            fb.build_int_binary_operation(stride_c, idx, IntBinaryOp::Mul, NodeOutputType::I32)
                .expect("mul")
        } else {
            fb.build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, NodeOutputType::I32)
                .expect("mul")
        };
        let base_c = fb.build_int_const(base, NodeOutputType::I32).unwrap();
        let addr = if commute_add {
            fb.build_int_binary_operation(mul, base_c, IntBinaryOp::Add, NodeOutputType::I32)
                .expect("add")
        } else {
            fb.build_int_binary_operation(base_c, mul, IntBinaryOp::Add, NodeOutputType::I32)
                .expect("add")
        };
        fb.build_load(addr, VnSpace::RAM, NodeOutputType::I32)
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
fn build_non_const_idx(fb: &mut FunctionBuilder) -> NodeOutputId {
    let addr = fb.build_int_const(0x9000u64, NodeOutputType::I32).unwrap();
    fb.build_load(addr, VnSpace::RAM, NodeOutputType::I32)
        .expect("u32 load (idx)")
}

#[test]
fn match_jump_table_shape_recognises_canonical_form() {
    // Load[base + idx*stride], non-commuted variant.  idx is a
    // load (non-const) so the shape match's stride-vs-idx
    // disambiguation is exercised cleanly.
    let (g, anchor) = build_jt_load(0x4000, 4, false, false, build_non_const_idx);
    let shape = match_jump_table_shape(crate::pattern::RewriteCtxView::from_built(&g).unwrap(), anchor).expect("must match");
    assert_eq!(shape.base, 0x4000);
    assert_eq!(shape.stride, 4);
    assert_eq!(shape.entry_size, 4);
}

#[test]
fn match_jump_table_shape_recognises_commuted_intadd() {
    // IntAdd(IntMul(idx, stride), IntConst(base)) — base on the
    // right.  match-shape must try both orderings.
    let (g, anchor) = build_jt_load(0x5000, 4, true, false, build_non_const_idx);
    let shape = match_jump_table_shape(crate::pattern::RewriteCtxView::from_built(&g).unwrap(), anchor).expect("must match commuted add");
    assert_eq!(shape.base, 0x5000);
    assert_eq!(shape.stride, 4);
}

#[test]
fn match_jump_table_shape_recognises_commuted_intmul() {
    // IntMul(IntConst(stride), idx) — stride on the left of the
    // multiplication.
    let (g, anchor) = build_jt_load(0x6000, 8, false, true, build_non_const_idx);
    let shape = match_jump_table_shape(crate::pattern::RewriteCtxView::from_built(&g).unwrap(), anchor).expect("must match commuted mul");
    assert_eq!(shape.base, 0x6000);
    assert_eq!(shape.stride, 8);
}

#[test]
fn match_jump_table_shape_recognises_both_commutations() {
    // Both add and mul commuted — the worst-case ordering.
    let (g, anchor) = build_jt_load(0x7000, 4, true, true, build_non_const_idx);
    let shape = match_jump_table_shape(crate::pattern::RewriteCtxView::from_built(&g).unwrap(), anchor).expect("must match both commuted");
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
    idx_provider: impl FnOnce(&mut FunctionBuilder) -> NodeOutputId,
) -> (Function, NodeOutputId) {
    build_with_anchor(|fb| {
        let idx = idx_provider(fb);
        let shift_c = fb.build_int_const(shift, NodeOutputType::I32).unwrap();
        let scaled = fb
            .build_int_binary_operation(idx, shift_c, IntBinaryOp::ShiftLeft, NodeOutputType::I32)
            .expect("shl");
        let base_c = fb.build_int_const(base, NodeOutputType::I32).unwrap();
        let addr = if commute_add {
            fb.build_int_binary_operation(scaled, base_c, IntBinaryOp::Add, NodeOutputType::I32)
                .expect("add")
        } else {
            fb.build_int_binary_operation(base_c, scaled, IntBinaryOp::Add, NodeOutputType::I32)
                .expect("add")
        };
        fb.build_load(addr, VnSpace::RAM, NodeOutputType::I32)
            .expect("load")
    })
}

#[test]
fn match_jump_table_shape_recognises_shl_form() {
    // AArch64 `ldr xN, [base, idx, lsl #2]` shape — table of 4-byte
    // entries.  `Shl(idx, 2)` is arithmetically equal to
    // `Mul(idx, 4)` but lifts as a distinct IR op.
    let (g, anchor) = build_jt_load_shl(0x4000, 2, false, build_non_const_idx);
    let shape = match_jump_table_shape(crate::pattern::RewriteCtxView::from_built(&g).unwrap(), anchor)
        .expect("Shl-scaled table must match");
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
    let shape = match_jump_table_shape(crate::pattern::RewriteCtxView::from_built(&g).unwrap(), anchor)
        .expect("Shl-scaled table with commuted add must match");
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
        match_jump_table_shape(crate::pattern::RewriteCtxView::from_built(&g).unwrap(), anchor).is_none(),
        "shift >= 64 must reject; otherwise stride computation overflows"
    );
}

#[test]
fn match_jump_table_shape_rejects_non_load_producer() {
    // Anchor is a raw IntConst, not a Load.  Reject.
    let (g, anchor) = build_with_anchor(|fb| fb.build_int_const(0x1000u64, NodeOutputType::I32).unwrap());
    assert!(match_jump_table_shape(crate::pattern::RewriteCtxView::from_built(&g).unwrap(), anchor).is_none());
}

#[test]
fn match_jump_table_shape_rejects_load_with_unrelated_addr_shape() {
    // Load[IntConst(addr)] — a simple global read, no Add/Mul.
    // Our shape requires IntAdd at the top of the address tree.
    let (g, anchor) = build_with_anchor(|fb| {
        let addr = fb.build_int_const(0x1234u64, NodeOutputType::I32).unwrap();
        fb.build_load(addr, VnSpace::RAM, NodeOutputType::I32).expect("load")
    });
    assert!(match_jump_table_shape(crate::pattern::RewriteCtxView::from_built(&g).unwrap(), anchor).is_none());
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
        let idx = fb.build_int_const(2u64, NodeOutputType::I32).unwrap();
        let stride_c = fb.build_int_const(4u64, NodeOutputType::I32).unwrap();
        let mul1 = fb
            .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, NodeOutputType::I32)
            .expect("mul1");
        let mul2 = fb
            .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, NodeOutputType::I32)
            .expect("mul2");
        let addr = fb
            .build_int_binary_operation(mul1, mul2, IntBinaryOp::Add, NodeOutputType::I32)
            .expect("add");
        fb.build_load(addr, VnSpace::RAM, NodeOutputType::I32).expect("load")
    });
    assert!(match_jump_table_shape(crate::pattern::RewriteCtxView::from_built(&g).unwrap(), anchor).is_none());
}

// ── Bound-via-known-bits tests ───────────────────────────────────────────

#[test]
fn bound_via_known_bits_returns_max_plus_one() {
    // idx = (some_var) & 0x7 → bound = 8.
    let (g, idx) = build_with_anchor(|fb| {
        let v = fb.build_int_const(0xffff_ffffu64, NodeOutputType::I32).unwrap();
        let mask = fb.build_int_const(0x7u64, NodeOutputType::I32).unwrap();
        fb.build_int_binary_operation(v, mask, IntBinaryOp::And, NodeOutputType::I32)
            .expect("and")
    });
    let known = analyze_known_bits(crate::pattern::RewriteCtxView::from_built(&g).unwrap()).expect("kb analyze");
    let bound = bound_via_known_bits(crate::pattern::RewriteCtxView::from_built(&g).unwrap(), idx, &known).expect("must bound");
    assert_eq!(bound, 8);
}

#[test]
fn bound_via_known_bits_returns_none_when_unbounded() {
    // idx = some unbounded I32 (a load output, no AND mask) → None.
    let (g, idx) = build_with_anchor(|fb| {
        let addr = fb.build_int_const(0x1000u64, NodeOutputType::I32).unwrap();
        fb.build_load(addr, VnSpace::RAM, NodeOutputType::I32).expect("load")
    });
    let known = analyze_known_bits(crate::pattern::RewriteCtxView::from_built(&g).unwrap()).expect("kb analyze");
    assert_eq!(bound_via_known_bits(crate::pattern::RewriteCtxView::from_built(&g).unwrap(), idx, &known), None);
}

#[test]
fn bound_via_known_bits_with_int_const_input() {
    // idx = IntConst(5) directly.  KnownBits gives mask = 5,
    // bound = 6.  (Real graphs would have ConstantFold collapse
    // this to a Single, but the local recurrence handles it
    // anyway.)
    let (g, idx) = build_with_anchor(|fb| fb.build_int_const(5u64, NodeOutputType::I32).unwrap());
    let known = analyze_known_bits(crate::pattern::RewriteCtxView::from_built(&g).unwrap()).expect("kb analyze");
    let bound = bound_via_known_bits(crate::pattern::RewriteCtxView::from_built(&g).unwrap(), idx, &known).expect("must bound a const");
    assert_eq!(bound, 6);
}

#[test]
fn bound_via_known_bits_handles_zero_extend() {
    // idx = ZeroExtend(u8 value).  Bound = 256 from the
    // narrower-type mask, regardless of the wider I32's full
    // range.  We build the Extend by hand (post-`build()`) because
    // the public `extend_if_needed` short-circuits constant inputs
    // to a folded IntConst, which would defeat the test's purpose;
    // we then route the Extend through the placeholder Return so
    // it lands on the entry-reachable spine the analyzer scopes
    // its worklist to.
    use strider_ir::node::NodeOutputKind;
    let mut builder = FunctionBuilder::empty().unwrap();
    let region = builder.create_region().unwrap();
    builder.set_entry_region(region).unwrap();
    builder.set_region(region);
    builder.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let addr = builder.build_int_const(0x9000u64, NodeOutputType::I32).unwrap();
    let narrow = builder
        .build_load(addr, VnSpace::RAM, NodeOutputType::I8)
        .expect("u8 load");
    // Provide a placeholder return value so build() succeeds; we
    // rewire the Return's value input to the new Extend below.
    let placeholder = builder.build_int_const(0u64, NodeOutputType::I32).unwrap();
    builder.build_indirect_branch(placeholder).expect("build_indirect_branch");
    builder.set_lift_addr(None);
    let mut function = builder.build().expect("build");
    let extend_node = function.create_node(
        NodeKind::Extend(strider_ir::ExtendOp::ZeroExtend),
        [narrow],
        [NodeOutputKind::OutputType(NodeOutputType::I32)],
    );
    function.set_asm_fingerprint(extend_node, vec![strider_ir_test_utils::SENTINEL_LIFT_ADDR]);
    let [idx] = function
        .node_outputs_exact::<1>(extend_node)
        .expect("extend output");
    // Replace the placeholder with the Extend so the Return
    // depends on it; `walk_graph` then sweeps it into preorder.
    function.replace_all_uses(placeholder, idx).expect("rewire");
    let known = analyze_known_bits(crate::pattern::RewriteCtxView::from_built(&function).unwrap()).expect("kb analyze");
    let bound = bound_via_known_bits(crate::pattern::RewriteCtxView::from_built(&function).unwrap(), idx, &known).expect("bound from zero-extend");
    // I8 narrows to 0..255, so bound = 256.
    assert_eq!(bound, 256);
}

#[test]
fn bound_via_known_bits_returns_none_for_unreachable_output() {
    // `analyze_known_bits` deliberately scopes its worklist to
    // entry-reachable nodes (the local-typing reachability boundary).
    // An output whose producer is not in the preorder traversal
    // gets `KnownBitsFacts::default()` (all-unknown), which yields `max ==
    // type_mask` and `bound_via_known_bits` returns None.
    //
    // This test pins the documented contract: callers may safely
    // pass any NodeOutputId — unreachable producers degrade to the
    // None-fallback rather than panic or return spurious bounds.
    use strider_ir::node::NodeOutputKind;
    let mut builder = FunctionBuilder::empty().unwrap();
    let region = builder.create_region().unwrap();
    builder.set_entry_region(region).unwrap();
    builder.set_region(region);
    builder.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    // Build a placeholder Return so build() succeeds.
    let placeholder = builder.build_int_const(0u64, NodeOutputType::I64).unwrap();
    builder.build_indirect_branch(placeholder).unwrap();
    builder.set_lift_addr(None);
    let mut function = builder.build().unwrap();

    // Build a detached AND that's narrower than I64 — definitely a
    // narrowing kb result IF the analyzer reached it.  Wire its
    // input to a fresh IntConst that is also detached so the AND is
    // truly unreachable from entry.
    let detached_const = function.create_node(
        NodeKind::IntConst(0xffff_ffffu128),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::I32)],
    );
    let detached_const_out = function
        .node_outputs_exact::<1>(detached_const)
        .expect("output")[0];
    let mask_const = function.create_node(
        NodeKind::IntConst(0x7u128),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::I32)],
    );
    let mask_const_out = function
        .node_outputs_exact::<1>(mask_const)
        .expect("output")[0];
    let detached_and = function.create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::And),
        [detached_const_out, mask_const_out],
        [NodeOutputKind::OutputType(NodeOutputType::I32)],
    );
    let detached_idx = function
        .node_outputs_exact::<1>(detached_and)
        .expect("output")[0];

    // The detached AND's output isn't in the entry preorder, so
    // `analyze_known_bits` never visits it.  Its kb defaults to
    // all-unknown and `bound_via_known_bits` returns None.
    let known = analyze_known_bits(crate::pattern::RewriteCtxView::from_built(&function).unwrap()).expect("kb analyze");
    let bound = bound_via_known_bits(crate::pattern::RewriteCtxView::from_built(&function).unwrap(), detached_idx, &known);
    assert_eq!(
        bound, None,
        "unreachable output must yield None (default KnownBitsFacts, no narrowing)",
    );
}

// ── Read-table-entries tests ─────────────────────────────────────────────

#[test]
fn read_table_entries_returns_targets_in_index_order() {
    // 4 entries: 0x100, 0x200, 0x300, 0x400.  Stride 4, base
    // 0x4000.  Verify the returned vec preserves index order.
    let rom = MockRom::strided(0x4000, 4, vec![0x100, 0x200, 0x300, 0x400], 4);
    let result = read_table_entries(&rom, 0x4000, 4, 4, 4).expect("must read all");
    assert_eq!(result, vec![0x100, 0x200, 0x300, 0x400]);
}

#[test]
fn read_table_entries_returns_none_on_partial_read() {
    // 4 entries requested; rom only serves the first 2.  Must
    // fail closed: returns None, NOT a Vec of length 2.
    let rom = MockRom::strided(0x5000, 4, vec![0x100, 0x200, 0x300, 0x400], 4).with_cutoff(2);
    assert_eq!(read_table_entries(&rom, 0x5000, 4, 4, 4), None);
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
    let _ = read_table_entries(&rom, 0x6000, 4, 3, 4).expect("read");
    let log = rom.log.lock().unwrap().clone();
    assert_eq!(log, vec![(0x6000, 4), (0x6004, 4), (0x6008, 4)]);
}

// ── End-to-end classifier-on-shape tests ────────────────────────────────

#[test]
fn classify_jump_table_with_known_bits_bound_returns_multiple() {
    // idx = (load) & 0x7 → bound 8.
    // Load[base + idx*stride] → resolves to Multiple of
    // table[0..8].
    let (g, anchor) = build_with_anchor(|fb| {
        // idx side: AND-masked to 0..7.
        let raw = fb.build_int_const(0xffff_ffffu64, NodeOutputType::I32).unwrap();
        let mask = fb.build_int_const(0x7u64, NodeOutputType::I32).unwrap();
        let idx = fb
            .build_int_binary_operation(raw, mask, IntBinaryOp::And, NodeOutputType::I32)
            .expect("and");
        let stride_c = fb.build_int_const(4u64, NodeOutputType::I32).unwrap();
        let mul = fb
            .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, NodeOutputType::I32)
            .expect("mul");
        let base_c = fb.build_int_const(0x4000u64, NodeOutputType::I32).unwrap();
        let addr = fb
            .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, NodeOutputType::I32)
            .expect("add");
        fb.build_load(addr, VnSpace::RAM, NodeOutputType::I32)
            .expect("load")
    });
    let rom = MockRom::strided(
        0x4000,
        4,
        vec![0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80],
        4,
    );
    let known = analyze_known_bits(crate::pattern::RewriteCtxView::from_built(&g).unwrap()).expect("kb analyze");
    let result = classify_jump_table(crate::pattern::RewriteCtxView::from_built(&g).unwrap(), anchor, Some(&rom), &known);
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
        let raw = fb.build_int_const(0xffff_ffffu64, NodeOutputType::I32).unwrap();
        let mask = fb.build_int_const(0x3u64, NodeOutputType::I32).unwrap();
        let idx = fb
            .build_int_binary_operation(raw, mask, IntBinaryOp::And, NodeOutputType::I32)
            .expect("and");
        let stride_c = fb.build_int_const(4u64, NodeOutputType::I32).unwrap();
        let mul = fb
            .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, NodeOutputType::I32)
            .expect("mul");
        let base_c = fb.build_int_const(0x4000u64, NodeOutputType::I32).unwrap();
        let addr = fb
            .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, NodeOutputType::I32)
            .expect("add");
        fb.build_load(addr, VnSpace::RAM, NodeOutputType::I32).expect("load")
    });
    let known = analyze_known_bits(crate::pattern::RewriteCtxView::from_built(&g).unwrap()).expect("kb analyze");
    let result = classify_jump_table(crate::pattern::RewriteCtxView::from_built(&g).unwrap(), anchor, None, &known);
    assert_eq!(result, None);
}

#[test]
fn classify_jump_table_unbounded_idx_returns_none() {
    // Shape is jt-shaped, but `idx` is a raw load with no AND
    // mask; predecessor-If walk also can't bound it (no If on
    // the path).  Must return None, not a Multiple.
    let (g, anchor) = build_with_anchor(|fb| {
        let some_addr = fb.build_int_const(0x9000u64, NodeOutputType::I32).unwrap();
        let idx = fb
            .build_load(some_addr, VnSpace::RAM, NodeOutputType::I32)
            .expect("load idx");
        let stride_c = fb.build_int_const(4u64, NodeOutputType::I32).unwrap();
        let mul = fb
            .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, NodeOutputType::I32)
            .expect("mul");
        let base_c = fb.build_int_const(0x4000u64, NodeOutputType::I32).unwrap();
        let addr = fb
            .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, NodeOutputType::I32)
            .expect("add");
        fb.build_load(addr, VnSpace::RAM, NodeOutputType::I32).expect("load")
    });
    let rom = MockRom::strided(0x4000, 4, vec![0x10, 0x20, 0x30, 0x40], 4);
    let known = analyze_known_bits(crate::pattern::RewriteCtxView::from_built(&g).unwrap()).expect("kb analyze");
    let result = classify_jump_table(crate::pattern::RewriteCtxView::from_built(&g).unwrap(), anchor, Some(&rom), &known);
    assert_eq!(result, None);
}

// ── bound_from_if_condition unit tests (direct) ─────────────────────────

#[test]
fn bound_from_if_condition_idx_less_than_n_true() {
    // Build idx and an `IntCmpOp::Less(idx, IntConst(4))`.  The
    // helper is on the `on_true` branch → bound = 4.
    let mut builder = FunctionBuilder::empty().unwrap();
    let region = builder.create_region().unwrap();
    builder.set_entry_region(region).unwrap();
    builder.set_region(region);
    builder.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let idx = builder.build_int_const(0u64, NodeOutputType::I32).unwrap();
    let n = builder.build_int_const(4u64, NodeOutputType::I32).unwrap();
    let cmp = builder
        .build_int_cmp_operation(idx, n, IntCmpOp::Less, NodeOutputType::I32)
        .unwrap();
    // Anchor with a placeholder return so build() succeeds.
    builder.build_indirect_branch(idx).unwrap();
    builder.set_lift_addr(None);
    let function = builder.build().unwrap();
    let bound = bound_from_if_condition(crate::pattern::RewriteCtxView::from_built(&function).unwrap(), cmp, idx, /* on_true */ true, &analyze_known_bits(crate::pattern::RewriteCtxView::from_built(&function).unwrap()).unwrap());
    assert_eq!(bound, Some(4));
}

#[test]
fn bound_from_if_condition_idx_less_than_n_false_returns_none() {
    // Same shape, but on the false branch → no upper bound.
    let mut builder = FunctionBuilder::empty().unwrap();
    let region = builder.create_region().unwrap();
    builder.set_entry_region(region).unwrap();
    builder.set_region(region);
    builder.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let idx = builder.build_int_const(0u64, NodeOutputType::I32).unwrap();
    let n = builder.build_int_const(4u64, NodeOutputType::I32).unwrap();
    let cmp = builder
        .build_int_cmp_operation(idx, n, IntCmpOp::Less, NodeOutputType::I32)
        .unwrap();
    builder.build_indirect_branch(idx).unwrap();
    builder.set_lift_addr(None);
    let function = builder.build().unwrap();
    let bound = bound_from_if_condition(crate::pattern::RewriteCtxView::from_built(&function).unwrap(), cmp, idx, /* on_true */ false, &analyze_known_bits(crate::pattern::RewriteCtxView::from_built(&function).unwrap()).unwrap());
    assert_eq!(bound, None);
}

#[test]
fn bound_from_if_condition_signed_less_unknown_sign_bit_returns_none() {
    // CORRECTNESS: `IntCmpOp::Sless` (signed `<`) on the true branch
    // bounds `idx` above by `N`, but the implicit *lower* bound is
    // `INT_MIN`, NOT `0`.  Without a separate proof that `idx >= 0`,
    // advertising target set `0..N` is unsound — runtime `idx` could
    // be negative and reach OOB via the wrapped unsigned cast.
    //
    // The classifier therefore requires KnownBits to prove the high
    // (sign) bit of `idx` is zero before accepting the `Sless` arm.
    // Here, `idx` is read from a tracked register varnode whose
    // KnownBits is fully unknown, so the helper falls through to
    // `None`.  The orchestrator then surfaces this dispatch as
    // `UnresolvedIndirectBranch` at fixed point — which is the
    // sound outcome.
    let idx_var = rsleigh::Vn {
        addr_off: 0x10,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let mut b = RegisterSet::new()
        .tracked(idx_var)
        .build_fn_single_region()
        .unwrap();
    let idx = b.read_variable(&idx_var).unwrap();
    let n = b.build_int_const(8u64, NodeOutputType::I32).unwrap();
    let cmp = b
        .build_int_cmp_operation(idx, n, IntCmpOp::Sless, NodeOutputType::I32)
        .unwrap();
    b.build_indirect_branch(idx).unwrap();
    b.set_lift_addr(None);
    let function = b.build().unwrap();
    let bound = bound_from_if_condition(
        crate::pattern::RewriteCtxView::from_built(&function).unwrap(),
        cmp,
        idx,
        /* on_true */ true,
        &analyze_known_bits(crate::pattern::RewriteCtxView::from_built(&function).unwrap()).unwrap(),
    );
    assert_eq!(bound, None, "Sless without idx>=0 proof must fall through");
}

#[test]
fn bound_from_if_condition_signed_less_with_known_nonneg_idx_accepts() {
    // POSITIVE: when KnownBits proves `idx`'s sign bit is zero (here
    // via `idx & 0x7F` — masking off the top 25 bits including the
    // sign bit of a I32), the `Sless` arm is sound and the classifier
    // accepts the bound `N`.  This pins the success path of the
    // INT_MIN gate: the bound IS recoverable when the surrounding
    // IR makes `idx` provably non-negative, matching the typical
    // compiler pattern of `cmp idx, 0; jl default` upstream of the
    // bounded compare.
    use strider_ir::IntBinaryOp;
    let idx_var = rsleigh::Vn {
        addr_off: 0x10,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let mut b = RegisterSet::new()
        .tracked(idx_var)
        .build_fn_single_region()
        .unwrap();
    let raw = b.read_variable(&idx_var).unwrap();
    // `raw & 0x7F` — clears the top bits including the sign bit.
    let mask = b.build_int_const(0x7Fu64, NodeOutputType::I32).unwrap();
    let idx = b
        .build_int_binary_operation(raw, mask, IntBinaryOp::And, NodeOutputType::I32)
        .unwrap();
    let n = b.build_int_const(8u64, NodeOutputType::I32).unwrap();
    let cmp = b
        .build_int_cmp_operation(idx, n, IntCmpOp::Sless, NodeOutputType::I32)
        .unwrap();
    b.build_indirect_branch(idx).unwrap();
    b.set_lift_addr(None);
    let function = b.build().unwrap();
    let bound = bound_from_if_condition(
        crate::pattern::RewriteCtxView::from_built(&function).unwrap(),
        cmp,
        idx,
        /* on_true */ true,
        &analyze_known_bits(crate::pattern::RewriteCtxView::from_built(&function).unwrap()).unwrap(),
    );
    assert_eq!(bound, Some(8), "Sless with proven idx>=0 yields bound = N");
}

#[test]
fn bound_from_if_condition_idx_le_n_true_is_n_plus_one() {
    // `idx <= 4` (taken-true) → bound = 5.  pcode-lift lowers
    // `IntLessEqual a, b` to `BoolNeg(IntLess(b, a))`, so the canonical
    // shape of "idx <= 4" in this IR is `BoolNeg(IntLess(IntConst(4), idx))`.
    // Build that shape directly here — the bound walker recognises it
    // and returns `4 + 1 = 5`.
    let mut builder = FunctionBuilder::empty().unwrap();
    let region = builder.create_region().unwrap();
    builder.set_entry_region(region).unwrap();
    builder.set_region(region);
    builder.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let idx = builder.build_int_const(0u64, NodeOutputType::I32).unwrap();
    let n = builder.build_int_const(4u64, NodeOutputType::I32).unwrap();
    // BoolNeg(IntLess(n, idx)) — operand order is (n, idx) per the
    // lift-time swap, mirroring `strider_lift::pcode_lift::handle_int_less_equal`.
    let inner = builder
        .build_int_cmp_operation(n, idx, IntCmpOp::Less, NodeOutputType::I32)
        .unwrap();
    let cmp = builder
        .build_int_unary_operation(inner, strider_ir::IntUnaryOp::BitNot, NodeOutputType::I1)
        .unwrap();
    builder.build_indirect_branch(idx).unwrap();
    builder.set_lift_addr(None);
    let function = builder.build().unwrap();
    let bound = bound_from_if_condition(crate::pattern::RewriteCtxView::from_built(&function).unwrap(), cmp, idx, true, &analyze_known_bits(crate::pattern::RewriteCtxView::from_built(&function).unwrap()).unwrap());
    assert_eq!(bound, Some(5));
}

/// Helper: build a graph where `entry` branches via
/// `if (idx < bound) { dispatch } else { exit }`, and the
/// dispatch region's placeholder Return uses an
/// `idx_in_dispatch` value (the dispatch's read of the same
/// idx_var, which travels through a single-input VarPhi).
/// Returns the graph, the anchor (placeholder Return's
/// value-input), and the dispatch's view of idx.
fn build_pred_if_graph(
    bound: u64,
) -> (Function, NodeOutputId, NodeOutputId) {
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
    let bound_c = b.build_int_const(bound, NodeOutputType::I32).unwrap();
    let cond = b
        .build_int_cmp_operation(idx_at_entry, bound_c, IntCmpOp::Less, NodeOutputType::I32)
        .unwrap();
    b.build_if(cond, dispatch, exit).unwrap();

    b.set_region(dispatch);
    let idx_in_dispatch = b.read_variable(&idx_var).unwrap();
    // Use idx_in_dispatch as the placeholder anchor — exercises
    // the bound walk against the dispatch's own idx-output, which
    // (without RedundantPhis) wraps the entry idx in a
    // single-input VarPhi.
    b.build_indirect_branch(idx_in_dispatch).unwrap();

    b.set_region(exit);
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);

    let function = b.build().unwrap();
    // The placeholder Return is the 3-input one in dispatch.
    let mut anchor = None;
    for nid in function.walk() {
        if !matches!(function.node_kind(nid), NodeKind::IndirectBranch) {
            continue;
        }
        let inputs: Vec<_> = function.node_inputs(nid).into_iter().collect();
        if inputs.len() == 3 {
            anchor = Some(inputs[2]);
        }
    }
    (function, anchor.expect("placeholder return"), idx_in_dispatch)
}

/// Stress test for the iterative `walk_control_for_if_bound`: a deep
/// chain of `If` nodes between Entry and the dispatch.  The recursive
/// version this replaces would burn stack frames per level; the
/// iterative worklist must complete on any depth.
///
/// The chain shape:
///   r0 (entry):   if (idx < TIGHT_BOUND) -> r1 else exit
///   r1:           if (idx < LOOSE_BOUND) -> r2 else exit
///   r2:           if (idx < LOOSE_BOUND) -> r3 else exit
///   ...
///   r{DEPTH-1}:   if (idx < LOOSE_BOUND) -> dispatch else exit
///   dispatch:     indirect_branch idx
///   exit:         return
///
/// `TIGHT_BOUND` is set on the outermost If; the rest use a strictly
/// looser bound.  The walk should crawl back through every region
/// and return `Some(LOOSE_BOUND)` (the first bound it discovers, on
/// the innermost If, since the walk goes innermost → outermost).
/// If the walk crashed, this test would never return.
#[test]
fn bound_via_predecessor_if_handles_deep_if_chain() {
    use strider_ir::IntCmpOp;
    // 50 is comfortably below `same_value`'s 64-step phi-walk budget
    // (each region introduces a single-input VarPhi between idx in
    // the If's condition and the dispatch's idx).  Past 64 the
    // budget runs out and the test would document the budget limit
    // rather than the iterative walk's depth-safety.
    const DEPTH: usize = 50;
    const TIGHT_BOUND: u64 = 4;
    const LOOSE_BOUND: u64 = 16;

    let idx_var = rsleigh::Vn {
        addr_off: 0x10,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let mut b = RegisterSet::new().tracked(idx_var).build_fn().unwrap();
    let mut regions = Vec::with_capacity(DEPTH + 2);
    for _ in 0..(DEPTH + 2) {
        regions.push(b.create_region().unwrap());
    }
    let entry = regions[0];
    let dispatch = regions[DEPTH];
    let exit = regions[DEPTH + 1];
    b.set_entry_region(entry).unwrap();
    for i in 0..DEPTH {
        b.set_region(regions[i]);
        let idx = b.read_variable(&idx_var).unwrap();
        let bound = if i == 0 { TIGHT_BOUND } else { LOOSE_BOUND };
        let bound_c = b.build_int_const(bound, NodeOutputType::I32).unwrap();
        let cond = b
            .build_int_cmp_operation(idx, bound_c, IntCmpOp::Less, NodeOutputType::I32)
            .unwrap();
        b.build_if(cond, regions[i + 1], exit).unwrap();
    }

    b.set_region(dispatch);
    let idx_in_dispatch = b.read_variable(&idx_var).unwrap();
    b.build_indirect_branch(idx_in_dispatch).unwrap();

    b.set_region(exit);
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);

    let function = b.build().unwrap();
    let mut anchor = None;
    for nid in function.walk() {
        if !matches!(function.node_kind(nid), NodeKind::IndirectBranch) {
            continue;
        }
        let inputs: Vec<_> = function.node_inputs(nid).into_iter().collect();
        if inputs.len() == 3 {
            anchor = Some(inputs[2]);
        }
    }
    let anchor = anchor.expect("placeholder anchor");
    // The walk hits the innermost If first (closest to dispatch),
    // whose bound is `LOOSE_BOUND` — and `bound_from_if_condition`
    // returns immediately, so it never crawls all the way back.
    let bound = bound_via_predecessor_if(crate::pattern::RewriteCtxView::from_built(&function).unwrap(), anchor, idx_in_dispatch, &analyze_known_bits(crate::pattern::RewriteCtxView::from_built(&function).unwrap()).unwrap());
    assert_eq!(bound, Some(LOOSE_BOUND));
}

#[test]
fn bound_via_predecessor_if_walks_one_hop() {
    // `If(idx < 4)` directly dominates the placeholder Return's
    // region.  bound_via_predecessor_if must follow control back
    // through one hop and surface bound = 4.
    let (g, anchor, idx_in_dispatch) = build_pred_if_graph(4);
    let bound = bound_via_predecessor_if(crate::pattern::RewriteCtxView::from_built(&g).unwrap(), anchor, idx_in_dispatch, &analyze_known_bits(crate::pattern::RewriteCtxView::from_built(&g).unwrap()).unwrap());
    assert_eq!(bound, Some(4));
}

#[test]
fn bound_via_predecessor_if_returns_none_when_no_if_on_path() {
    // No If on the path (single-region function with raw idx).
    // The walk reaches Entry without finding a bound → None.
    let idx_var = rsleigh::Vn {
        addr_off: 0x10,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let mut b = RegisterSet::new()
        .tracked(idx_var)
        .build_fn_single_region()
        .unwrap();
    let idx = b.read_variable(&idx_var).unwrap();
    b.build_indirect_branch(idx).unwrap();
    b.set_lift_addr(None);
    let function = b.build().unwrap();
    let mut anchor = None;
    for nid in function.walk() {
        if !matches!(function.node_kind(nid), NodeKind::IndirectBranch) {
            continue;
        }
        let inputs: Vec<_> = function.node_inputs(nid).into_iter().collect();
        if inputs.len() == 3 {
            anchor = Some(inputs[2]);
        }
    }
    let anchor = anchor.expect("anchor");
    let bound = bound_via_predecessor_if(crate::pattern::RewriteCtxView::from_built(&function).unwrap(), anchor, idx, &analyze_known_bits(crate::pattern::RewriteCtxView::from_built(&function).unwrap()).unwrap());
    assert_eq!(bound, None);
}

#[test]
fn bound_via_predecessor_if_returns_none_when_idx_unrelated_to_cond() {
    // The If's condition compares a DIFFERENT variable, not the
    // dispatch's idx.  The walk must NOT confabulate a bound.
    use strider_ir::IntCmpOp;
    let idx_var = rsleigh::Vn {
        addr_off: 0x10,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let other_var = rsleigh::Vn {
        addr_off: 0x14,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let mut b = RegisterSet::new()
        .tracked(idx_var)
        .tracked(other_var)
        .build_fn()
        .unwrap();
    let entry = b.create_region().unwrap();
    let dispatch = b.create_region().unwrap();
    let exit = b.create_region().unwrap();
    b.set_entry_region(entry).unwrap();

    b.set_region(entry);
    // Compare OTHER var, not idx.
    let other = b.read_variable(&other_var).unwrap();
    let bound_c = b.build_int_const(4u64, NodeOutputType::I32).unwrap();
    let cond = b
        .build_int_cmp_operation(other, bound_c, IntCmpOp::Less, NodeOutputType::I32)
        .unwrap();
    b.build_if(cond, dispatch, exit).unwrap();

    b.set_region(dispatch);
    let idx_in_dispatch = b.read_variable(&idx_var).unwrap();
    b.build_indirect_branch(idx_in_dispatch).unwrap();
    b.set_region(exit);
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);

    let function = b.build().unwrap();
    let mut anchor = None;
    for nid in function.walk() {
        if !matches!(function.node_kind(nid), NodeKind::IndirectBranch) {
            continue;
        }
        let inputs: Vec<_> = function.node_inputs(nid).into_iter().collect();
        if inputs.len() == 3 {
            anchor = Some(inputs[2]);
        }
    }
    let anchor = anchor.expect("anchor");
    let bound = bound_via_predecessor_if(crate::pattern::RewriteCtxView::from_built(&function).unwrap(), anchor, idx_in_dispatch, &analyze_known_bits(crate::pattern::RewriteCtxView::from_built(&function).unwrap()).unwrap());
    assert_eq!(bound, None, "If on unrelated var must not bound idx");
}

#[test]
fn bound_from_if_condition_idx_equal_n_true_returns_none() {
    // CORRECTNESS: `idx == N` taken-true constrains idx to
    // the single value `{N}`, not `[0, N]`.  The `0..bound`
    // enumeration shape this fn feeds into would mis-resolve, so
    // the helper *must* return None for `IntCmpOp::Equal` even on
    // the true branch.  Pin this here so any "let's tighten the
    // pattern" change surfaces as a unit-test failure.
    let mut builder = FunctionBuilder::empty().unwrap();
    let region = builder.create_region().unwrap();
    builder.set_entry_region(region).unwrap();
    builder.set_region(region);
    builder.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let idx = builder.build_int_const(0u64, NodeOutputType::I32).unwrap();
    let n = builder.build_int_const(4u64, NodeOutputType::I32).unwrap();
    let cmp = builder
        .build_int_cmp_operation(idx, n, IntCmpOp::Equal, NodeOutputType::I32)
        .unwrap();
    builder.build_indirect_branch(idx).unwrap();
    builder.set_lift_addr(None);
    let function = builder.build().unwrap();
    assert_eq!(
        bound_from_if_condition(crate::pattern::RewriteCtxView::from_built(&function).unwrap(), cmp, idx, /* on_true */ true, &analyze_known_bits(crate::pattern::RewriteCtxView::from_built(&function).unwrap()).unwrap()),
        None,
        "Equal must NOT yield a 0..N bound — see H2 fix",
    );
    // Same on the false branch (the negation idx != N — also no bound).
    assert_eq!(bound_from_if_condition(crate::pattern::RewriteCtxView::from_built(&function).unwrap(), cmp, idx, false, &analyze_known_bits(crate::pattern::RewriteCtxView::from_built(&function).unwrap()).unwrap()), None);
}

#[test]
fn bound_from_if_condition_with_n_on_lhs_does_not_match() {
    // `IntCmpOp::Less` is non-commutative; the pattern only binds
    // when `idx_var` is on the LHS.  Compilers occasionally emit
    // `IntCmp(N, idx)` (i.e. `N < idx`, equivalent to `idx > N`)
    // which on the *false* branch would imply `idx <= N`.  That
    // shape is currently NOT recognised — the pattern simply
    // doesn't bind and the helper returns None.  Pin it so a
    // future tightening (or refactor of `int_cmp_any`) surfaces
    // any behaviour change here.
    let mut builder = FunctionBuilder::empty().unwrap();
    let region = builder.create_region().unwrap();
    builder.set_entry_region(region).unwrap();
    builder.set_region(region);
    builder.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let idx = builder.build_int_const(0u64, NodeOutputType::I32).unwrap();
    let n = builder.build_int_const(4u64, NodeOutputType::I32).unwrap();
    // N on LHS, idx on RHS — `N < idx` shape.
    let cmp = builder
        .build_int_cmp_operation(n, idx, IntCmpOp::Less, NodeOutputType::I32)
        .unwrap();
    builder.build_indirect_branch(idx).unwrap();
    builder.set_lift_addr(None);
    let function = builder.build().unwrap();
    // True branch of `N < idx` ↔ `idx > N` — no upper bound (and
    // the pattern wouldn't bind to the desired `idx_var` anyway).
    assert_eq!(bound_from_if_condition(crate::pattern::RewriteCtxView::from_built(&function).unwrap(), cmp, idx, true, &analyze_known_bits(crate::pattern::RewriteCtxView::from_built(&function).unwrap()).unwrap()), None);
    // False branch of `N < idx` ↔ `idx <= N` — *would* be
    // soundly bounded by N+1 if the helper looked through the
    // swapped operands, but the current implementation returns
    // None.  Documented limitation.
    assert_eq!(bound_from_if_condition(crate::pattern::RewriteCtxView::from_built(&function).unwrap(), cmp, idx, false, &analyze_known_bits(crate::pattern::RewriteCtxView::from_built(&function).unwrap()).unwrap()), None);
}

#[test]
fn bound_from_if_condition_unrelated_idx_returns_none() {
    // The cmp is on `other`, not `idx`.  Must return None.
    let mut builder = FunctionBuilder::empty().unwrap();
    let region = builder.create_region().unwrap();
    builder.set_entry_region(region).unwrap();
    builder.set_region(region);
    builder.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let idx = builder.build_int_const(0u64, NodeOutputType::I32).unwrap();
    let other = builder.build_int_const(7u64, NodeOutputType::I32).unwrap();
    let n = builder.build_int_const(4u64, NodeOutputType::I32).unwrap();
    let cmp = builder
        .build_int_cmp_operation(other, n, IntCmpOp::Less, NodeOutputType::I32)
        .unwrap();
    builder.build_indirect_branch(idx).unwrap();
    builder.set_lift_addr(None);
    let function = builder.build().unwrap();
    let bound = bound_from_if_condition(crate::pattern::RewriteCtxView::from_built(&function).unwrap(), cmp, idx, true, &analyze_known_bits(crate::pattern::RewriteCtxView::from_built(&function).unwrap()).unwrap());
    assert_eq!(bound, None);
}

// ── Region multi-predecessor join behaviour ────────────────────────
//
// `walk_control_for_if_bound` takes the **max** over every predecessor's
// proved bound at a `Region` join.  If even one predecessor cannot
// prove a bound, the join's combined bound is `None` (fail closed).  The
// tests below pin both directions:
//
//   * positive: two `If`s on the same `idx` flow into a common dispatch
//     region.  Both prove `idx < N` for different N — combined = max(N).
//   * negative: only one path proves a bound, the other reaches Entry
//     unbounded — combined = None.
//
// Builds the diamond shape:
//
//        entry
//        /   \
//       v     v
//   path_a  path_b
//        \   /
//        v   v
//        dispatch
//
// where `path_a` is reached via `if (idx < bound_a)` (taken-true) and
// `path_b` via `if (idx < bound_b)` (taken-true).  Both paths
// `build_branch` to `dispatch`, giving `dispatch`'s `Region` two
// control inputs.

/// Build a diamond where both predecessors of the dispatch region prove
/// `idx < bound` via separate `If` nodes.  Returns the graph, the
/// placeholder anchor (Return.inputs[2]), and the dispatch's view of idx.
fn build_diamond_two_bounds(
    bound_a: u64,
    bound_b: u64,
) -> (Function, NodeOutputId, NodeOutputId) {
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

    // entry: split on a non-idx-related boolean so both arms proceed.
    // We use `idx == 0` as a dummy so both paths exist.
    b.set_region(entry);
    let idx_at_entry = b.read_variable(&idx_var).unwrap();
    let zero = b.build_int_const(0u64, NodeOutputType::I32).unwrap();
    let dummy = b
        .build_int_cmp_operation(idx_at_entry, zero, IntCmpOp::Equal, NodeOutputType::I32)
        .unwrap();
    b.build_if(dummy, path_a, path_b).unwrap();

    // path_a: `if (idx < bound_a) goto dispatch else goto exit_a`
    b.set_region(path_a);
    let idx_a = b.read_variable(&idx_var).unwrap();
    let bound_a_c = b.build_int_const(bound_a, NodeOutputType::I32).unwrap();
    let cond_a = b
        .build_int_cmp_operation(idx_a, bound_a_c, IntCmpOp::Less, NodeOutputType::I32)
        .unwrap();
    b.build_if(cond_a, dispatch, exit_a).unwrap();

    // path_b: `if (idx < bound_b) goto dispatch else goto exit_b`
    b.set_region(path_b);
    let idx_b = b.read_variable(&idx_var).unwrap();
    let bound_b_c = b.build_int_const(bound_b, NodeOutputType::I32).unwrap();
    let cond_b = b
        .build_int_cmp_operation(idx_b, bound_b_c, IntCmpOp::Less, NodeOutputType::I32)
        .unwrap();
    b.build_if(cond_b, dispatch, exit_b).unwrap();

    // dispatch: placeholder Return on idx.
    b.set_region(dispatch);
    let idx_in_dispatch = b.read_variable(&idx_var).unwrap();
    b.build_indirect_branch(idx_in_dispatch).unwrap();

    b.set_region(exit_a);
    b.build_return(None, &[]).unwrap();
    b.set_region(exit_b);
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);

    let function = b.build().unwrap();
    let mut anchor = None;
    for nid in function.walk() {
        if !matches!(function.node_kind(nid), NodeKind::IndirectBranch) {
            continue;
        }
        let inputs: Vec<_> = function.node_inputs(nid).into_iter().collect();
        if inputs.len() == 3 {
            anchor = Some(inputs[2]);
        }
    }
    (function, anchor.expect("placeholder return"), idx_in_dispatch)
}

#[test]
fn bound_via_predecessor_if_join_with_multi_input_phi_is_unbounded() {
    // Diamond: both predecessors *could* prove `idx < bound` against
    // the dispatch's own `idx` reading.  But the dispatch region
    // joins two control predecessors, so its `idx` read is a
    // VarPhi with two value inputs (one per path) — `same_value`
    // only walks through *trivial* (single-input) phis, so the
    // predecessor `If`s' `idx` LHS does not unify with the
    // dispatch's `idx_in_dispatch` and `bound_from_if_condition`
    // returns None for each path.  The join's combined bound is
    // therefore None.
    //
    // This pins a documented limitation of the predecessor-If walk:
    // it cannot prove a max-bound across a multi-input join unless a
    // later optimization pass (`RedundantPhis`) collapses the phi
    // first or `same_value` is taught to look through multi-value
    // phis.  See the `same_value` rationale in `jump_table.rs`.
    let (g, anchor, idx_in_dispatch) = build_diamond_two_bounds(4, 8);
    let bound = bound_via_predecessor_if(crate::pattern::RewriteCtxView::from_built(&g).unwrap(), anchor, idx_in_dispatch, &analyze_known_bits(crate::pattern::RewriteCtxView::from_built(&g).unwrap()).unwrap());
    assert_eq!(
        bound, None,
        "multi-input join phi blocks predecessor-If walk's bound proof",
    );
}

#[test]
fn bound_via_predecessor_if_join_fails_closed_when_one_path_unbounded() {
    // Diamond where path_a proves `idx < 4` but path_b sets up a
    // dummy If that doesn't bound idx — the walk reaches Entry on
    // path_b.  Per the documented contract: any unbounded
    // predecessor → join's bound = None.
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

    // entry: dummy split so both paths start.
    b.set_region(entry);
    let idx_e = b.read_variable(&idx_var).unwrap();
    let zero = b.build_int_const(0u64, NodeOutputType::I32).unwrap();
    let dummy = b
        .build_int_cmp_operation(idx_e, zero, IntCmpOp::Equal, NodeOutputType::I32)
        .unwrap();
    b.build_if(dummy, path_a, path_b).unwrap();

    // path_a: `if (idx < 4) goto dispatch else goto exit_a`
    b.set_region(path_a);
    let idx_a = b.read_variable(&idx_var).unwrap();
    let four = b.build_int_const(4u64, NodeOutputType::I32).unwrap();
    let cond_a = b
        .build_int_cmp_operation(idx_a, four, IntCmpOp::Less, NodeOutputType::I32)
        .unwrap();
    b.build_if(cond_a, dispatch, exit_a).unwrap();

    // path_b: unconditional branch to dispatch — no idx bound proved.
    b.set_region(path_b);
    b.build_branch(dispatch).unwrap();

    b.set_region(dispatch);
    let idx_in_dispatch = b.read_variable(&idx_var).unwrap();
    b.build_indirect_branch(idx_in_dispatch).unwrap();

    b.set_region(exit_a);
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);

    let function = b.build().unwrap();
    let mut anchor = None;
    for nid in function.walk() {
        if !matches!(function.node_kind(nid), NodeKind::IndirectBranch) {
            continue;
        }
        let inputs: Vec<_> = function.node_inputs(nid).into_iter().collect();
        if inputs.len() == 3 {
            anchor = Some(inputs[2]);
        }
    }
    let anchor = anchor.expect("anchor");
    let bound = bound_via_predecessor_if(crate::pattern::RewriteCtxView::from_built(&function).unwrap(), anchor, idx_in_dispatch, &analyze_known_bits(crate::pattern::RewriteCtxView::from_built(&function).unwrap()).unwrap());
    assert_eq!(
        bound, None,
        "any unbounded predecessor must collapse the join's bound to None",
    );
}
