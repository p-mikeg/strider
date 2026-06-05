//! End-to-end tests for [`pcode_lift::ValueLifter::lift`].
//!
//! Each test:
//! 1. Builds a synthetic [`rsleigh::Sleigh`] (x86 — chosen for
//!    minimal-fuss register-table availability).
//! 2. Creates a [`strider_ir::FunctionBuilder`] backed by a small set
//!    of register varnodes.
//! 3. Constructs a hand-crafted [`rsleigh::Insn`] and asks the
//!    `ValueLifter` to lift it.
//! 4. Inspects the resulting graph node(s) and / or the lift's
//!    `Ok(true|false)` signal.
//!
//! The opcodes used by these tests are typed by hand; no actual machine
//! code is decoded.  This lets us exercise every value-op family
//! without depending on the test fixtures.
//!
//! Ported from the pre-rewrite `crates/pcode-lift/tests/value_lifter.rs`
//! (deleted in the rename / restructure to `strider-lift`).  The tests
//! pin per-opcode behaviour — especially the lift-time canonicalisations
//! that the pattern DSL and downstream passes rely on:
//!
//! * `IntSub(a, b)` → `Add(a, IntUnaryOp::Neg(b))`
//! * `IntLessEqual(a, b)` → `BoolNeg(IntLess(b, a))` (args swapped)
//! * `IntSlessEqual(a, b)` → `BoolNeg(IntSless(b, a))`
//! * `FloatSub(a, b)` → `FloatAdd(a, FloatUnaryOp::Neg(b))`
//! * `FloatNotEqual(a, b)` → `BoolNeg(FloatEqual(a, b))`
//! * `FloatLessEqual(a, b)` → `Or(FloatLess(a, b), FloatEqual(a, b))`
//!   (NaN-aware)

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use strider_ir::IRBuilderExt;
use strider_ir::IRViewer;
use rsleigh::mem_readers::BufMemReader;
use rsleigh::{Insn, Opcode, Vn, VnSpace};
use strider_ir::node::{NodeId, NodeKind};
use strider_ir::{FunctionBuilder, IntBinaryOp, IntCmpOp, IntUnaryOp};
use strider_lift::pcode_lift::ValueLifter;

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

/// Default endianness threaded into the builder in these tests.
const TEST_ENDIAN: strider_target::Endianness = strider_target::Endianness::Little;

/// Builds a `FunctionBuilder` with three 4-byte REGISTER variables at
/// offsets 0, 4, 8.  Synthetic, no calling convention.
fn make_builder() -> FunctionBuilder {
    let vars = vec![reg(0), reg(4), reg(8)];
    let mut b = strider_ir_test_utils::builder(vars, &[], &[], &[], None, 0, TEST_ENDIAN)
        .expect("FunctionBuilder::new");
    // The test-utils helper stamps the sentinel lift address; clear it so
    // these tests start from a clean `lift_addr = None` and set their own
    // per-insn address where they assert on fingerprints.
    b.set_lift_addr(None);
    b.build_entry().expect("build_entry");
    let region = b.create_region().expect("create_region");
    b.set_entry_region(region).expect("set_entry_region");
    b.set_region(region);
    b
}

/// Shared scaffold for the smoke-test shape "build sleigh + builder +
/// lifter, construct an `Insn { opcode, output, inputs }`, assert the
/// lift returns `Ok(true)`".  Migrating each smoke test to this
/// helper collapses 6 lines of setup ritual to a single line and
/// surfaces the per-test variance (the opcode + i/o varnodes) at the
/// call site.  Tests that need to inspect the resulting graph keep
/// their hand-written setup.
fn assert_lifts_one(opcode: Opcode, output: Option<Vn>, inputs: Vec<Vn>) {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh);
    let insn = Insn {
        opcode,
        output,
        inputs: inputs.into(),
    };
    assert!(lifter.lift(&insn).unwrap(),
        "lift returned Ok(false) for {opcode:?} — expected Ok(true)");
}

// ── Boolean family ──────────────────────────────────────────────────────────

#[test]
fn lift_bool_and_of_consts() {
    assert_lifts_one(Opcode::BoolAnd, Some(reg(0)), vec![const_vn(1, 1), const_vn(1, 1)]);
}

#[test]
fn lift_bool_or_of_consts() {
    assert_lifts_one(Opcode::BoolOr, Some(reg(0)), vec![const_vn(0, 1), const_vn(1, 1)]);
}

#[test]
fn lift_bool_neg_of_const() {
    assert_lifts_one(Opcode::BoolNeg, Some(reg(0)), vec![const_vn(0, 1)]);
}

// ── Integer family (Copy + Sext/Zext) ───────────────────────────────────────

#[test]
fn lift_int_copy_from_const() {
    assert_lifts_one(Opcode::Copy, Some(reg(0)), vec![const_vn(42, 4)]);
}

#[test]
fn lift_int_zext_extends_const() {
    assert_lifts_one(Opcode::IntZext, Some(reg(0)), vec![const_vn(0xff, 1)]);
}

#[test]
fn lift_int_sext_extends_const() {
    assert_lifts_one(Opcode::IntSext, Some(reg(0)), vec![const_vn(0xff, 1)]);
}

// ── Arithmetic family ───────────────────────────────────────────────────────

#[test]
fn lift_int_add_of_consts() {
    assert_lifts_one(Opcode::IntAdd, Some(reg(0)), vec![const_vn(7, 4), const_vn(35, 4)]);
}

#[test]
fn lift_int_sub_of_consts() {
    assert_lifts_one(Opcode::IntSub, Some(reg(0)), vec![const_vn(50, 4), const_vn(8, 4)]);
}

#[test]
fn lift_int_mul_of_consts() {
    assert_lifts_one(Opcode::IntMul, Some(reg(0)), vec![const_vn(6, 4), const_vn(7, 4)]);
}

// ── Cast family ─────────────────────────────────────────────────────────────

#[test]
fn lift_truncate_extracts_low_bits() {
    // Subpiece(value, byte_offset, out_size).
    assert_lifts_one(Opcode::Subpiece, Some(Vn { size: 1, addr_off: 0, addr_space: VnSpace::REGISTER }), vec![const_vn(0x1234_5678, 4), const_vn(0, 4)]);
}

#[test]
fn lift_piece_concatenates() {
    assert_lifts_one(Opcode::Piece, Some(Vn { size: 4, addr_off: 0, addr_space: VnSpace::REGISTER }), vec![const_vn(0xAA, 2), const_vn(0xBB, 2)]);
}

#[test]
fn lift_extract_returns_slice() {
    // Extract(value, lsb, bit_count).
    assert_lifts_one(Opcode::Extract, Some(Vn { size: 1, addr_off: 0, addr_space: VnSpace::REGISTER }), vec![const_vn(0xFF00, 4), const_vn(8, 4), const_vn(8, 4)]);
}

#[test]
fn lift_insert_field_past_destination_width_errors() {
    // Insert(dest, src, lsb=24, bit_count=16) into a 4-byte (32-bit) dest:
    // 24 + 16 = 40 > 32, so the field does not fit.  Must surface a typed
    // error rather than silently produce wrong bits (the host-side
    // wrapping_shl mask and the IR ShiftLeft diverge past the width).
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh);
    let insn = Insn {
        opcode: Opcode::Insert,
        output: Some(reg(0)),
        inputs: vec![reg(0), reg(8), const_vn(24, 4), const_vn(16, 4)].into(),
    };
    assert!(
        lifter.lift(&insn).is_err(),
        "Insert field exceeding destination width must error"
    );
}

#[test]
fn lift_extract_field_past_input_width_errors() {
    // Extract(value, lsb=28, bit_count=8) from a 4-byte (32-bit) input:
    // 28 + 8 = 36 > 32 — the slice runs past the input.  Must error.
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh);
    let insn = Insn {
        opcode: Opcode::Extract,
        output: Some(Vn { size: 1, addr_off: 0, addr_space: VnSpace::REGISTER }),
        inputs: vec![const_vn(0xFFFF_FFFF, 4), const_vn(28, 4), const_vn(8, 4)].into(),
    };
    assert!(
        lifter.lift(&insn).is_err(),
        "Extract slice exceeding input width must error"
    );
}

#[test]
fn lift_popcount() {
    assert_lifts_one(Opcode::Popcount, Some(reg(0)), vec![const_vn(0b1011, 4)]);
}

#[test]
fn lift_lzcount() {
    assert_lifts_one(Opcode::Lzcount, Some(reg(0)), vec![const_vn(0xF, 4)]);
}

// ── Float family ────────────────────────────────────────────────────────────

#[test]
fn lift_float_add_of_consts() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh);
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
    assert_lifts_one(Opcode::FloatNeg, Some(reg(0)), vec![const_vn(0, 4)]);
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
    assert_lifts_one(Opcode::SegmentOp, Some(reg(0)), vec![const_vn(0, 4), const_vn(0, 2), const_vn(0, 4)]);
}

// ── Lift-time canonicalisation shape checks ─────────────────────────────────
//
// `IntLessEqual` / `IntSlessEqual` are not primitives in this IR; they are
// lowered to `BoolNeg(IntLess(b, a))` / `BoolNeg(IntSless(b, a))` at lift
// time.  These tests assert the produced node shape so that any
// regression (e.g. accidental round-trip back to a `LessEqual` variant
// in some code path) fails immediately.

/// Returns true if the graph contains at least one node of `target` kind.
fn graph_has_kind(builder: &FunctionBuilder, target: NodeKind) -> bool {
    builder
        .function()
        .graph().all_node_ids()
        .any(|id| builder.function().node_kind(id) == &target)
}

/// Returns the first node-id in the graph matching `target`, or `None`.
fn find_first_node(builder: &FunctionBuilder, target: NodeKind) -> Option<NodeId> {
    builder
        .function()
        .graph().all_node_ids()
        .find(|id| builder.function().node_kind(*id) == &target)
}

#[test]
fn signed_binary_op_sign_extends_narrower_operand() {
    // IntSdiv with a 2-byte dividend (0xFFFF = -1) and a 4-byte output.
    // A signed op must SIGN-extend the narrower operand to the op width
    // (0xFFFF -> 0xFFFF_FFFF), not zero-extend it (-> 0x0000_FFFF).  Under
    // the prior build_int_binary_operation zero-extension the 32-bit value
    // 0xFFFF_FFFF never appeared.
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    {
        let mut lifter = ValueLifter::new(&mut builder, &sleigh);
        let insn = Insn {
            opcode: Opcode::IntSdiv,
            output: Some(reg(0)),
            inputs: vec![const_vn(0xFFFF, 2), const_vn(2, 4)].into(),
        };
        assert!(lifter.lift(&insn).unwrap());
    }
    assert!(
        graph_has_kind(&builder, NodeKind::IntBinaryOp(IntBinaryOp::Sdiv)),
        "expected an Sdiv node"
    );
    assert!(
        graph_has_kind(&builder, NodeKind::IntConst(0xFFFF_FFFF)),
        "the 2-byte -1 dividend must be SIGN-extended to the 4-byte op width (0xFFFF_FFFF)"
    );
}

#[test]
fn int_signed_cmp_uses_max_width_and_sign_extends_narrower_operand() {
    // IntSless of a 4-byte operand (0xFFFFFFFF = -1) and an 8-byte operand.
    // Compare at the MAX of the two widths (8 bytes) so the wider operand is
    // never truncated, and SIGN-extend the narrower *signed* operand so -1
    // stays -1 (0xFFFF_FFFF_FFFF_FFFF), not the zero-extended
    // 0x0000_0000_FFFF_FFFF.  Under the old inputs[0]-width behavior the
    // 8-byte operand was truncated to 4 bytes and this 64-bit sign-extended
    // constant never appeared.
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    {
        let mut lifter = ValueLifter::new(&mut builder, &sleigh);
        let insn = Insn {
            opcode: Opcode::IntSless,
            output: Some(reg(0)),
            inputs: vec![const_vn(0xFFFF_FFFF, 4), const_vn(5, 8)].into(),
        };
        assert!(lifter.lift(&insn).unwrap());
    }
    assert!(
        graph_has_kind(&builder, NodeKind::IntCmpOp(IntCmpOp::Sless)),
        "expected an Sless comparison node"
    );
    assert!(
        graph_has_kind(&builder, NodeKind::IntConst(0xFFFF_FFFF_FFFF_FFFF)),
        "the 4-byte -1 operand must be SIGN-extended to the 8-byte max width \
         (0xFFFF_FFFF_FFFF_FFFF) — proving max-width comparison + sign-correct extension"
    );
}

#[test]
fn lift_with_set_lift_addr_records_asm_fingerprint() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    builder.set_lift_addr(Some(0x4242));
    {
        let mut lifter = ValueLifter::new(&mut builder, &sleigh);
        let insn = Insn {
            opcode: Opcode::IntAdd,
            output: Some(reg(0)),
            inputs: vec![const_vn(3, 4), const_vn(4, 4)].into(),
        };
        assert!(lifter.lift(&insn).unwrap());
    }
    let add_node = find_first_node(&builder, NodeKind::IntBinaryOp(IntBinaryOp::Add))
        .expect("IntAdd lift must produce an Add node");
    let fp = builder.function().asm_fingerprint(add_node);
    assert_eq!(fp, &[0x4242], "Add node fingerprint should record 0x4242");
    // The two IntConst inputs should also carry the address.
    let const3 = find_first_node(&builder, NodeKind::IntConst(3))
        .expect("IntConst(3) must be present");
    let const4 = find_first_node(&builder, NodeKind::IntConst(4))
        .expect("IntConst(4) must be present");
    assert_eq!(builder.function().asm_fingerprint(const3), &[0x4242]);
    assert_eq!(builder.function().asm_fingerprint(const4), &[0x4242]);
}

#[test]
fn lift_without_lift_addr_leaves_fingerprint_empty() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    // Note: builder.set_lift_addr is NOT called.
    {
        let mut lifter = ValueLifter::new(&mut builder, &sleigh);
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
        builder.function().asm_fingerprint(add_node).is_empty(),
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
        let mut lifter = ValueLifter::new(&mut builder, &sleigh);
        assert!(lifter.lift(&insn).unwrap());
    }
    builder.set_lift_addr(Some(0x2000));
    {
        let mut lifter = ValueLifter::new(&mut builder, &sleigh);
        assert!(lifter.lift(&insn).unwrap());
    }
    let add_node = find_first_node(&builder, NodeKind::IntBinaryOp(IntBinaryOp::Add))
        .expect("Add must dedup to a single node");
    let fp = builder.function().asm_fingerprint(add_node);
    assert_eq!(fp, &[0x1000, 0x2000], "both addresses should be unioned");
}

#[test]
fn lift_int_less_equal_lowers_to_boolneg_less() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    {
        let mut lifter = ValueLifter::new(&mut builder, &sleigh);
        let insn = Insn {
            opcode: Opcode::IntLessEqual,
            output: Some(reg(0)),
            inputs: vec![const_vn(5, 4), const_vn(7, 4)].into(),
        };
        assert!(lifter.lift(&insn).unwrap());
    }
    // Canonical shape: `Xor(IntLess(_, _), IntConst(1)):I1` (a 1-bit
    // logical NOT — the former BitNot unary-op was removed in favour of the
    // Xor-with-all-ones shape).  Pin the I1 Xor and the IntCmpOp::Less.
    assert!(
        graph_has_kind(&builder, NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::Xor)),
        "expected IntBinaryOp::Xor in graph (the I1 logical-NOT wrap)"
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
        let mut lifter = ValueLifter::new(&mut builder, &sleigh);
        let insn = Insn {
            opcode: Opcode::IntSub,
            output: Some(reg(0)),
            inputs: vec![const_vn(50, 4), const_vn(8, 4)].into(),
        };
        assert!(lifter.lift(&insn).unwrap());
    }
    // Canonical shape: IntBinaryOp::Add over (lhs, IntUnaryOp::Neg(rhs)).
    assert!(
        graph_has_kind(&builder, NodeKind::IntBinaryOp(IntBinaryOp::Add)),
        "expected IntBinaryOp::Add in graph (the lowering wrap)"
    );
    assert!(
        graph_has_kind(&builder, NodeKind::IntUnaryOp(IntUnaryOp::Neg)),
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
    let count = |b: &FunctionBuilder, target: NodeKind| -> usize {
        b.function()
            .graph().all_node_ids()
            .filter(|&id| b.function().node_kind(id) == &target)
            .count()
    };
    {
        let mut lifter = ValueLifter::new(&mut builder, &sleigh);
        // IntSub reg(0), reg(4)  →  reg(8).  Variable inputs.
        let insn = Insn {
            opcode: Opcode::IntSub,
            output: Some(reg(8)),
            inputs: vec![reg(0), reg(4)].into(),
        };
        assert!(lifter.lift(&insn).unwrap());
    }
    let adds_after_first = count(&builder, NodeKind::IntBinaryOp(IntBinaryOp::Add));
    let negs_after_first = count(&builder, NodeKind::IntUnaryOp(IntUnaryOp::Neg));
    assert_eq!(adds_after_first, 1, "first IntSub lift must produce exactly one Add");
    assert_eq!(negs_after_first, 1, "first IntSub lift must produce exactly one Neg");
    {
        let mut lifter = ValueLifter::new(&mut builder, &sleigh);
        // Same inputs (reg(0), reg(4)), DIFFERENT output reg.  Cache must
        // dedupe the inner Neg(reg(4)) and outer Add(reg(0), Neg(reg(4))).
        let insn = Insn {
            opcode: Opcode::IntSub,
            output: Some(reg(0)),
            inputs: vec![reg(0), reg(4)].into(),
        };
        assert!(lifter.lift(&insn).unwrap());
    }
    let adds_after_second = count(&builder, NodeKind::IntBinaryOp(IntBinaryOp::Add));
    let negs_after_second = count(&builder, NodeKind::IntUnaryOp(IntUnaryOp::Neg));
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
    let count_subs_in_graph = |b: &FunctionBuilder| -> usize {
        b.function()
            .graph().all_node_ids()
            .filter(|&id| matches!(b.function().node_kind(id), NodeKind::IntBinaryOp(IntBinaryOp::Add)))
            .count()
    };
    {
        let mut lifter = ValueLifter::new(&mut builder, &sleigh);
        let insn = Insn {
            opcode: Opcode::IntSub,
            output: Some(reg(0)),
            inputs: vec![const_vn(50, 4), const_vn(8, 4)].into(),
        };
        assert!(lifter.lift(&insn).unwrap());
    }
    let after_first = count_subs_in_graph(&builder);
    {
        let mut lifter = ValueLifter::new(&mut builder, &sleigh);
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
        let mut lifter = ValueLifter::new(&mut builder, &sleigh);
        let insn = Insn {
            opcode: Opcode::IntSlessEqual,
            output: Some(reg(0)),
            inputs: vec![const_vn(5, 4), const_vn(7, 4)].into(),
        };
        assert!(lifter.lift(&insn).unwrap());
    }
    assert!(
        graph_has_kind(&builder, NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::Xor)),
        "expected IntBinaryOp::Xor in graph (the I1 logical-NOT wrap, post-BitNot removal)"
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
    let mut lifter = ValueLifter::new(&mut builder, &sleigh);
    let insn = Insn { opcode: Opcode::Branch, output: None, inputs: Default::default() };
    assert!(!lifter.lift(&insn).unwrap());
}

#[test]
fn lift_returns_false_for_cond_branch() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh);
    let insn = Insn { opcode: Opcode::CondBranch, output: None, inputs: Default::default() };
    assert!(!lifter.lift(&insn).unwrap());
}

#[test]
fn lift_returns_false_for_branch_indirect() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh);
    let insn = Insn { opcode: Opcode::BranchIndirect, output: None, inputs: Default::default() };
    assert!(!lifter.lift(&insn).unwrap());
}

#[test]
fn lift_returns_false_for_return() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh);
    let insn = Insn { opcode: Opcode::Return, output: None, inputs: Default::default() };
    assert!(!lifter.lift(&insn).unwrap());
}

#[test]
fn lift_returns_false_for_call() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh);
    let insn = Insn { opcode: Opcode::Call, output: None, inputs: Default::default() };
    assert!(!lifter.lift(&insn).unwrap());
}

#[test]
fn lift_returns_false_for_call_indirect() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh);
    let insn = Insn { opcode: Opcode::CallIndirect, output: None, inputs: Default::default() };
    assert!(!lifter.lift(&insn).unwrap());
}

#[test]
fn lift_returns_false_for_call_other() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh);
    let insn = Insn { opcode: Opcode::CallOther, output: None, inputs: Default::default() };
    assert!(!lifter.lift(&insn).unwrap());
}

#[test]
fn lift_returns_false_for_store() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh);
    let insn = Insn { opcode: Opcode::Store, output: None, inputs: Default::default() };
    assert!(!lifter.lift(&insn).unwrap());
}

#[test]
fn lift_returns_false_for_nop() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh);
    let insn = Insn { opcode: Opcode::Nop, output: None, inputs: Default::default() };
    assert!(!lifter.lift(&insn).unwrap());
}

// ── vn_io tests ─────────────────────────────────────────────────────────────

#[test]
fn read_vn_unknown_returns_initial_var_or_phi() {
    // First read of an architectural register that's never been
    // written in this region should yield either an `InitialVar` (the
    // value at function entry) or a `Phi` (the SSA-style merge node
    // the FunctionBuilder lazily inserts at region entries pointing
    // back to the entry InitialVar).  Either is correct — the
    // producer is NOT some random arithmetic node.
    //
    // Note: `NodeKind::Phi` is now a unit variant; the Vn tag lives
    // on `Graph::phi_var_tag(node)` (the pre-rewrite enum carried
    // the tag inline as `VarPhi(_)`).
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh);
    let value = lifter.read_vn(&reg(0)).expect("read_vn should succeed");
    let producer = lifter.builder.function().producer(value);
    let kind = lifter.builder.function().node_kind(producer);
    assert!(
        matches!(kind, NodeKind::InitialVar(_) | NodeKind::Phi),
        "first read of an unwritten register should produce InitialVar or Phi, got {kind:?}"
    );
}

#[test]
fn write_vn_then_read_vn_round_trip() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh);
    // Write 42 to reg(0).
    let const_42 = lifter
        .builder
        .build_int_const(42u64, strider_ir::ValueType::I32)
        .unwrap();
    lifter.write_vn(&reg(0), const_42).expect("write_vn");
    // Read it back.
    let value = lifter.read_vn(&reg(0)).expect("read_vn");
    let producer = lifter.builder.function().producer(value);
    let kind = lifter.builder.function().node_kind(producer);
    match kind {
        NodeKind::IntConst(n) => assert_eq!(*n, 42u128),
        other => panic!("expected IntConst(42), got {other:?}"),
    }
}

#[test]
fn write_vn_to_const_space_errors() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh);
    let val = lifter
        .builder
        .build_int_const(0u64, strider_ir::ValueType::I32)
        .unwrap();
    let res = lifter.write_vn(&const_vn(0, 4), val);
    assert!(res.is_err(), "writing to CONST space should error");
}

// ── Error paths ─────────────────────────────────────────────────────────────

#[test]
fn lift_subpiece_out_of_range_errors() {
    // byte_offset >= input.size  →  SubpieceOffsetOutOfRange.
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh);
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
    let mut lifter = ValueLifter::new(&mut builder, &sleigh);
    let insn = Insn {
        opcode: Opcode::Copy,
        output: None,
        inputs: vec![const_vn(0, 4)].into(),
    };
    let res = lifter.lift(&insn);
    assert!(res.is_err(), "Copy without output_vn should error");
    if let Err(e) = res {
        assert!(
            e.to_string().contains("instruction has no output varnode"),
            "got: {e}"
        );
    }
}

#[test]
fn lift_binary_op_with_too_few_inputs_errors_not_panics() {
    // A binary opcode (IntAdd) given only ONE input must surface a
    // typed "too few inputs" error rather than panicking on the
    // out-of-bounds `insn.inputs[1]` access.  Regression guard for the
    // panic-safety conversion of raw slice indexing to checked accessors.
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh);
    let insn = Insn {
        opcode: Opcode::IntAdd,
        output: Some(reg(0)),
        // Only one input — the binary handler reads inputs[1].
        inputs: vec![const_vn(7, 4)].into(),
    };
    let res = lifter.lift(&insn);
    assert!(
        res.is_err(),
        "binary op with too few inputs should error, not panic"
    );
}

#[test]
fn lift_call_other_returns_false_via_value_lifter() {
    // CallOther stays in strider; the lifter never claims it.
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    let mut lifter = ValueLifter::new(&mut builder, &sleigh);
    let insn = Insn { opcode: Opcode::CallOther, output: None, inputs: Default::default() };
    assert!(!lifter.lift(&insn).unwrap());
}

// ── Float lift-time canonicalisation shape checks ─────────────────────────────

#[test]
fn lift_float_sub_lowers_to_float_add_neg() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    {
        let mut lifter = ValueLifter::new(&mut builder, &sleigh);
        let insn = Insn {
            opcode: Opcode::FloatSub,
            output: Some(reg(0)),
            inputs: vec![const_vn(0, 4), const_vn(0, 4)].into(),
        };
        assert!(lifter.lift(&insn).unwrap());
    }
    assert!(
        graph_has_kind(&builder, NodeKind::FloatBinaryOp(strider_ir::FloatBinaryOp::Add)),
        "FloatSub lift must produce a FloatAdd (the lowering wrap)"
    );
    assert!(
        graph_has_kind(&builder, NodeKind::FloatUnaryOp(strider_ir::FloatUnaryOp::Neg)),
        "FloatSub lift must produce a FloatUnaryOp::Neg (the negated rhs)"
    );
}

#[test]
fn lift_float_not_equal_lowers_to_boolneg_float_equal() {
    let sleigh = make_sleigh();
    let mut builder = make_builder();
    {
        let mut lifter = ValueLifter::new(&mut builder, &sleigh);
        let insn = Insn {
            opcode: Opcode::FloatNotEqual,
            output: Some(reg(0)),
            inputs: vec![const_vn(0, 4), const_vn(0, 4)].into(),
        };
        assert!(lifter.lift(&insn).unwrap());
    }
    assert!(
        graph_has_kind(&builder, NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::Xor)),
        "FloatNotEqual lift must produce an IntBinaryOp::Xor (the I1 logical-NOT wrap, post-BitNot removal)"
    );
    assert!(
        graph_has_kind(&builder, NodeKind::FloatCmpOp(strider_ir::FloatCmpOp::Equal)),
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
        let mut lifter = ValueLifter::new(&mut builder, &sleigh);
        let insn = Insn {
            opcode: Opcode::FloatLessEqual,
            output: Some(reg(0)),
            inputs: vec![const_vn(0, 4), const_vn(0, 4)].into(),
        };
        assert!(lifter.lift(&insn).unwrap());
    }
    assert!(
        graph_has_kind(&builder, NodeKind::IntBinaryOp(IntBinaryOp::Or)),
        "FloatLessEqual lift must produce an IntBinaryOp::Or (the disjunction wrap)"
    );
    assert!(
        graph_has_kind(&builder, NodeKind::FloatCmpOp(strider_ir::FloatCmpOp::Less)),
        "FloatLessEqual lift must produce a FloatCmpOp::Less"
    );
    assert!(
        graph_has_kind(&builder, NodeKind::FloatCmpOp(strider_ir::FloatCmpOp::Equal)),
        "FloatLessEqual lift must produce a FloatCmpOp::Equal"
    );
}
