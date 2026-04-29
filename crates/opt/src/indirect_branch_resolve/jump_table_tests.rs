//! Unit tests for the jump-table classifier.
//!
//! Each test builds a minimal [`BuiltFunctionGraph`] via
//! [`ir::FunctionBuilder::new_raw`] (and `graph.create_node` for
//! shapes the validator otherwise rejects), then invokes the
//! piece-under-test in isolation.  Helpers are scoped to the
//! module rather than promoted to `tier2_helpers.rs` so the
//! unit tests stay self-contained.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

use super::*;
use ir::BuiltFunctionGraph;
use ir::FunctionBuilder;
use ir::node::NodeOutputType;
use std::sync::Mutex;

/// Toy `ReadOnlyMemory` impl that returns successive 4-byte
/// values at `base`, `base + stride`, `base + 2*stride`, …
/// according to a fixed table.  Reads outside the table return
/// None.  Used to exercise `read_table_entries` deterministically
/// and to drive the integration tests' rom setup.
pub struct TableRom {
    pub base: u64,
    pub stride: u64,
    pub entries: Vec<u64>,
    pub size: usize,
}

impl ReadOnlyMemory for TableRom {
    fn read(&self, _space: VnSpace, addr: u64, size: usize) -> Option<u64> {
        if size != self.size {
            return None;
        }
        if addr < self.base {
            return None;
        }
        let offset = addr - self.base;
        if self.stride == 0 {
            return None;
        }
        if !offset.is_multiple_of(self.stride) {
            return None;
        }
        let idx = (offset / self.stride) as usize;
        self.entries.get(idx).copied()
    }
}

/// `ReadOnlyMemory` impl that records every (addr,size) read it
/// services.  Used to assert `read_table_entries` issues exactly
/// `count` reads in index order.
pub struct RecordingRom {
    pub inner: TableRom,
    pub log: Mutex<Vec<(u64, usize)>>,
}

impl ReadOnlyMemory for RecordingRom {
    fn read(&self, space: VnSpace, addr: u64, size: usize) -> Option<u64> {
        self.log.lock().unwrap().push((addr, size));
        self.inner.read(space, addr, size)
    }
}

/// `ReadOnlyMemory` impl that reads `cutoff` entries successfully
/// then returns None for the rest.  Drives the partial-read
/// soundness test.
pub struct PartialRom {
    pub inner: TableRom,
    pub cutoff: usize,
}

impl ReadOnlyMemory for PartialRom {
    fn read(&self, space: VnSpace, addr: u64, size: usize) -> Option<u64> {
        if addr < self.inner.base {
            return None;
        }
        let offset = addr - self.inner.base;
        if self.inner.stride == 0 {
            return None;
        }
        let idx = (offset / self.inner.stride) as usize;
        if idx >= self.cutoff {
            return None;
        }
        self.inner.read(space, addr, size)
    }
}

/// Minimal `BuiltFunctionGraph` carrying nothing but the entry
/// region terminated by a placeholder `Return(anchor)`.  The
/// caller-supplied closure builds the anchor's producer subtree.
fn build_with_anchor(
    anchor_inputs: impl FnOnce(&mut FunctionBuilder) -> NodeOutputId,
) -> (BuiltFunctionGraph, NodeOutputId) {
    let mut builder = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)
        .expect("FunctionBuilder::new_raw");
    let region = builder.create_region().expect("create_region");
    builder.set_entry_region(region).expect("set_entry_region");
    builder.set_region(region);
    let anchor = anchor_inputs(&mut builder);
    builder.build_return(Some(anchor), &[]).expect("build_return");
    let graph = builder.build().expect("build");
    (graph, anchor)
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
) -> (BuiltFunctionGraph, NodeOutputId) {
    build_with_anchor(|fb| {
        let idx = idx_provider(fb);
        let stride_c = fb.build_int_const(stride, NodeOutputType::U32);
        let mul = if commute_mul {
            fb.build_int_binary_operation(stride_c, idx, IntBinaryOp::Mul, NodeOutputType::U32)
                .expect("mul")
        } else {
            fb.build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, NodeOutputType::U32)
                .expect("mul")
        };
        let base_c = fb.build_int_const(base, NodeOutputType::U32);
        let addr = if commute_add {
            fb.build_int_binary_operation(mul, base_c, IntBinaryOp::Add, NodeOutputType::U32)
                .expect("add")
        } else {
            fb.build_int_binary_operation(base_c, mul, IntBinaryOp::Add, NodeOutputType::U32)
                .expect("add")
        };
        fb.build_load(addr, VnSpace::RAM, NodeOutputType::U32)
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
    let addr = fb.build_int_const(0x9000u64, NodeOutputType::U32);
    fb.build_load(addr, VnSpace::RAM, NodeOutputType::U32)
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

#[test]
fn match_jump_table_shape_rejects_non_load_producer() {
    // Anchor is a raw IntConst, not a Load.  Reject.
    let (g, anchor) = build_with_anchor(|fb| fb.build_int_const(0x1000u64, NodeOutputType::U32));
    assert!(match_jump_table_shape(&g, anchor).is_none());
}

#[test]
fn match_jump_table_shape_rejects_load_with_unrelated_addr_shape() {
    // Load[IntConst(addr)] — a simple global read, no Add/Mul.
    // Our shape requires IntAdd at the top of the address tree.
    let (g, anchor) = build_with_anchor(|fb| {
        let addr = fb.build_int_const(0x1234u64, NodeOutputType::U32);
        fb.build_load(addr, VnSpace::RAM, NodeOutputType::U32).expect("load")
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
        let idx = fb.build_int_const(2u64, NodeOutputType::U32);
        let stride_c = fb.build_int_const(4u64, NodeOutputType::U32);
        let mul1 = fb
            .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, NodeOutputType::U32)
            .expect("mul1");
        let mul2 = fb
            .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, NodeOutputType::U32)
            .expect("mul2");
        let addr = fb
            .build_int_binary_operation(mul1, mul2, IntBinaryOp::Add, NodeOutputType::U32)
            .expect("add");
        fb.build_load(addr, VnSpace::RAM, NodeOutputType::U32).expect("load")
    });
    assert!(match_jump_table_shape(&g, anchor).is_none());
}

// ── Bound-via-known-bits tests ───────────────────────────────────────────

#[test]
fn bound_via_known_bits_returns_max_plus_one() {
    // idx = (some_var) & 0x7 → bound = 8.
    let (g, idx) = build_with_anchor(|fb| {
        let v = fb.build_int_const(0xffff_ffffu64, NodeOutputType::U32);
        let mask = fb.build_int_const(0x7u64, NodeOutputType::U32);
        fb.build_int_binary_operation(v, mask, IntBinaryOp::And, NodeOutputType::U32)
            .expect("and")
    });
    let bound = bound_via_known_bits(&g.graph, idx).expect("must bound");
    assert_eq!(bound, 8);
}

#[test]
fn bound_via_known_bits_returns_none_when_unbounded() {
    // idx = some unbounded U32 (a load output, no AND mask) → None.
    let (g, idx) = build_with_anchor(|fb| {
        let addr = fb.build_int_const(0x1000u64, NodeOutputType::U32);
        fb.build_load(addr, VnSpace::RAM, NodeOutputType::U32).expect("load")
    });
    assert_eq!(bound_via_known_bits(&g.graph, idx), None);
}

#[test]
fn bound_via_known_bits_with_int_const_input() {
    // idx = IntConst(5) directly.  KnownBits gives mask = 5,
    // bound = 6.  (Real graphs would have ConstantFold collapse
    // this to a Single, but the local recurrence handles it
    // anyway.)
    let (g, idx) = build_with_anchor(|fb| fb.build_int_const(5u64, NodeOutputType::U32));
    let bound = bound_via_known_bits(&g.graph, idx).expect("must bound a const");
    assert_eq!(bound, 6);
}

#[test]
fn bound_via_known_bits_handles_zero_extend() {
    // idx = ZeroExtend(u8 value).  Bound = 256 from the
    // narrower-type mask, regardless of the wider U32's full
    // range.  Build by hand via Graph::create_node because the
    // public `extend_if_needed` short-circuits constant inputs
    // to a folded IntConst, defeating the test's purpose.
    use ir::node::NodeOutputKind;
    let mut builder = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
    let region = builder.create_region().unwrap();
    builder.set_entry_region(region).unwrap();
    builder.set_region(region);
    // We need a non-IntConst U8 producer to feed into the Extend.
    // Use a U32 load truncated to U8 — both built via create_node
    // so we don't depend on builder's truncate-fold path.
    // Simpler: build a Load that produces U8.
    let addr = builder.build_int_const(0x9000u64, NodeOutputType::U32);
    let narrow = builder
        .build_load(addr, VnSpace::RAM, NodeOutputType::U8)
        .expect("u8 load");
    // Build the Extend node directly so it isn't folded.
    let mut g = builder.build().expect("build");
    let extend_node = g.graph.create_node(
        NodeKind::Extend(ir::ExtendOp::ZeroExtend),
        [narrow],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    let [idx] = g
        .graph
        .node_outputs_exact::<1>(extend_node)
        .expect("extend output");
    let bound = bound_via_known_bits(&g.graph, idx).expect("bound from zero-extend");
    // U8 narrows to 0..255, so bound = 256.
    assert_eq!(bound, 256);
}

// ── Read-table-entries tests ─────────────────────────────────────────────

#[test]
fn read_table_entries_returns_targets_in_index_order() {
    // 4 entries: 0x100, 0x200, 0x300, 0x400.  Stride 4, base
    // 0x4000.  Verify the returned vec preserves index order.
    let rom = TableRom {
        base: 0x4000,
        stride: 4,
        entries: vec![0x100, 0x200, 0x300, 0x400],
        size: 4,
    };
    let result = read_table_entries(&rom, 0x4000, 4, 4, 4).expect("must read all");
    assert_eq!(result, vec![0x100, 0x200, 0x300, 0x400]);
}

#[test]
fn read_table_entries_returns_none_on_partial_read() {
    // 4 entries requested; rom only serves the first 2.  Must
    // fail closed: returns None, NOT a Vec of length 2.
    let rom = PartialRom {
        inner: TableRom {
            base: 0x5000,
            stride: 4,
            entries: vec![0x100, 0x200, 0x300, 0x400],
            size: 4,
        },
        cutoff: 2,
    };
    assert_eq!(read_table_entries(&rom, 0x5000, 4, 4, 4), None);
}

#[test]
fn read_table_entries_issues_count_reads_in_index_order() {
    // RecordingRom logs every (addr, size) pair.  For 3 entries
    // at stride 4, base 0x6000, expect: (0x6000, 4), (0x6004, 4),
    // (0x6008, 4) in that order.
    let rom = RecordingRom {
        inner: TableRom {
            base: 0x6000,
            stride: 4,
            entries: vec![0xaaaa, 0xbbbb, 0xcccc],
            size: 4,
        },
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
        let raw = fb.build_int_const(0xffff_ffffu64, NodeOutputType::U32);
        let mask = fb.build_int_const(0x7u64, NodeOutputType::U32);
        let idx = fb
            .build_int_binary_operation(raw, mask, IntBinaryOp::And, NodeOutputType::U32)
            .expect("and");
        let stride_c = fb.build_int_const(4u64, NodeOutputType::U32);
        let mul = fb
            .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, NodeOutputType::U32)
            .expect("mul");
        let base_c = fb.build_int_const(0x4000u64, NodeOutputType::U32);
        let addr = fb
            .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, NodeOutputType::U32)
            .expect("add");
        fb.build_load(addr, VnSpace::RAM, NodeOutputType::U32)
            .expect("load")
    });
    let rom = TableRom {
        base: 0x4000,
        stride: 4,
        entries: vec![0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80],
        size: 4,
    };
    let result = classify_jump_table(&g, anchor, Some(&rom), None);
    match result {
        Some(BranchResolution::Multiple(ts)) => {
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
        let raw = fb.build_int_const(0xffff_ffffu64, NodeOutputType::U32);
        let mask = fb.build_int_const(0x3u64, NodeOutputType::U32);
        let idx = fb
            .build_int_binary_operation(raw, mask, IntBinaryOp::And, NodeOutputType::U32)
            .expect("and");
        let stride_c = fb.build_int_const(4u64, NodeOutputType::U32);
        let mul = fb
            .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, NodeOutputType::U32)
            .expect("mul");
        let base_c = fb.build_int_const(0x4000u64, NodeOutputType::U32);
        let addr = fb
            .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, NodeOutputType::U32)
            .expect("add");
        fb.build_load(addr, VnSpace::RAM, NodeOutputType::U32).expect("load")
    });
    let result = classify_jump_table(&g, anchor, None, None);
    assert_eq!(result, None);
}

#[test]
fn classify_jump_table_unbounded_idx_returns_none() {
    // Shape is jt-shaped, but `idx` is a raw load with no AND
    // mask; predecessor-If walk also can't bound it (no If on
    // the path).  Must return None, not a Multiple.
    let (g, anchor) = build_with_anchor(|fb| {
        let some_addr = fb.build_int_const(0x9000u64, NodeOutputType::U32);
        let idx = fb
            .build_load(some_addr, VnSpace::RAM, NodeOutputType::U32)
            .expect("load idx");
        let stride_c = fb.build_int_const(4u64, NodeOutputType::U32);
        let mul = fb
            .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, NodeOutputType::U32)
            .expect("mul");
        let base_c = fb.build_int_const(0x4000u64, NodeOutputType::U32);
        let addr = fb
            .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, NodeOutputType::U32)
            .expect("add");
        fb.build_load(addr, VnSpace::RAM, NodeOutputType::U32).expect("load")
    });
    let rom = TableRom {
        base: 0x4000,
        stride: 4,
        entries: vec![0x10, 0x20, 0x30, 0x40],
        size: 4,
    };
    let result = classify_jump_table(&g, anchor, Some(&rom), None);
    assert_eq!(result, None);
}

// ── bound_from_if_condition unit tests (direct) ─────────────────────────

#[test]
fn bound_from_if_condition_idx_less_than_n_true() {
    // Build idx and an `IntCmpOp::Less(idx, IntConst(4))`.  The
    // helper is on the `on_true` branch → bound = 4.
    let mut builder = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
    let region = builder.create_region().unwrap();
    builder.set_entry_region(region).unwrap();
    builder.set_region(region);
    let idx = builder.build_int_const(0u64, NodeOutputType::U32);
    let n = builder.build_int_const(4u64, NodeOutputType::U32);
    let cmp = builder
        .build_int_cmp_operation(idx, n, IntCmpOp::Less, NodeOutputType::U32)
        .unwrap();
    // Anchor with a placeholder return so build() succeeds.
    builder.build_return(Some(idx), &[]).unwrap();
    let g = builder.build().unwrap();
    let bound = bound_from_if_condition(&g.graph, cmp, idx, /* on_true */ true);
    assert_eq!(bound, Some(4));
}

#[test]
fn bound_from_if_condition_idx_less_than_n_false_returns_none() {
    // Same shape, but on the false branch → no upper bound.
    let mut builder = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
    let region = builder.create_region().unwrap();
    builder.set_entry_region(region).unwrap();
    builder.set_region(region);
    let idx = builder.build_int_const(0u64, NodeOutputType::U32);
    let n = builder.build_int_const(4u64, NodeOutputType::U32);
    let cmp = builder
        .build_int_cmp_operation(idx, n, IntCmpOp::Less, NodeOutputType::U32)
        .unwrap();
    builder.build_return(Some(idx), &[]).unwrap();
    let g = builder.build().unwrap();
    let bound = bound_from_if_condition(&g.graph, cmp, idx, /* on_true */ false);
    assert_eq!(bound, None);
}

#[test]
fn bound_from_if_condition_idx_le_n_true_is_n_plus_one() {
    // idx <= 4 (taken-true) → bound = 5.
    let mut builder = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
    let region = builder.create_region().unwrap();
    builder.set_entry_region(region).unwrap();
    builder.set_region(region);
    let idx = builder.build_int_const(0u64, NodeOutputType::U32);
    let n = builder.build_int_const(4u64, NodeOutputType::U32);
    let cmp = builder
        .build_int_cmp_operation(idx, n, IntCmpOp::LessEqual, NodeOutputType::U32)
        .unwrap();
    builder.build_return(Some(idx), &[]).unwrap();
    let g = builder.build().unwrap();
    let bound = bound_from_if_condition(&g.graph, cmp, idx, true);
    assert_eq!(bound, Some(5));
}

/// Helper: build a graph where `entry` branches via
/// `if (idx < bound) { dispatch } else { exit }`, and the
/// dispatch region's placeholder Return uses an
/// `idx_in_dispatch` value (the dispatch's read of the same
/// idx_var, which travels through a single-input ControlPhi).
/// Returns the graph, the anchor (placeholder Return's
/// value-input), and the dispatch's view of idx.
fn build_pred_if_graph(
    bound: u64,
) -> (BuiltFunctionGraph, NodeOutputId, NodeOutputId) {
    use ir::{FunctionBuilder, IntCmpOp};
    let idx_var = rsleigh::Vn {
        addr: rsleigh::VnAddr {
            space: rsleigh::VnSpace::REGISTER,
            off: 0x10,
        },
        size: 4,
    };
    let mut b = FunctionBuilder::new_raw(vec![idx_var], &[], &[], &[], None, 0).unwrap();
    let entry = b.create_region().unwrap();
    let dispatch = b.create_region().unwrap();
    let exit = b.create_region().unwrap();
    b.set_entry_region(entry).unwrap();

    b.set_region(entry);
    let idx_at_entry = b.read_variable(&idx_var).unwrap();
    let bound_c = b.build_int_const(bound, NodeOutputType::U32);
    let cond = b
        .build_int_cmp_operation(idx_at_entry, bound_c, IntCmpOp::Less, NodeOutputType::U32)
        .unwrap();
    b.build_if(cond, dispatch, exit).unwrap();

    b.set_region(dispatch);
    let idx_in_dispatch = b.read_variable(&idx_var).unwrap();
    // Use idx_in_dispatch as the placeholder anchor — exercises
    // the bound walk against the dispatch's own idx-output, which
    // (without RedundantPhis) wraps the entry idx in a
    // single-input ControlPhi.
    b.build_return(Some(idx_in_dispatch), &[]).unwrap();

    b.set_region(exit);
    b.build_return(None, &[]).unwrap();

    let g = b.build().unwrap();
    // The placeholder Return is the 3-input one in dispatch.
    let mut anchor = None;
    for nid in g.preorder() {
        if !matches!(g.graph.node_kind(nid), NodeKind::Return) {
            continue;
        }
        let inputs: Vec<_> = g.graph.node_inputs(nid).into_iter().collect();
        if inputs.len() == 3 {
            anchor = Some(inputs[2]);
        }
    }
    (g, anchor.expect("placeholder return"), idx_in_dispatch)
}

#[test]
fn bound_via_predecessor_if_walks_one_hop() {
    // `If(idx < 4)` directly dominates the placeholder Return's
    // region.  bound_via_predecessor_if must follow control back
    // through one hop and surface bound = 4.
    let (g, anchor, idx_in_dispatch) = build_pred_if_graph(4);
    let bound = bound_via_predecessor_if(&g.graph, anchor, idx_in_dispatch);
    assert_eq!(bound, Some(4));
}

#[test]
fn bound_via_predecessor_if_returns_none_when_no_if_on_path() {
    // No If on the path (single-region function with raw idx).
    // The walk reaches Entry without finding a bound → None.
    use ir::FunctionBuilder;
    let idx_var = rsleigh::Vn {
        addr: rsleigh::VnAddr {
            space: rsleigh::VnSpace::REGISTER,
            off: 0x10,
        },
        size: 4,
    };
    let mut b = FunctionBuilder::new_raw(vec![idx_var], &[], &[], &[], None, 0).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    let idx = b.read_variable(&idx_var).unwrap();
    b.build_return(Some(idx), &[]).unwrap();
    let g = b.build().unwrap();
    let mut anchor = None;
    for nid in g.preorder() {
        if !matches!(g.graph.node_kind(nid), NodeKind::Return) {
            continue;
        }
        let inputs: Vec<_> = g.graph.node_inputs(nid).into_iter().collect();
        if inputs.len() == 3 {
            anchor = Some(inputs[2]);
        }
    }
    let anchor = anchor.expect("anchor");
    let bound = bound_via_predecessor_if(&g.graph, anchor, idx);
    assert_eq!(bound, None);
}

#[test]
fn bound_via_predecessor_if_returns_none_when_idx_unrelated_to_cond() {
    // The If's condition compares a DIFFERENT variable, not the
    // dispatch's idx.  The walk must NOT confabulate a bound.
    use ir::{FunctionBuilder, IntCmpOp};
    let idx_var = rsleigh::Vn {
        addr: rsleigh::VnAddr {
            space: rsleigh::VnSpace::REGISTER,
            off: 0x10,
        },
        size: 4,
    };
    let other_var = rsleigh::Vn {
        addr: rsleigh::VnAddr {
            space: rsleigh::VnSpace::REGISTER,
            off: 0x14,
        },
        size: 4,
    };
    let mut b = FunctionBuilder::new_raw(vec![idx_var, other_var], &[], &[], &[], None, 0)
        .unwrap();
    let entry = b.create_region().unwrap();
    let dispatch = b.create_region().unwrap();
    let exit = b.create_region().unwrap();
    b.set_entry_region(entry).unwrap();

    b.set_region(entry);
    // Compare OTHER var, not idx.
    let other = b.read_variable(&other_var).unwrap();
    let bound_c = b.build_int_const(4u64, NodeOutputType::U32);
    let cond = b
        .build_int_cmp_operation(other, bound_c, IntCmpOp::Less, NodeOutputType::U32)
        .unwrap();
    b.build_if(cond, dispatch, exit).unwrap();

    b.set_region(dispatch);
    let idx_in_dispatch = b.read_variable(&idx_var).unwrap();
    b.build_return(Some(idx_in_dispatch), &[]).unwrap();
    b.set_region(exit);
    b.build_return(None, &[]).unwrap();

    let g = b.build().unwrap();
    let mut anchor = None;
    for nid in g.preorder() {
        if !matches!(g.graph.node_kind(nid), NodeKind::Return) {
            continue;
        }
        let inputs: Vec<_> = g.graph.node_inputs(nid).into_iter().collect();
        if inputs.len() == 3 {
            anchor = Some(inputs[2]);
        }
    }
    let anchor = anchor.expect("anchor");
    let bound = bound_via_predecessor_if(&g.graph, anchor, idx_in_dispatch);
    assert_eq!(bound, None, "If on unrelated var must not bound idx");
}

#[test]
fn bound_from_if_condition_unrelated_idx_returns_none() {
    // The cmp is on `other`, not `idx`.  Must return None.
    let mut builder = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
    let region = builder.create_region().unwrap();
    builder.set_entry_region(region).unwrap();
    builder.set_region(region);
    let idx = builder.build_int_const(0u64, NodeOutputType::U32);
    let other = builder.build_int_const(7u64, NodeOutputType::U32);
    let n = builder.build_int_const(4u64, NodeOutputType::U32);
    let cmp = builder
        .build_int_cmp_operation(other, n, IntCmpOp::Less, NodeOutputType::U32)
        .unwrap();
    builder.build_return(Some(idx), &[]).unwrap();
    let g = builder.build().unwrap();
    let bound = bound_from_if_condition(&g.graph, cmp, idx, true);
    assert_eq!(bound, None);
}
