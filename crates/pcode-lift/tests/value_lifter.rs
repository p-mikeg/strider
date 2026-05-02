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
use rsleigh::{Insn, Opcode, Vn, VnAddr, VnSpace};

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
    Vn { size: 4, addr: VnAddr { off, space: VnSpace::REGISTER } }
}

/// CONST-space varnode of byte width `size` carrying integer `val`.
fn const_vn(val: u64, size: u32) -> Vn {
    Vn { size, addr: VnAddr { off: val, space: VnSpace::CONST } }
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
        inputs: vec![const_vn(1, 1), const_vn(1, 1)],
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
        inputs: vec![const_vn(0, 1), const_vn(1, 1)],
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
        inputs: vec![const_vn(0, 1)],
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
        inputs: vec![const_vn(42, 4)],
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
        inputs: vec![const_vn(0xff, 1)],
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
        inputs: vec![const_vn(0xff, 1)],
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
        inputs: vec![const_vn(7, 4), const_vn(35, 4)],
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
        inputs: vec![const_vn(50, 4), const_vn(8, 4)],
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
        inputs: vec![const_vn(6, 4), const_vn(7, 4)],
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
        output: Some(Vn { size: 1, addr: VnAddr { off: 0, space: VnSpace::REGISTER } }),
        inputs: vec![const_vn(0x1234_5678, 4), const_vn(0, 4)],
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
        output: Some(Vn { size: 4, addr: VnAddr { off: 0, space: VnSpace::REGISTER } }),
        inputs: vec![const_vn(0xAA, 2), const_vn(0xBB, 2)],
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
        output: Some(Vn { size: 1, addr: VnAddr { off: 0, space: VnSpace::REGISTER } }),
        inputs: vec![const_vn(0xFF00, 4), const_vn(8, 4), const_vn(8, 4)],
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
        inputs: vec![const_vn(0b1011, 4)],
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
        inputs: vec![const_vn(0xF, 4)],
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
        inputs: vec![const_vn(0, 4), const_vn(0, 4)],
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
        inputs: vec![const_vn(0, 4)],
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
        inputs: vec![const_vn(0, 4), const_vn(0, 2), const_vn(0, 4)],
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
use ir::IntCmpOp;
use ir::node::NodeKind;

/// Returns true if the graph contains at least one node of `target` kind.
fn graph_has_kind(builder: &ir::FunctionBuilder, target: NodeKind) -> bool {
    let body = builder.body();
    body.graph.all_node_ids()
        .any(|id| body.graph.node_kind(id) == &target)
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
            inputs: vec![const_vn(5, 4), const_vn(7, 4)],
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
fn lift_int_sless_equal_lowers_to_boolneg_sless() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    {
        let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
        let insn = Insn {
            opcode: Opcode::IntSlessEqual,
            output: Some(reg(0)),
            inputs: vec![const_vn(5, 4), const_vn(7, 4)],
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
    let insn = Insn { opcode: Opcode::Branch, output: None, inputs: vec![] };
    assert!(!lifter.lift(&insn).unwrap());
}

#[test]
fn lift_returns_false_for_cond_branch() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn { opcode: Opcode::CondBranch, output: None, inputs: vec![] };
    assert!(!lifter.lift(&insn).unwrap());
}

#[test]
fn lift_returns_false_for_branch_indirect() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn { opcode: Opcode::BranchIndirect, output: None, inputs: vec![] };
    assert!(!lifter.lift(&insn).unwrap());
}

#[test]
fn lift_returns_false_for_return() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn { opcode: Opcode::Return, output: None, inputs: vec![] };
    assert!(!lifter.lift(&insn).unwrap());
}

#[test]
fn lift_returns_false_for_call() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn { opcode: Opcode::Call, output: None, inputs: vec![] };
    assert!(!lifter.lift(&insn).unwrap());
}

#[test]
fn lift_returns_false_for_call_indirect() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn { opcode: Opcode::CallIndirect, output: None, inputs: vec![] };
    assert!(!lifter.lift(&insn).unwrap());
}

#[test]
fn lift_returns_false_for_call_other() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn { opcode: Opcode::CallOther, output: None, inputs: vec![] };
    assert!(!lifter.lift(&insn).unwrap());
}

#[test]
fn lift_returns_false_for_store() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn { opcode: Opcode::Store, output: None, inputs: vec![] };
    assert!(!lifter.lift(&insn).unwrap());
}

#[test]
fn lift_returns_false_for_nop() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh, TEST_ENDIAN);
    let insn = Insn { opcode: Opcode::Nop, output: None, inputs: vec![] };
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
        output: Some(Vn { size: 1, addr: VnAddr { off: 0, space: VnSpace::REGISTER } }),
        // input is 4 bytes wide, byte_offset = 5 (> 4) ⇒ error.
        inputs: vec![const_vn(0, 4), const_vn(5, 4)],
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
        inputs: vec![const_vn(0, 4)],
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
    let insn = Insn { opcode: Opcode::CallOther, output: None, inputs: vec![] };
    assert!(!lifter.lift(&insn).unwrap());
}

