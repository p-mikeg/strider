//! End-to-end tests for [`pcode_lift::ValueLifter::lift`].
//!
//! Each test:
//! 1. Builds a synthetic [`rsleigh::Sleigh`] (x86 — chosen for
//!    minimal-fuss register-table availability).
//! 2. Creates a [`ir::FunctionBuilder`] backed by a small set of
//!    register varnodes.
//! 3. Constructs a hand-crafted [`rsleigh::Insn`] and asks the
//!    `ValueLifter` to lift it.
//! 4. Inspects the resulting graph node(s) and / or the lift's
//!    `Ok(true|false)` signal.
//!
//! The opcodes used by these tests are typed by hand; no actual machine
//! code is decoded.  This lets us exercise every value-op family
//! without depending on the test fixtures.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use ir::FunctionBuilder;
use pcode_lift::ValueLifter;
use rsleigh::mem_readers::BufMemReader;
use rsleigh::{Insn, Opcode, Vn, VnSpace};

type TestReader = BufMemReader<Vec<u8>>;

/// Empty-buffer x86 sleigh — enough to query default address spaces.
fn make_sleigh() -> rsleigh::Sleigh<TestReader> {
    let reader = BufMemReader::new(Vec::<u8>::new(), 0x0);
    rsleigh::Sleigh::new(
        rsleigh::sla_spec::SLA_SPEC_X86,
        rsleigh::pspec::PSPEC_X86,
        reader,
    )
    .expect("failed to create test Sleigh")
}

/// 4-byte register at the given REGISTER-space offset.
fn reg(off: u64) -> Vn {
    Vn { size: 4, addr_off: off, addr_space: VnSpace::REGISTER }
}

/// CONST-space varnode of byte width `size` carrying integer `val`.
fn const_vn(val: u64, size: u32) -> Vn {
    Vn { size, addr_off: val, addr_space: VnSpace::CONST }
}

/// Builds a `FunctionBuilder` with three 4-byte REGISTER variables at
/// offsets 0, 4, 8.  Synthetic, no calling convention.
fn make_builder() -> FunctionBuilder {
    let vars = vec![reg(0), reg(4), reg(8)];
    let mut b = FunctionBuilder::new_raw(vars, &[], &[], &[], None, 0)
        .expect("FunctionBuilder::new_raw");
    b.build_entry().expect("build_entry");
    let region = b.create_region().expect("create_region");
    b.set_entry_region(region).expect("set_entry_region");
    b.set_region(region);
    b
}

/// Default endianness used by the ValueLifter constructor in these tests.
const TEST_ENDIAN: target::Endianness = target::Endianness::Little;

// ── Boolean family ──────────────────────────────────────────────────────────

#[test]
fn lift_bool_and_of_consts() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn {
        opcode: Opcode::BoolAnd,
        output: Some(reg(0)),
        inputs: vec![const_vn(1, 1), const_vn(1, 1)].into(),
    };
    assert!(lifter.lift(&insn).unwrap());
}

#[test]
fn lift_bool_or_of_consts() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn {
        opcode: Opcode::BoolOr,
        output: Some(reg(0)),
        inputs: vec![const_vn(0, 1), const_vn(1, 1)].into(),
    };
    assert!(lifter.lift(&insn).unwrap());
}

#[test]
fn lift_bool_neg_of_const() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn {
        opcode: Opcode::BoolNeg,
        output: Some(reg(0)),
        inputs: vec![const_vn(0, 1)].into(),
    };
    assert!(lifter.lift(&insn).unwrap());
}

// ── Integer family (Copy + Sext/Zext) ───────────────────────────────────────

#[test]
fn lift_int_copy_from_const() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn {
        opcode: Opcode::Copy,
        output: Some(reg(0)),
        inputs: vec![const_vn(42, 4)].into(),
    };
    assert!(lifter.lift(&insn).unwrap());
}

#[test]
fn lift_int_zext_extends_const() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn {
        opcode: Opcode::IntZext,
        output: Some(reg(0)),
        inputs: vec![const_vn(0xff, 1)].into(),
    };
    assert!(lifter.lift(&insn).unwrap());
}

#[test]
fn lift_int_sext_extends_const() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn {
        opcode: Opcode::IntSext,
        output: Some(reg(0)),
        inputs: vec![const_vn(0xff, 1)].into(),
    };
    assert!(lifter.lift(&insn).unwrap());
}

// ── Arithmetic family ───────────────────────────────────────────────────────

#[test]
fn lift_int_add_of_consts() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn {
        opcode: Opcode::IntAdd,
        output: Some(reg(0)),
        inputs: vec![const_vn(7, 4), const_vn(35, 4)].into(),
    };
    assert!(lifter.lift(&insn).unwrap());
}

#[test]
fn lift_int_sub_of_consts() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn {
        opcode: Opcode::IntSub,
        output: Some(reg(0)),
        inputs: vec![const_vn(50, 4), const_vn(8, 4)].into(),
    };
    assert!(lifter.lift(&insn).unwrap());
}

#[test]
fn lift_int_mul_of_consts() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn {
        opcode: Opcode::IntMul,
        output: Some(reg(0)),
        inputs: vec![const_vn(6, 4), const_vn(7, 4)].into(),
    };
    assert!(lifter.lift(&insn).unwrap());
}

// ── Cast family ─────────────────────────────────────────────────────────────

#[test]
fn lift_truncate_extracts_low_bits() {
    // Subpiece(value, byte_offset, out_size).
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn {
        opcode: Opcode::Subpiece,
        output: Some(Vn { size: 1, addr_off: 0, addr_space: VnSpace::REGISTER }),
        inputs: vec![const_vn(0x1234_5678, 4), const_vn(0, 4)].into(),
    };
    assert!(lifter.lift(&insn).unwrap());
}

#[test]
fn lift_piece_concatenates() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn {
        opcode: Opcode::Piece,
        output: Some(Vn { size: 4, addr_off: 0, addr_space: VnSpace::REGISTER }),
        inputs: vec![const_vn(0xAA, 2), const_vn(0xBB, 2)].into(),
    };
    assert!(lifter.lift(&insn).unwrap());
}

#[test]
fn lift_extract_returns_slice() {
    // Extract(value, lsb, bit_count).
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn {
        opcode: Opcode::Extract,
        output: Some(Vn { size: 1, addr_off: 0, addr_space: VnSpace::REGISTER }),
        inputs: vec![const_vn(0xFF00, 4), const_vn(8, 4), const_vn(8, 4)].into(),
    };
    assert!(lifter.lift(&insn).unwrap());
}

#[test]
fn lift_popcount() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn {
        opcode: Opcode::Popcount,
        output: Some(reg(0)),
        inputs: vec![const_vn(0b1011, 4)].into(),
    };
    assert!(lifter.lift(&insn).unwrap());
}

#[test]
fn lift_lzcount() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn {
        opcode: Opcode::Lzcount,
        output: Some(reg(0)),
        inputs: vec![const_vn(0xF, 4)].into(),
    };
    assert!(lifter.lift(&insn).unwrap());
}

// ── Float family ────────────────────────────────────────────────────────────

#[test]
fn lift_float_add_of_consts() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn {
        opcode: Opcode::FloatAdd,
        output: Some(reg(0)),
        // 4-byte (F32) varnodes — float-typed when read via read_vn,
        // but const space carries arbitrary bits.
        inputs: vec![const_vn(0, 4), const_vn(0, 4)].into(),
    };
    assert!(lifter.lift(&insn).unwrap());
}

#[test]
fn lift_float_neg() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn {
        opcode: Opcode::FloatNeg,
        output: Some(reg(0)),
        inputs: vec![const_vn(0, 4)].into(),
    };
    assert!(lifter.lift(&insn).unwrap());
}

// ── mem_load family ─────────────────────────────────────────────────────────

// `Load` is a recognised value-op — its dispatch arm exists in
// `value::lift`.  We don't end-to-end exercise it here: the
// `VnSpace::by_id` decode path expects an inputs[0] whose offset
// is the raw pointer to a Sleigh AddrSpace object, which a synthetic
// test cannot construct safely.  The strider per-arch tests cover
// the real-decoded Load paths.

// ── misc_value family ───────────────────────────────────────────────────────

#[test]
fn lift_segment_op_recognised() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn {
        opcode: Opcode::SegmentOp,
        output: Some(reg(0)),
        inputs: vec![const_vn(0, 4), const_vn(0, 2), const_vn(0, 4)].into(),
    };
    assert!(lifter.lift(&insn).unwrap());
}

// ── Lift-time canonicalisation shape checks ─────────────────────────────────
//
// `IntLessEqual` / `IntSlessEqual` are not primitives in this IR; they are
// lowered to `BoolNeg(IntLess(b, a))` / `BoolNeg(IntSless(b, a))` at lift
// time.  These tests assert the produced node shape so that any
// regression (e.g. accidental round-trip back to a `LessEqual` variant
// in some code path) fails immediately.

use ir::BoolUnaryOp;
use ir::IntBinaryOp;
use ir::IntCmpOp;
use ir::node::NodeKind;

/// Returns true if the graph contains at least one node of `target` kind.
fn graph_has_kind(builder: &ir::FunctionBuilder, target: NodeKind) -> bool {
    let body = builder.body();
    body.graph.all_node_ids()
        .any(|id| body.graph.node_kind(id) == &target)
}

/// Returns the first node-id in the graph matching `target`, or `None`.
fn find_first_node(builder: &ir::FunctionBuilder, target: NodeKind) -> Option<ir::node::NodeId> {
    let body = builder.body();
    body.graph
        .all_node_ids()
        .find(|id| body.graph.node_kind(*id) == &target)
}

#[test]
fn lift_with_set_lift_addr_records_asm_fingerprint() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    builder.set_lift_addr(Some(0x4242));
    {
        let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
        let insn = Insn {
            opcode: Opcode::IntAdd,
            output: Some(reg(0)),
            inputs: vec![const_vn(3, 4), const_vn(4, 4)].into(),
        };
        assert!(lifter.lift(&insn).unwrap());
    }
    let add_node = find_first_node(&builder, NodeKind::IntBinaryOp(IntBinaryOp::Add))
        .expect("IntAdd lift must produce an Add node");
    let fp = builder.body().graph.asm_fingerprint(add_node);
    assert_eq!(fp, &[0x4242], "Add node fingerprint should record 0x4242");
    // The two IntConst inputs should also carry the address.
    let const3 = find_first_node(&builder, NodeKind::IntConst(3))
        .expect("IntConst(3) must be present");
    let const4 = find_first_node(&builder, NodeKind::IntConst(4))
        .expect("IntConst(4) must be present");
    assert_eq!(builder.body().graph.asm_fingerprint(const3), &[0x4242]);
    assert_eq!(builder.body().graph.asm_fingerprint(const4), &[0x4242]);
}

#[test]
fn lift_without_lift_addr_leaves_fingerprint_empty() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    // Note: builder.set_lift_addr is NOT called.
    {
        let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
        let insn = Insn {
            opcode: Opcode::IntAdd,
            output: Some(reg(0)),
            inputs: vec![const_vn(3, 4), const_vn(4, 4)].into(),
        };
        assert!(lifter.lift(&insn).unwrap());
    }
    let add_node = find_first_node(&builder, NodeKind::IntBinaryOp(IntBinaryOp::Add))
        .expect("IntAdd lift must produce an Add node");
    assert!(
        builder.body().graph.asm_fingerprint(add_node).is_empty(),
        "Add fingerprint should be empty when no lift addr is set"
    );
}

#[test]
fn lift_dedup_unions_two_addresses() {
    // Same insn lifted twice from two different machine addresses; the
    // dedup cache returns the same NodeId; both contributors are unioned.
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let insn = Insn {
        opcode: Opcode::IntAdd,
        output: Some(reg(0)),
        inputs: vec![const_vn(3, 4), const_vn(4, 4)].into(),
    };
    builder.set_lift_addr(Some(0x1000));
    {
        let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
        assert!(lifter.lift(&insn).unwrap());
    }
    builder.set_lift_addr(Some(0x2000));
    {
        let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
        assert!(lifter.lift(&insn).unwrap());
    }
    let add_node = find_first_node(&builder, NodeKind::IntBinaryOp(IntBinaryOp::Add))
        .expect("Add must dedup to a single node");
    let fp = builder.body().graph.asm_fingerprint(add_node);
    assert_eq!(fp, &[0x1000, 0x2000], "both addresses should be unioned");
}

#[test]
fn lift_int_less_equal_lowers_to_boolneg_less() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    {
        let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
        let insn = Insn {
            opcode: Opcode::IntLessEqual,
            output: Some(reg(0)),
            inputs: vec![const_vn(5, 4), const_vn(7, 4)].into(),
        };
        assert!(lifter.lift(&insn).unwrap());
    }
    // Canonical shape: BoolUnaryOp::Neg over IntCmpOp::Less.
    assert!(
        graph_has_kind(&builder, NodeKind::BoolUnaryOp(BoolUnaryOp::Neg)),
        "expected BoolUnaryOp::Neg in graph (the lowering wrap)"
    );
    assert!(
        graph_has_kind(&builder, NodeKind::IntCmpOp(IntCmpOp::Less)),
        "expected IntCmpOp::Less in graph (the lowered cmp)"
    );
}

#[test]
fn lift_int_sub_lowers_to_add_neg() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    {
        let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
        let insn = Insn {
            opcode: Opcode::IntSub,
            output: Some(reg(0)),
            inputs: vec![const_vn(50, 4), const_vn(8, 4)].into(),
        };
        assert!(lifter.lift(&insn).unwrap());
    }
    // Canonical shape: IntBinaryOp::Add over (lhs, IntUnaryOp::Neg(rhs)).
    assert!(
        graph_has_kind(&builder, NodeKind::IntBinaryOp(ir::IntBinaryOp::Add)),
        "expected IntBinaryOp::Add in graph (the lowering wrap)"
    );
    assert!(
        graph_has_kind(&builder, NodeKind::IntUnaryOp(ir::IntUnaryOp::Neg)),
        "expected IntUnaryOp::Neg in graph (the negated rhs)"
    );
}

/// Two `IntSub` lifts with VARIABLE operands must dedupe via the IR's
/// node cache: the canonical lowered shape `Add(a, Neg(b))` is built
/// from cacheable node kinds, so the second lift reuses both the inner
/// `Neg(b)` node and the outer `Add` node.  Variable operands are the
/// strict case — constant-operand lifts dedupe trivially because the
/// `IntConst` keys match — so this test reads a register varnode for
/// both inputs.  Regression guard against the lowering accidentally
/// synthesising fresh non-cacheable nodes.
#[test]
fn lift_int_sub_caches_lowered_shape_variable_operands() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let count = |b: &ir::FunctionBuilder, target: NodeKind| -> usize {
        b.body()
            .graph
            .all_node_ids()
            .filter(|&id| b.body().graph.node_kind(id) == &target)
            .count()
    };
    {
        let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
        // IntSub reg(0), reg(4)  →  reg(8).  Variable inputs.
        let insn = Insn {
            opcode: Opcode::IntSub,
            output: Some(reg(8)),
            inputs: vec![reg(0), reg(4)].into(),
        };
        assert!(lifter.lift(&insn).unwrap());
    }
    let adds_after_first = count(&builder, NodeKind::IntBinaryOp(ir::IntBinaryOp::Add));
    let negs_after_first = count(&builder, NodeKind::IntUnaryOp(ir::IntUnaryOp::Neg));
    assert_eq!(adds_after_first, 1, "first IntSub lift must produce exactly one Add");
    assert_eq!(negs_after_first, 1, "first IntSub lift must produce exactly one Neg");
    {
        let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
        // Same inputs (reg(0), reg(4)), DIFFERENT output reg.  Cache must
        // dedupe the inner Neg(reg(4)) and outer Add(reg(0), Neg(reg(4))).
        let insn = Insn {
            opcode: Opcode::IntSub,
            output: Some(reg(0)),
            inputs: vec![reg(0), reg(4)].into(),
        };
        assert!(lifter.lift(&insn).unwrap());
    }
    let adds_after_second = count(&builder, NodeKind::IntBinaryOp(ir::IntBinaryOp::Add));
    let negs_after_second = count(&builder, NodeKind::IntUnaryOp(ir::IntUnaryOp::Neg));
    assert_eq!(
        adds_after_second, adds_after_first,
        "second IntSub lift with same operands must dedup the Add via the node cache"
    );
    assert_eq!(
        negs_after_second, negs_after_first,
        "second IntSub lift with same operands must dedup the Neg via the node cache"
    );
}

/// Companion to the variable-operand cache test: two const-operand lifts
/// must also dedupe.  Cheaper to detect cache-bypass regressions on the
/// happy path before they cause graph bloat in real binaries.
#[test]
fn lift_int_sub_caches_lowered_shape() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let count_subs_in_graph = |b: &ir::FunctionBuilder| -> usize {
        b.body()
            .graph
            .all_node_ids()
            .filter(|&id| matches!(b.body().graph.node_kind(id), NodeKind::IntBinaryOp(ir::IntBinaryOp::Add)))
            .count()
    };
    {
        let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
        let insn = Insn {
            opcode: Opcode::IntSub,
            output: Some(reg(0)),
            inputs: vec![const_vn(50, 4), const_vn(8, 4)].into(),
        };
        assert!(lifter.lift(&insn).unwrap());
    }
    let after_first = count_subs_in_graph(&builder);
    {
        let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
        // Same operands, different output reg — the value-producing nodes
        // should still dedupe through the cache.
        let insn = Insn {
            opcode: Opcode::IntSub,
            output: Some(reg(4)),
            inputs: vec![const_vn(50, 4), const_vn(8, 4)].into(),
        };
        assert!(lifter.lift(&insn).unwrap());
    }
    let after_second = count_subs_in_graph(&builder);
    assert_eq!(
        after_first, after_second,
        "second IntSub lift must dedupe the lowered Add+Neg shape via the node cache"
    );
}

#[test]
fn lift_int_sless_equal_lowers_to_boolneg_sless() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    {
        let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
        let insn = Insn {
            opcode: Opcode::IntSlessEqual,
            output: Some(reg(0)),
            inputs: vec![const_vn(5, 4), const_vn(7, 4)].into(),
        };
        assert!(lifter.lift(&insn).unwrap());
    }
    assert!(
        graph_has_kind(&builder, NodeKind::BoolUnaryOp(BoolUnaryOp::Neg)),
        "expected BoolUnaryOp::Neg in graph (the lowering wrap)"
    );
    assert!(
        graph_has_kind(&builder, NodeKind::IntCmpOp(IntCmpOp::Sless)),
        "expected IntCmpOp::Sless in graph (the lowered cmp)"
    );
}

// ── Rejected opcodes (caller-handled control flow / store) ──────────────────

#[test]
fn lift_returns_false_for_branch() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn { opcode: Opcode::Branch, output: None, inputs: Default::default() };
    assert!(!lifter.lift(&insn).unwrap());
}

#[test]
fn lift_returns_false_for_cond_branch() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn { opcode: Opcode::CondBranch, output: None, inputs: Default::default() };
    assert!(!lifter.lift(&insn).unwrap());
}

#[test]
fn lift_returns_false_for_branch_indirect() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn { opcode: Opcode::BranchIndirect, output: None, inputs: Default::default() };
    assert!(!lifter.lift(&insn).unwrap());
}

#[test]
fn lift_returns_false_for_return() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn { opcode: Opcode::Return, output: None, inputs: Default::default() };
    assert!(!lifter.lift(&insn).unwrap());
}

#[test]
fn lift_returns_false_for_call() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn { opcode: Opcode::Call, output: None, inputs: Default::default() };
    assert!(!lifter.lift(&insn).unwrap());
}

#[test]
fn lift_returns_false_for_call_indirect() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn { opcode: Opcode::CallIndirect, output: None, inputs: Default::default() };
    assert!(!lifter.lift(&insn).unwrap());
}

#[test]
fn lift_returns_false_for_call_other() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn { opcode: Opcode::CallOther, output: None, inputs: Default::default() };
    assert!(!lifter.lift(&insn).unwrap());
}

#[test]
fn lift_returns_false_for_store() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn { opcode: Opcode::Store, output: None, inputs: Default::default() };
    assert!(!lifter.lift(&insn).unwrap());
}

#[test]
fn lift_returns_false_for_nop() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn { opcode: Opcode::Nop, output: None, inputs: Default::default() };
    assert!(!lifter.lift(&insn).unwrap());
}

// ── vn_io tests ─────────────────────────────────────────────────────────────

#[test]
fn read_vn_unknown_returns_initial_var_or_phi() {
    // First read of an architectural register that's never been
    // written in this region should yield either an `InitialVar` (the
    // value at function entry) or a `VarPhi` (the SSA-style merge
    // node the FunctionBuilder lazily inserts at region entries
    // pointing back to the entry InitialVar).  Either is correct —
    // the producer is NOT some random arithmetic node.
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let value = lifter.read_vn(&reg(0)).expect("read_vn should succeed");
    let producer = lifter.builder.body().graph.get_node_from_output(value);
    let kind = lifter.builder.body().graph.node_kind(producer);
    assert!(
        matches!(
            kind,
            ir::node::NodeKind::InitialVar(_) | ir::node::NodeKind::VarPhi(_)
        ),
        "first read of an unwritten register should produce InitialVar or VarPhi, got {kind:?}"
    );
}

#[test]
fn write_vn_then_read_vn_round_trip() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    // Write 42 to reg(0).
    let const_42 = lifter.builder.build_int_const(42u64, ir::ValueType::U32).unwrap();
    lifter.write_vn(&reg(0), const_42).expect("write_vn");
    // Read it back.
    let value = lifter.read_vn(&reg(0)).expect("read_vn");
    let producer = lifter.builder.body().graph.get_node_from_output(value);
    let kind = lifter.builder.body().graph.node_kind(producer);
    match kind {
        ir::node::NodeKind::IntConst(n) => assert_eq!(*n, 42u128),
        other => panic!("expected IntConst(42), got {other:?}"),
    }
}

#[test]
fn write_vn_to_const_space_errors() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let val = lifter.builder.build_int_const(0u64, ir::ValueType::U32).unwrap();
    let res = lifter.write_vn(&const_vn(0, 4), val);
    assert!(res.is_err(), "writing to CONST space should error");
}

// ── Error paths ─────────────────────────────────────────────────────────────

#[test]
fn lift_subpiece_out_of_range_errors() {
    // byte_offset >= input.size  →  SubpieceOffsetOutOfRange.
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn {
        opcode: Opcode::Subpiece,
        output: Some(Vn { size: 1, addr_off: 0, addr_space: VnSpace::REGISTER }),
        // input is 4 bytes wide, byte_offset = 5 (> 4) ⇒ error.
        inputs: vec![const_vn(0, 4), const_vn(5, 4)].into(),
    };
    let res = lifter.lift(&insn);
    assert!(res.is_err(), "out-of-range Subpiece should error");
    if let Err(e) = res {
        assert!(e.to_string().contains("Subpiece byte_offset"), "got: {e}");
    }
}

#[test]
fn lift_missing_output_errors_for_op_that_needs_one() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn {
        opcode: Opcode::Copy,
        output: None,
        inputs: vec![const_vn(0, 4)].into(),
    };
    let res = lifter.lift(&insn);
    assert!(res.is_err(), "Copy without output_vn should error");
    if let Err(e) = res {
        assert!(e.to_string().contains("instruction has no output varnode"), "got: {e}");
    }
}

#[test]
fn lift_call_other_returns_false_via_value_lifter() {
    // CallOther stays in strider; the lifter never claims it.
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn { opcode: Opcode::CallOther, output: None, inputs: Default::default() };
    assert!(!lifter.lift(&insn).unwrap());
}

// ── Float lift-time canonicalisation shape checks ─────────────────────────────

#[test]
fn lift_float_sub_lowers_to_float_add_neg() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    {
        let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
        let insn = Insn {
            opcode: Opcode::FloatSub,
            output: Some(reg(0)),
            inputs: vec![const_vn(0, 4), const_vn(0, 4)].into(),
        };
        assert!(lifter.lift(&insn).unwrap());
    }
    assert!(
        graph_has_kind(&builder, NodeKind::FloatBinaryOp(ir::FloatBinaryOp::Add)),
        "FloatSub lift must produce a FloatAdd (the lowering wrap)"
    );
    assert!(
        graph_has_kind(&builder, NodeKind::FloatUnaryOp(ir::FloatUnaryOp::Neg)),
        "FloatSub lift must produce a FloatUnaryOp::Neg (the negated rhs)"
    );
}

#[test]
fn lift_float_not_equal_lowers_to_boolneg_float_equal() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    {
        let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
        let insn = Insn {
            opcode: Opcode::FloatNotEqual,
            output: Some(reg(0)),
            inputs: vec![const_vn(0, 4), const_vn(0, 4)].into(),
        };
        assert!(lifter.lift(&insn).unwrap());
    }
    assert!(
        graph_has_kind(&builder, NodeKind::BoolUnaryOp(BoolUnaryOp::Neg)),
        "FloatNotEqual lift must produce a BoolUnaryOp::Neg (the lowering wrap)"
    );
    assert!(
        graph_has_kind(&builder, NodeKind::FloatCmpOp(ir::FloatCmpOp::Equal)),
        "FloatNotEqual lift must produce a FloatCmpOp::Equal (the lowered cmp)"
    );
}

#[test]
fn lift_float_less_equal_lowers_to_or_less_equal() {
    // `a <= b` (IEEE 754) lowers to `Or(Less(a, b), Equal(a, b))`,
    // NaN-aware (both children false on NaN, so Or is false).
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    {
        let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
        let insn = Insn {
            opcode: Opcode::FloatLessEqual,
            output: Some(reg(0)),
            inputs: vec![const_vn(0, 4), const_vn(0, 4)].into(),
        };
        assert!(lifter.lift(&insn).unwrap());
    }
    assert!(
        graph_has_kind(&builder, NodeKind::BoolBinaryOp(ir::BoolBinaryOp::Or)),
        "FloatLessEqual lift must produce a BoolBinaryOp::Or (the disjunction wrap)"
    );
    assert!(
        graph_has_kind(&builder, NodeKind::FloatCmpOp(ir::FloatCmpOp::Less)),
        "FloatLessEqual lift must produce a FloatCmpOp::Less"
    );
    assert!(
        graph_has_kind(&builder, NodeKind::FloatCmpOp(ir::FloatCmpOp::Equal)),
        "FloatLessEqual lift must produce a FloatCmpOp::Equal"
    );
}


#[test]
fn handle_int_sub_rejects_width_mismatch() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    // Mismatched widths: lhs is 4-byte (REGISTER), rhs is 2-byte CONST,
    // out is 4-byte.  Sleigh's IntSub requires equal widths; the
    // lifter must surface this as Err rather than silently coerce.
    let insn = Insn {
        opcode: Opcode::IntSub,
        output: Some(reg(0)),
        inputs: vec![reg(4), const_vn(1, 2)].into(),
    };
    let res = lifter.lift(&insn);
    assert!(
        res.is_err(),
        "IntSub with mismatched widths must Err, got {res:?}"
    );
    let msg = res.unwrap_err().to_string();
    assert!(
        msg.contains("IntSub width mismatch"),
        "error message should name the invariant; got {msg}"
    );
}

#[test]
fn int_sub_lowers_to_add_with_inner_neg() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn {
        opcode: Opcode::IntSub,
        output: Some(reg(0)),
        inputs: vec![reg(4), reg(8)].into(),
    };
    assert!(lifter.lift(&insn).unwrap());
    // Lift-time canonicalisation contract: no `IntBinaryOp::Sub` survives;
    // every Sub is rewritten as Add(_, Neg(_)).
    use ir::node::NodeKind;
    let saw_neg = graph_has_kind(&builder, NodeKind::IntUnaryOp(ir::IntUnaryOp::Neg));
    let saw_add = graph_has_kind(&builder, NodeKind::IntBinaryOp(ir::IntBinaryOp::Add));
    assert!(
        saw_neg && saw_add,
        "IntSub lift must produce both an inner Neg and an outer Add"
    );
}

// Note: T-30 (IntLessEqual lowering shape) is covered by the existing
// `lift_int_less_equal_lowers_to_boolneg_less` test earlier in this file.
