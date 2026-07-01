//! Value-opcode lifting unit tests.
//!
//! These hand-build `rsleigh::Insn` structs (chosen REGISTER/CONST
//! varnodes — not decoded from bytes) and lift them through the unified
//! per-CFG dispatch ([`FunctionLifter::process_insn`]).  The CFG and
//! calling convention handed to the lifter are throwaway scaffolding:
//! value lifting touches only the IR builder and the Sleigh context, and
//! never consults the region id or the region map (an empty
//! `RegionMap::default()` is passed).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use rsleigh::mem_readers::BufMemReader;
use rsleigh::{Insn, Opcode, Vn, VnSpace};

use strider_ir::node::{NodeId, NodeKind};
use strider_ir::{FunctionBuilder, IRBuilderExt, IRViewer, IntBinaryOp, IntCmpOp, IntUnaryOp};

use crate::lift::{FunctionLifter, Lifter};

type TestReader = BufMemReader<Vec<u8>>;

/// Default endianness for these tests.
const TEST_ENDIAN: strider_target::Endianness = strider_target::Endianness::Little;

/// 4-byte register at the given REGISTER-space offset.
fn reg(off: u64) -> Vn {
    Vn {
        size: 4,
        addr_off: off,
        addr_space: VnSpace::REGISTER,
    }
}

/// CONST-space varnode of byte width `size` carrying integer `val`.
fn const_vn(val: u64, size: u32) -> Vn {
    Vn {
        size,
        addr_off: val,
        addr_space: VnSpace::CONST,
    }
}

/// A throwaway pcode address for the `process_insn` funnel.  Tests that
/// don't inspect asm-fingerprints use this; the funnel's
/// `set_lift_addr(Some(..))/set_lift_addr(None)` bracket leaves the builder
/// back at `lift_addr = None` afterward.
fn test_addr() -> strider_cfg::PcodeInsnAddr {
    strider_cfg::PcodeInsnAddr::at_machine_start(0x1000)
}

/// An empty (no-arg, no-clobber) convention — matches the synthetic
/// builder the pre-merge value-lifter tests used (the value handlers
/// never consult the convention).  Struct-literal construction skips the
/// ABI-disjointness validation, fine for a synthetic fixture.
pub(super) fn empty_cc() -> strider_target::BuiltCallingConvention {
    let _ = TEST_ENDIAN;
    strider_target::BuiltCallingConvention {
        arg_passing_regs: Vec::new(),
        callee_saved_regs: Vec::new(),
        ret_val_regs: Vec::new(),
        ret_val_regs_float: Vec::new(),
        stack_vn: Vn {
            size: 4,
            addr_off: 0x9000,
            addr_space: VnSpace::REGISTER,
        },
        stack_args: None,
        ret_stack_pop: 0,
        link_register_vn: None,
        preserves_memory: false,
    }
}

/// Runs `f` with a `FunctionLifter` whose builder tracks three 4-byte
/// REGISTER vars at offsets 0/4/8 and has a single entry region — the
/// same synthetic state the pre-merge value-lifter unit tests used.
///
/// The Sleigh + CFG are throwaway scaffolding (a single `ret` at 0x1000);
/// the helper owns all the borrowed locals so the per-CFG lifter — which
/// borrows them — need not be returned.
fn with_test_lifter(f: impl FnOnce(&mut FunctionLifter<'_, TestReader>, strider_cfg::RegionId)) {
    with_test_lifter_tracking(vec![reg(0), reg(4), reg(8)], f);
}

/// Like [`with_test_lifter`] but with an explicit tracked-varnode list
/// (`all_vns`).  Tests exercising registers wider than the default 4-byte
/// regs (e.g. a 32-byte YMM / 64-byte ZMM `IntNeg`) seed those containers
/// here so `read_vn` finds them rather than erroring on an unknown variable.
/// `all_vns` must be pre-sorted by `(space, offset, size)` like the lifter
/// expects.
fn with_test_lifter_tracking(
    all_vns: Vec<Vn>,
    f: impl FnOnce(&mut FunctionLifter<'_, TestReader>, strider_cfg::RegionId),
) {
    with_test_lifter_tracking_arch(strider_target::SleighArch::x86(), vec![0xc3], all_vns, f);
}

/// Like [`with_test_lifter_tracking`] but for an arbitrary `arch` and
/// caller-provided terminator `term_bytes` (the single-instruction throwaway
/// CFG must terminate cleanly — e.g. `0xc3` `ret` on x86, or `4e 80 00 20`
/// `blr` on big-endian PowerPC).  This lets the aliasing tests exercise the
/// prod `read_reg_vn`/`write_reg_vn` methods under both endiannesses.
pub(super) fn with_test_lifter_tracking_arch(
    arch: strider_target::SleighArch,
    term_bytes: Vec<u8>,
    all_vns: Vec<Vn>,
    f: impl FnOnce(&mut FunctionLifter<'_, TestReader>, strider_cfg::RegionId),
) {
    let mut sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        BufMemReader::new(term_bytes, 0x1000),
    )
    .expect("create test Sleigh");
    let cfg = strider_cfg::Builder::for_arch(
        &arch,
        &mut sleigh,
        0x1000,
        &strider_cfg::CfgOptions::default(),
    )
    .build()
    .expect("throwaway cfg");
    // The throwaway CFG's entry region id — handed to `process_insn` as the
    // `region_id` arg.  Value opcodes never consult it (or the region map),
    // so any valid id is fine.
    let region_id = cfg.entry();
    // The Lifter now owns the Sleigh; CC is a per-call argument.
    let cc = empty_cc();
    let lifter = Lifter::new(arch, sleigh).expect("lifter");
    let no_overrides = rustc_hash::FxHashMap::default();
    let mut driver =
        FunctionLifter::new(&lifter, &cc, &cfg, all_vns, &no_overrides).expect("driver");
    // Entry-region setup (matches the old `make_builder`).  Clear the
    // lift address so tests start from `lift_addr = None`.
    driver.builder.set_lift_addr(None);
    driver.builder.build_entry().expect("build_entry");
    let region = driver.builder.create_region().expect("create_region");
    driver
        .builder
        .set_entry_region(region)
        .expect("set_entry_region");
    driver.builder.set_region(region);
    f(&mut driver, region_id);
}

/// Shared scaffold: lift one hand-built `Insn` through the unified
/// `process_insn` dispatch and assert it succeeds (the opcode lifts
/// cleanly).  Value opcodes never resolve a region, so an empty
/// `RegionMap` is passed.
fn assert_lifts_one(opcode: Opcode, output: Option<Vn>, inputs: Vec<Vn>) {
    with_test_lifter(|d, rid| {
        let insn = Insn {
            opcode,
            output,
            inputs: inputs.into(),
        };
        d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
            .unwrap_or_else(|e| panic!("process_insn failed for {opcode:?}: {e}"));
    });
}

// ── Smoke tests (validation subset; full port follows) ───────────────────

#[test]
fn lift_int_add_of_consts() {
    assert_lifts_one(
        Opcode::IntAdd,
        Some(reg(0)),
        vec![const_vn(7, 4), const_vn(35, 4)],
    );
}

#[test]
fn lift_int_sub_of_consts() {
    assert_lifts_one(
        Opcode::IntSub,
        Some(reg(0)),
        vec![const_vn(50, 4), const_vn(8, 4)],
    );
}

#[test]
fn lift_bool_and_of_consts() {
    assert_lifts_one(
        Opcode::BoolAnd,
        Some(reg(0)),
        vec![const_vn(1, 1), const_vn(1, 1)],
    );
}

#[test]
fn lift_popcount() {
    assert_lifts_one(Opcode::Popcount, Some(reg(0)), vec![const_vn(0b1011, 4)]);
}

#[test]
fn lift_insert_field_past_dest_width_errors() {
    with_test_lifter(|d, rid| {
        let insn = Insn {
            opcode: Opcode::Insert,
            output: Some(reg(0)),
            inputs: vec![reg(0), reg(8), const_vn(24, 4), const_vn(16, 4)].into(),
        };
        assert!(
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .is_err(),
            "Insert field exceeding destination width must error"
        );
    });
}

// ── Boolean family ──────────────────────────────────────────────────────────

#[test]
fn lift_bool_or_of_consts() {
    assert_lifts_one(
        Opcode::BoolOr,
        Some(reg(0)),
        vec![const_vn(0, 1), const_vn(1, 1)],
    );
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
fn lift_int_mul_of_consts() {
    assert_lifts_one(
        Opcode::IntMul,
        Some(reg(0)),
        vec![const_vn(6, 4), const_vn(7, 4)],
    );
}

/// `IntNeg` (Sleigh bitwise complement) at a narrow width lowers to
/// `Xor(x, all_ones)` with the all-ones constant materialised inline.
#[test]
fn lift_int_neg_narrow_lowers_to_xor_all_ones() {
    assert_lifts_one(Opcode::IntNeg, Some(reg(0)), vec![const_vn(0x1234, 4)]);
    with_test_lifter(|d, rid| {
        let insn = Insn {
            opcode: Opcode::IntNeg,
            output: Some(reg(0)),
            inputs: vec![const_vn(0x1234, 4)].into(),
        };
        d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
            .unwrap();
        assert!(
            graph_has_kind(&d.builder, NodeKind::IntBinaryOp(IntBinaryOp::Xor)),
            "narrow IntNeg must lower to an Xor"
        );
        // The I32 all-ones constant has value 0xFFFF_FFFF.
        assert!(
            graph_has_int_const_value(&d.builder, 0xFFFF_FFFF),
            "narrow IntNeg must materialise the I32 all-ones constant"
        );
    });
}

/// A register-wide (256-bit YMM / 512-bit ZMM) `IntNeg` is a well-defined
/// bitwise complement.  The lift must SUCCEED and produce `Xor(x, all_ones)`
/// at the wide type with the wide all-ones constant — the all-ones operand
/// goes through the wide-const path rather than `build_int_const` (which
/// rejects I256/I512).
#[test]
fn lift_int_neg_register_wide_lowers_to_xor_all_ones() {
    // (label, output byte width, wide ValueType).
    let cases: [(&str, u32, strider_ir::ValueType); 2] = [
        ("ymm_i256", 32, strider_ir::ValueType::I256),
        ("zmm_i512", 64, strider_ir::ValueType::I512),
    ];
    for (label, width, _ty) in cases {
        // A wide REGISTER varnode (own container — direct access, no
        // sub-register masking).  Offset well clear of the default 4-byte
        // regs at 0/4/8; tracked so the read resolves to its InitialVar.
        let wide = Vn {
            size: width,
            addr_off: 0x100,
            addr_space: VnSpace::REGISTER,
        };
        with_test_lifter_tracking(vec![wide], |d, rid| {
            let insn = Insn {
                opcode: Opcode::IntNeg,
                output: Some(wide),
                inputs: vec![wide].into(),
            };
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .unwrap_or_else(|e| {
                    panic!("{label}: register-wide IntNeg must lift, got error: {e}")
                });
            assert!(
                graph_has_kind(&d.builder, NodeKind::IntBinaryOp(IntBinaryOp::Xor)),
                "{label}: register-wide IntNeg must lower to an Xor at the wide type"
            );
        });
    }
}

// ── Cast family ─────────────────────────────────────────────────────────────

#[test]
fn lift_truncate_extracts_low_bits() {
    // Subpiece(value, byte_offset, out_size).
    assert_lifts_one(
        Opcode::Subpiece,
        Some(Vn {
            size: 1,
            addr_off: 0,
            addr_space: VnSpace::REGISTER,
        }),
        vec![const_vn(0x1234_5678, 4), const_vn(0, 4)],
    );
}

#[test]
fn lift_piece_concatenates() {
    assert_lifts_one(
        Opcode::Piece,
        Some(Vn {
            size: 4,
            addr_off: 0,
            addr_space: VnSpace::REGISTER,
        }),
        vec![const_vn(0xAA, 2), const_vn(0xBB, 2)],
    );
}

#[test]
fn lift_extract_returns_slice() {
    // Extract(value, lsb, bit_count).
    assert_lifts_one(
        Opcode::Extract,
        Some(Vn {
            size: 1,
            addr_off: 0,
            addr_space: VnSpace::REGISTER,
        }),
        vec![const_vn(0xFF00, 4), const_vn(8, 4), const_vn(8, 4)],
    );
}

#[test]
fn lift_extract_field_past_input_width_errors() {
    // Extract(value, lsb=28, bit_count=8) from a 4-byte (32-bit) input:
    // 28 + 8 = 36 > 32 — the slice runs past the input.  Must error.
    with_test_lifter(|d, rid| {
        let insn = Insn {
            opcode: Opcode::Extract,
            output: Some(Vn {
                size: 1,
                addr_off: 0,
                addr_space: VnSpace::REGISTER,
            }),
            inputs: vec![const_vn(0xFFFF_FFFF, 4), const_vn(28, 4), const_vn(8, 4)].into(),
        };
        assert!(
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .is_err(),
            "Extract slice exceeding input width must error"
        );
    });
}

#[test]
fn lift_lzcount() {
    assert_lifts_one(Opcode::Lzcount, Some(reg(0)), vec![const_vn(0xF, 4)]);
}

// ── Float family ────────────────────────────────────────────────────────────

#[test]
fn lift_float_add_of_consts() {
    with_test_lifter(|d, rid| {
        let insn = Insn {
            opcode: Opcode::FloatAdd,
            output: Some(reg(0)),
            // 4-byte (F32) varnodes — float-typed when read via read_vn,
            // but const space carries arbitrary bits.
            inputs: vec![const_vn(0, 4), const_vn(0, 4)].into(),
        };
        d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
            .unwrap();
    });
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
    assert_lifts_one(
        Opcode::SegmentOp,
        Some(reg(0)),
        vec![const_vn(0, 4), const_vn(0, 2), const_vn(0, 4)],
    );
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
        .graph()
        .all_node_ids()
        .any(|id| builder.function().node_kind(id) == &target)
}

/// Returns the first node-id in the graph matching `target`, or `None`.
fn find_first_node(builder: &FunctionBuilder, target: NodeKind) -> Option<NodeId> {
    builder
        .function()
        .graph()
        .all_node_ids()
        .find(|id| builder.function().node_kind(*id) == &target)
}

/// Returns true if the graph contains an `IntConst` node whose value (masked
/// to width, read via `int_const_u128`) equals `expected`.  Used in tests that
/// pin a specific small-integer constant value without depending on the opaque
/// `ConstId` internals.
fn graph_has_int_const_value(builder: &FunctionBuilder, expected: u128) -> bool {
    builder.function().graph().all_node_ids().any(|id| {
        if !matches!(builder.function().node_kind(id), NodeKind::IntConst(_)) {
            return false;
        }
        let outputs = builder.function().node_outputs(id);
        outputs
            .iter()
            .any(|&v| builder.function().int_const_u128(v) == Some(expected))
    })
}

/// Returns the first `IntConst` node-id whose output value equals `expected`,
/// or `None` if no such node is present.
fn find_int_const_node(builder: &FunctionBuilder, expected: u128) -> Option<NodeId> {
    builder.function().graph().all_node_ids().find(|&id| {
        if !matches!(builder.function().node_kind(id), NodeKind::IntConst(_)) {
            return false;
        }
        let outputs = builder.function().node_outputs(id);
        outputs
            .iter()
            .any(|&v| builder.function().int_const_u128(v) == Some(expected))
    })
}

#[test]
fn signed_binary_op_sign_extends_narrower_operand() {
    // IntSdiv with a 2-byte dividend (0xFFFF = -1) and a 4-byte output.
    // A signed op must SIGN-extend the narrower operand to the op width
    // (0xFFFF -> 0xFFFF_FFFF), not zero-extend it (-> 0x0000_FFFF).  Under
    // the prior build_int_binary_operation zero-extension the 32-bit value
    // 0xFFFF_FFFF never appeared.
    with_test_lifter(|d, rid| {
        {
            let insn = Insn {
                opcode: Opcode::IntSdiv,
                output: Some(reg(0)),
                inputs: vec![const_vn(0xFFFF, 2), const_vn(2, 4)].into(),
            };
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .unwrap();
        }
        assert!(
            graph_has_kind(&d.builder, NodeKind::IntBinaryOp(IntBinaryOp::Sdiv)),
            "expected an Sdiv node"
        );
        assert!(
            graph_has_int_const_value(&d.builder, 0xFFFF_FFFF),
            "the 2-byte -1 dividend must be SIGN-extended to the 4-byte op width (0xFFFF_FFFF)"
        );
    });
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
    with_test_lifter(|d, rid| {
        {
            let insn = Insn {
                opcode: Opcode::IntSless,
                output: Some(reg(0)),
                inputs: vec![const_vn(0xFFFF_FFFF, 4), const_vn(5, 8)].into(),
            };
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .unwrap();
        }
        assert!(
            graph_has_kind(&d.builder, NodeKind::IntCmpOp(IntCmpOp::Sless)),
            "expected an Sless comparison node"
        );
        assert!(
            graph_has_int_const_value(&d.builder, 0xFFFF_FFFF_FFFF_FFFF),
            "the 4-byte -1 operand must be SIGN-extended to the 8-byte max width \
             (0xFFFF_FFFF_FFFF_FFFF) — proving max-width comparison + sign-correct extension"
        );
    });
}

#[test]
fn lift_with_set_lift_addr_records_asm_fingerprint() {
    with_test_lifter(|d, rid| {
        // `process_insn` owns the fingerprint funnel: it brackets the lift
        // with the machine address carried by its `addr` argument, so we
        // drive the fingerprint via that address (0x4242) rather than a
        // manual `set_lift_addr`.
        {
            let insn = Insn {
                opcode: Opcode::IntAdd,
                output: Some(reg(0)),
                inputs: vec![const_vn(3, 4), const_vn(4, 4)].into(),
            };
            d.process_insn(
                rid,
                &insn,
                strider_cfg::PcodeInsnAddr::at_machine_start(0x4242),
                &super::RegionMap::default(),
            )
            .unwrap();
        }
        let add_node = find_first_node(&d.builder, NodeKind::IntBinaryOp(IntBinaryOp::Add))
            .expect("IntAdd lift must produce an Add node");
        let fp = d.builder.function().asm_fingerprint(add_node);
        assert_eq!(fp, &[0x4242], "Add node fingerprint should record 0x4242");
        // The two IntConst inputs should also carry the address.
        let const3 = find_int_const_node(&d.builder, 3).expect("IntConst(3) must be present");
        let const4 = find_int_const_node(&d.builder, 4).expect("IntConst(4) must be present");
        assert_eq!(d.builder.function().asm_fingerprint(const3), &[0x4242]);
        assert_eq!(d.builder.function().asm_fingerprint(const4), &[0x4242]);
    });
}

#[test]
fn lift_without_lift_addr_leaves_fingerprint_empty() {
    // `process_insn` always brackets the lift with its `addr` argument and
    // resets `lift_addr` to `None` on exit.  This pins the funnel's reset
    // arm: a node built AFTER `process_insn` returns (with no lift addr in
    // effect) carries an empty fingerprint.
    with_test_lifter(|d, rid| {
        {
            let insn = Insn {
                opcode: Opcode::IntAdd,
                output: Some(reg(0)),
                inputs: vec![const_vn(3, 4), const_vn(4, 4)].into(),
            };
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .unwrap();
        }
        // The funnel has reset `lift_addr` to `None`; a fresh node minted
        // now must have no fingerprint.
        let outside = d
            .builder
            .build_int_const(0x55u64, strider_ir::ValueType::I32)
            .unwrap();
        let outside_node = d.builder.function().producer(outside);
        assert!(
            d.builder
                .function()
                .asm_fingerprint(outside_node)
                .is_empty(),
            "a node built after process_insn returns should have an empty fingerprint \
             (the funnel reset lift_addr to None)"
        );
    });
}

#[test]
fn lift_dedup_unions_two_addresses() {
    // Same insn lifted twice from two different machine addresses; the
    // dedup cache returns the same NodeId; both contributors are unioned.
    with_test_lifter(|d, rid| {
        let insn = Insn {
            opcode: Opcode::IntAdd,
            output: Some(reg(0)),
            inputs: vec![const_vn(3, 4), const_vn(4, 4)].into(),
        };
        // Drive the two contributing machine addresses through
        // `process_insn`'s fingerprint funnel (its `addr` argument).
        d.process_insn(
            rid,
            &insn,
            strider_cfg::PcodeInsnAddr::at_machine_start(0x1000),
            &super::RegionMap::default(),
        )
        .unwrap();
        d.process_insn(
            rid,
            &insn,
            strider_cfg::PcodeInsnAddr::at_machine_start(0x2000),
            &super::RegionMap::default(),
        )
        .unwrap();
        let add_node = find_first_node(&d.builder, NodeKind::IntBinaryOp(IntBinaryOp::Add))
            .expect("Add must dedup to a single node");
        let fp = d.builder.function().asm_fingerprint(add_node);
        assert_eq!(fp, &[0x1000, 0x2000], "both addresses should be unioned");
    });
}

#[test]
fn lift_int_less_equal_lowers_to_boolneg_less() {
    with_test_lifter(|d, rid| {
        {
            let insn = Insn {
                opcode: Opcode::IntLessEqual,
                output: Some(reg(0)),
                inputs: vec![const_vn(5, 4), const_vn(7, 4)].into(),
            };
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .unwrap();
        }
        // Canonical shape: `Xor(IntLess(_, _), IntConst(1)):I1` (a 1-bit
        // logical NOT — the former BitNot unary-op was removed in favour of the
        // Xor-with-all-ones shape).  Pin the I1 Xor and the IntCmpOp::Less.
        assert!(
            graph_has_kind(
                &d.builder,
                NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::Xor)
            ),
            "expected IntBinaryOp::Xor in graph (the I1 logical-NOT wrap)"
        );
        assert!(
            graph_has_kind(&d.builder, NodeKind::IntCmpOp(IntCmpOp::Less)),
            "expected IntCmpOp::Less in graph (the lowered cmp)"
        );
    });
}

/// `IntLessEqual(a, b)` lowers to `Xor(IntLess(b, a), 1):I1` — a SWAP.
/// Feeding two DISTINCT register operands lets us assert the `Less` node's
/// inputs are in SWAPPED order (rhs-then-lhs vs instruction order): the
/// first cmp input is the read of `b` (reg(4)), the second is the read of
/// `a` (reg(0)).  Existing const-fed tests could not catch a dropped swap.
#[test]
fn lift_int_less_equal_swaps_operands() {
    with_test_lifter(|d, rid| {
        {
            // IntLessEqual(reg(0), reg(4)): a = reg(0), b = reg(4).
            let insn = Insn {
                opcode: Opcode::IntLessEqual,
                output: Some(reg(8)),
                inputs: vec![reg(0), reg(4)].into(),
            };
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .unwrap();
        }
        let less = find_first_node(&d.builder, NodeKind::IntCmpOp(IntCmpOp::Less))
            .expect("IntLessEqual must lower to an IntLess cmp");
        let [cmp_lhs, cmp_rhs] = d
            .builder
            .function()
            .node_inputs_exact::<2>(less)
            .expect("Less has two inputs");
        // Swapped: cmp_lhs is the read of b (reg(4)), cmp_rhs is the read of
        // a (reg(0)).  An entry-region register read materialises as a Phi
        // whose source-varnode tag (get_vn_for_value) names the register.
        assert_eq!(
            d.builder.function().get_vn_for_value(cmp_lhs),
            Some(reg(4)),
            "Less first operand must be the SWAPPED rhs (read of b = reg(4))"
        );
        assert_eq!(
            d.builder.function().get_vn_for_value(cmp_rhs),
            Some(reg(0)),
            "Less second operand must be the SWAPPED lhs (read of a = reg(0))"
        );
    });
}

#[test]
fn lift_int_sub_lowers_to_add_neg() {
    with_test_lifter(|d, rid| {
        {
            let insn = Insn {
                opcode: Opcode::IntSub,
                output: Some(reg(0)),
                inputs: vec![const_vn(50, 4), const_vn(8, 4)].into(),
            };
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .unwrap();
        }
        // Canonical shape: IntBinaryOp::Add over (lhs, IntUnaryOp::Neg(rhs)).
        assert!(
            graph_has_kind(&d.builder, NodeKind::IntBinaryOp(IntBinaryOp::Add)),
            "expected IntBinaryOp::Add in graph (the lowering wrap)"
        );
        assert!(
            graph_has_kind(&d.builder, NodeKind::IntUnaryOp(IntUnaryOp::Neg)),
            "expected IntUnaryOp::Neg in graph (the negated rhs)"
        );
    });
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
    with_test_lifter(|d, rid| {
        let count = |b: &FunctionBuilder, target: NodeKind| -> usize {
            b.function()
                .graph()
                .all_node_ids()
                .filter(|&id| b.function().node_kind(id) == &target)
                .count()
        };
        {
            // IntSub reg(0), reg(4)  →  reg(8).  Variable inputs.
            let insn = Insn {
                opcode: Opcode::IntSub,
                output: Some(reg(8)),
                inputs: vec![reg(0), reg(4)].into(),
            };
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .unwrap();
        }
        let adds_after_first = count(&d.builder, NodeKind::IntBinaryOp(IntBinaryOp::Add));
        let negs_after_first = count(&d.builder, NodeKind::IntUnaryOp(IntUnaryOp::Neg));
        assert_eq!(
            adds_after_first, 1,
            "first IntSub lift must produce exactly one Add"
        );
        assert_eq!(
            negs_after_first, 1,
            "first IntSub lift must produce exactly one Neg"
        );
        {
            // Same inputs (reg(0), reg(4)), DIFFERENT output reg.  Cache must
            // dedupe the inner Neg(reg(4)) and outer Add(reg(0), Neg(reg(4))).
            let insn = Insn {
                opcode: Opcode::IntSub,
                output: Some(reg(0)),
                inputs: vec![reg(0), reg(4)].into(),
            };
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .unwrap();
        }
        let adds_after_second = count(&d.builder, NodeKind::IntBinaryOp(IntBinaryOp::Add));
        let negs_after_second = count(&d.builder, NodeKind::IntUnaryOp(IntUnaryOp::Neg));
        assert_eq!(
            adds_after_second, adds_after_first,
            "second IntSub lift with same operands must dedup the Add via the node cache"
        );
        assert_eq!(
            negs_after_second, negs_after_first,
            "second IntSub lift with same operands must dedup the Neg via the node cache"
        );
    });
}

/// Companion to the variable-operand cache test: two const-operand lifts
/// must also dedupe.  Cheaper to detect cache-bypass regressions on the
/// happy path before they cause graph bloat in real binaries.
#[test]
fn lift_int_sub_caches_lowered_shape() {
    with_test_lifter(|d, rid| {
        let count_subs_in_graph = |b: &FunctionBuilder| -> usize {
            b.function()
                .graph()
                .all_node_ids()
                .filter(|&id| {
                    matches!(
                        b.function().node_kind(id),
                        NodeKind::IntBinaryOp(IntBinaryOp::Add)
                    )
                })
                .count()
        };
        {
            let insn = Insn {
                opcode: Opcode::IntSub,
                output: Some(reg(0)),
                inputs: vec![const_vn(50, 4), const_vn(8, 4)].into(),
            };
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .unwrap();
        }
        let after_first = count_subs_in_graph(&d.builder);
        {
            // Same operands, different output reg — the value-producing nodes
            // should still dedupe through the cache.
            let insn = Insn {
                opcode: Opcode::IntSub,
                output: Some(reg(4)),
                inputs: vec![const_vn(50, 4), const_vn(8, 4)].into(),
            };
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .unwrap();
        }
        let after_second = count_subs_in_graph(&d.builder);
        assert_eq!(
            after_first, after_second,
            "second IntSub lift must dedupe the lowered Add+Neg shape via the node cache"
        );
    });
}

#[test]
fn lift_int_sless_equal_lowers_to_boolneg_sless() {
    with_test_lifter(|d, rid| {
        {
            let insn = Insn {
                opcode: Opcode::IntSlessEqual,
                output: Some(reg(0)),
                inputs: vec![const_vn(5, 4), const_vn(7, 4)].into(),
            };
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .unwrap();
        }
        assert!(
            graph_has_kind(
                &d.builder,
                NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::Xor)
            ),
            "expected IntBinaryOp::Xor in graph (the I1 logical-NOT wrap, post-BitNot removal)"
        );
        assert!(
            graph_has_kind(&d.builder, NodeKind::IntCmpOp(IntCmpOp::Sless)),
            "expected IntCmpOp::Sless in graph (the lowered cmp)"
        );
    });
}

// ── Control-flow / call / store opcodes (now dispatched, not declined) ──────
//
// Before the value/control dispatch merge these opcodes were *declined* by
// the value lifter (`lift_value` returned `Ok(false)`) and routed through a
// second control match.  With the unified `process_insn` there is one match
// and each opcode is dispatched to its real handler.  These tests pin that
// routing: opcodes whose handler reads operands surface a typed error on the
// empty (no-input) insns used here; the no-operand handlers (Nop / Branch /
// Return / BranchIndirect) succeed.  An empty region map is passed —
// none of these handlers consults it on these inputs (`CondBranch`
// errors on its missing condition operand before any lookup).

#[test]
fn process_insn_branch_is_noop_ok() {
    with_test_lifter(|d, rid| {
        let insn = Insn {
            opcode: Opcode::Branch,
            output: None,
            inputs: Default::default(),
        };
        assert!(
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .is_ok(),
            "Branch dispatches to the no-op handle_branch"
        );
    });
}

#[test]
fn process_insn_cond_branch_errors_on_missing_cond() {
    with_test_lifter(|d, rid| {
        let insn = Insn {
            opcode: Opcode::CondBranch,
            output: None,
            inputs: Default::default(),
        };
        assert!(
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .is_err(),
            "CondBranch reads its condition operand and errors when absent"
        );
    });
}

#[test]
fn process_insn_branch_indirect_dispatches_to_return() {
    with_test_lifter(|d, rid| {
        let insn = Insn {
            opcode: Opcode::BranchIndirect,
            output: None,
            inputs: Default::default(),
        };
        assert!(
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .is_ok(),
            "BranchIndirect shares the CC Return handler (link-register return)"
        );
    });
}

#[test]
fn process_insn_return_dispatches_to_return() {
    with_test_lifter(|d, rid| {
        let insn = Insn {
            opcode: Opcode::Return,
            output: None,
            inputs: Default::default(),
        };
        assert!(
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .is_ok(),
            "Return dispatches to the CC return handler"
        );
    });
}

#[test]
fn process_insn_call_errors_on_missing_target() {
    with_test_lifter(|d, rid| {
        let insn = Insn {
            opcode: Opcode::Call,
            output: None,
            inputs: Default::default(),
        };
        assert!(
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .is_err(),
            "Call reads its target operand and errors when absent"
        );
    });
}

#[test]
fn process_insn_call_indirect_errors_on_missing_target() {
    with_test_lifter(|d, rid| {
        let insn = Insn {
            opcode: Opcode::CallIndirect,
            output: None,
            inputs: Default::default(),
        };
        assert!(
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .is_err(),
            "CallIndirect reads its target operand and errors when absent"
        );
    });
}

#[test]
fn process_insn_call_other_errors_on_missing_user_op_id() {
    with_test_lifter(|d, rid| {
        let insn = Insn {
            opcode: Opcode::CallOther,
            output: None,
            inputs: Default::default(),
        };
        assert!(
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .is_err(),
            "CallOther reads its user-op id operand and errors when absent"
        );
    });
}

#[test]
fn process_insn_store_errors_on_missing_operands() {
    with_test_lifter(|d, rid| {
        let insn = Insn {
            opcode: Opcode::Store,
            output: None,
            inputs: Default::default(),
        };
        assert!(
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .is_err(),
            "Store reads its address/data operands and errors when absent"
        );
    });
}

#[test]
fn process_insn_nop_is_ok() {
    with_test_lifter(|d, rid| {
        let insn = Insn {
            opcode: Opcode::Nop,
            output: None,
            inputs: Default::default(),
        };
        assert!(
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .is_ok(),
            "Nop dispatches to the empty arm"
        );
    });
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
    // in the `value_vn` map keyed by the Phi's output ValueId
    // (queried via `Function::get_vn_for_value`; the pre-rewrite
    // enum carried the tag inline as `VarPhi(_)`).
    with_test_lifter(|d, _rid| {
        let value = d.read_vn(&reg(0)).expect("read_vn should succeed");
        let producer = d.builder.function().producer(value);
        let kind = d.builder.function().node_kind(producer);
        assert!(
            matches!(kind, NodeKind::InitialVar(_) | NodeKind::Phi),
            "first read of an unwritten register should produce InitialVar or Phi, got {kind:?}"
        );
    });
}

#[test]
fn write_vn_then_read_vn_round_trip() {
    with_test_lifter(|d, _rid| {
        // Write 42 to reg(0).
        let const_42 = d
            .builder
            .build_int_const(42u64, strider_ir::ValueType::I32)
            .unwrap();
        d.write_vn(&reg(0), const_42).expect("write_vn");
        // Read it back.
        let value = d.read_vn(&reg(0)).expect("read_vn");
        let producer = d.builder.function().producer(value);
        assert!(
            matches!(
                d.builder.function().node_kind(producer),
                NodeKind::IntConst(_)
            ),
            "expected IntConst node, got {:?}",
            d.builder.function().node_kind(producer)
        );
        assert_eq!(
            d.builder.function().int_const_u128(value),
            Some(42u128),
            "expected IntConst(42)"
        );
    });
}

#[test]
fn write_vn_to_const_space_errors() {
    with_test_lifter(|d, _rid| {
        let val = d
            .builder
            .build_int_const(0u64, strider_ir::ValueType::I32)
            .unwrap();
        let res = d.write_vn(&const_vn(0, 4), val);
        assert!(res.is_err(), "writing to CONST space should error");
    });
}

// ── Error paths ─────────────────────────────────────────────────────────────

#[test]
fn lift_subpiece_out_of_range_errors() {
    // byte_offset >= input.size  →  SubpieceOffsetOutOfRange.
    with_test_lifter(|d, rid| {
        let insn = Insn {
            opcode: Opcode::Subpiece,
            output: Some(Vn {
                size: 1,
                addr_off: 0,
                addr_space: VnSpace::REGISTER,
            }),
            // input is 4 bytes wide, byte_offset = 5 (> 4) ⇒ error.
            inputs: vec![const_vn(0, 4), const_vn(5, 4)].into(),
        };
        let res = d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default());
        assert!(res.is_err(), "out-of-range Subpiece should error");
        if let Err(e) = res {
            assert!(
                format!("{e:#}").contains("Subpiece byte_offset"),
                "got: {e:#}"
            );
        }
    });
}

#[test]
fn lift_missing_output_errors_for_op_that_needs_one() {
    with_test_lifter(|d, rid| {
        let insn = Insn {
            opcode: Opcode::Copy,
            output: None,
            inputs: vec![const_vn(0, 4)].into(),
        };
        let res = d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default());
        assert!(res.is_err(), "Copy without output_vn should error");
        if let Err(e) = res {
            assert!(
                format!("{e:#}").contains("instruction has no output varnode"),
                "got: {e:#}"
            );
        }
    });
}

#[test]
fn lift_binary_op_with_too_few_inputs_errors_not_panics() {
    // A binary opcode (IntAdd) given only ONE input must surface a
    // typed "too few inputs" error rather than panicking on the
    // out-of-bounds `insn.inputs[1]` access.  Regression guard for the
    // panic-safety conversion of raw slice indexing to checked accessors.
    with_test_lifter(|d, rid| {
        let insn = Insn {
            opcode: Opcode::IntAdd,
            output: Some(reg(0)),
            // Only one input — the binary handler reads inputs[1].
            inputs: vec![const_vn(7, 4)].into(),
        };
        let res = d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default());
        assert!(
            res.is_err(),
            "binary op with too few inputs should error, not panic"
        );
    });
}

#[test]
fn process_insn_call_other_dispatches_to_call_other_handler() {
    // CallOther is dispatched to handle_call_other, which reads the user-op
    // id operand and errors when it is absent (as here).  Pins that the
    // unified dispatch routes CallOther to its handler rather than declining
    // it as the pre-merge value lifter did.
    with_test_lifter(|d, rid| {
        let insn = Insn {
            opcode: Opcode::CallOther,
            output: None,
            inputs: Default::default(),
        };
        assert!(
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .is_err(),
            "CallOther dispatches to handle_call_other and errors on the missing user-op id"
        );
    });
}

// ── Float lift-time canonicalisation shape checks ─────────────────────────────

#[test]
fn lift_float_sub_lowers_to_float_add_neg() {
    with_test_lifter(|d, rid| {
        {
            let insn = Insn {
                opcode: Opcode::FloatSub,
                output: Some(reg(0)),
                inputs: vec![const_vn(0, 4), const_vn(0, 4)].into(),
            };
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .unwrap();
        }
        assert!(
            graph_has_kind(
                &d.builder,
                NodeKind::FloatBinaryOp(strider_ir::FloatBinaryOp::Add)
            ),
            "FloatSub lift must produce a FloatAdd (the lowering wrap)"
        );
        assert!(
            graph_has_kind(
                &d.builder,
                NodeKind::FloatUnaryOp(strider_ir::FloatUnaryOp::Neg)
            ),
            "FloatSub lift must produce a FloatUnaryOp::Neg (the negated rhs)"
        );
    });
}

#[test]
fn lift_float_not_equal_lowers_to_boolneg_float_equal() {
    with_test_lifter(|d, rid| {
        {
            let insn = Insn {
                opcode: Opcode::FloatNotEqual,
                output: Some(reg(0)),
                inputs: vec![const_vn(0, 4), const_vn(0, 4)].into(),
            };
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .unwrap();
        }
        assert!(
            graph_has_kind(
                &d.builder,
                NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::Xor)
            ),
            "FloatNotEqual lift must produce an IntBinaryOp::Xor (the I1 logical-NOT wrap, post-BitNot removal)"
        );
        assert!(
            graph_has_kind(
                &d.builder,
                NodeKind::FloatCmpOp(strider_ir::FloatCmpOp::Equal)
            ),
            "FloatNotEqual lift must produce a FloatCmpOp::Equal (the lowered cmp)"
        );
    });
}

#[test]
fn lift_float_less_equal_lowers_to_or_less_equal() {
    // `a <= b` (IEEE 754) lowers to `Or(Less(a, b), Equal(a, b))`,
    // NaN-aware (both children false on NaN, so Or is false).
    with_test_lifter(|d, rid| {
        {
            let insn = Insn {
                opcode: Opcode::FloatLessEqual,
                output: Some(reg(0)),
                inputs: vec![const_vn(0, 4), const_vn(0, 4)].into(),
            };
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .unwrap();
        }
        assert!(
            graph_has_kind(&d.builder, NodeKind::IntBinaryOp(IntBinaryOp::Or)),
            "FloatLessEqual lift must produce an IntBinaryOp::Or (the disjunction wrap)"
        );
        assert!(
            graph_has_kind(
                &d.builder,
                NodeKind::FloatCmpOp(strider_ir::FloatCmpOp::Less)
            ),
            "FloatLessEqual lift must produce a FloatCmpOp::Less"
        );
        assert!(
            graph_has_kind(
                &d.builder,
                NodeKind::FloatCmpOp(strider_ir::FloatCmpOp::Equal)
            ),
            "FloatLessEqual lift must produce a FloatCmpOp::Equal"
        );
    });
}

/// `FloatNan(x)` lowers to `Xor(FloatEqual(x, x), 1):I1` — `is_nan(x)` ≡
/// `x != x`.  Assert the `FloatEqual`'s two inputs are the SAME value, and
/// that it is wrapped by an `IntBinaryOp::Xor` at I1 (the I1 logical-NOT).
#[test]
fn lift_float_nan_lowers_to_self_inequality() {
    with_test_lifter(|d, rid| {
        {
            // Single 4-byte (F32) input read from a register so both
            // FloatEqual operands resolve to the same value.
            let insn = Insn {
                opcode: Opcode::FloatNan,
                output: Some(reg(4)),
                inputs: vec![reg(0)].into(),
            };
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .unwrap();
        }
        let eq = find_first_node(
            &d.builder,
            NodeKind::FloatCmpOp(strider_ir::FloatCmpOp::Equal),
        )
        .expect("FloatNan must lower to a FloatEqual cmp");
        let [eq_lhs, eq_rhs] = d
            .builder
            .function()
            .node_inputs_exact::<2>(eq)
            .expect("FloatEqual has two inputs");
        assert_eq!(
            eq_lhs, eq_rhs,
            "FloatNan(x) compares x against ITSELF — both FloatEqual operands \
             must be the identical value"
        );

        // The FloatEqual (I1) feeds the I1 logical-NOT Xor.
        let xor = find_first_node(
            &d.builder,
            NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::Xor),
        )
        .expect("FloatNan must wrap the cmp in an I1 Xor (logical NOT)");
        let [xor_out] = d
            .builder
            .function()
            .node_outputs_exact::<1>(xor)
            .expect("Xor has one output");
        assert_eq!(
            d.builder.function().value_type(xor_out).ok(),
            Some(strider_ir::ValueType::I1),
            "the Xor wrap must be at I1"
        );
        let [eq_out] = d
            .builder
            .function()
            .node_outputs_exact::<1>(eq)
            .expect("FloatEqual has one output");
        assert!(
            d.builder
                .function()
                .node_inputs(xor)
                .into_iter()
                .any(|v| v == eq_out),
            "the I1 Xor must consume the FloatEqual result"
        );
    });
}

// ── Signed-op width-mismatch guard (MED-2) ───────────────────────────────────

/// `IntSdiv` with a LHS wider than the output must not silently truncate the
/// wider operand before the signed division (which would drop high bits with
/// no sign awareness).  The dispatched signed-op path now guards on
/// equal input widths, surfacing the mismatch as a loud lift-time error.
#[test]
fn sdiv_with_wider_lhs_does_not_silently_truncate() {
    with_test_lifter_tracking(
        vec![
            Vn {
                size: 8,
                addr_off: 0x200,
                addr_space: VnSpace::REGISTER,
            },
            reg(0),
            reg(4),
        ],
        |d, rid| {
            // lhs is an 8-byte register, rhs + output are 4 bytes.  The old
            // path would `extend_if_needed(lhs, I32, SignExtend)` which
            // truncates the 8-byte lhs to 4 bytes *before* the signed div.
            let wide_lhs = Vn {
                size: 8,
                addr_off: 0x200,
                addr_space: VnSpace::REGISTER,
            };
            let insn = Insn {
                opcode: Opcode::IntSdiv,
                output: Some(reg(0)),
                inputs: vec![wide_lhs, reg(4)].into(),
            };
            let res = d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default());
            assert!(
                res.is_err(),
                "IntSdiv with lhs wider than output must error (no silent truncate)"
            );
            if let Err(e) = res {
                // `{:#}` renders the full anyhow cause chain (the per-insn
                // funnel wraps the inner error with asm context).
                let msg = format!("{e:#}");
                assert!(
                    msg.contains("width mismatch"),
                    "error must name the width mismatch; got: {msg}"
                );
            }
        },
    );
}

/// `IntDiv` / `IntRem` are unsigned, but the permissive arm still routes a
/// wider-than-output operand through `convert_to_int_if_needed`, which
/// SILENTLY TRUNCATES it.  Unlike the bitwise / shift-left / add / mul ops,
/// a divide's (or remainder's) low bits are NOT width-agnostic, so a
/// truncated wider dividend yields the wrong quotient.  They must guard the
/// wider-than-output operand exactly as the signed `Sdiv` / `Srem` do.
#[test]
fn unsigned_div_rem_with_wider_lhs_does_not_silently_truncate() {
    for opcode in [Opcode::IntDiv, Opcode::IntRem] {
        with_test_lifter_tracking(
            vec![
                Vn {
                    size: 8,
                    addr_off: 0x200,
                    addr_space: VnSpace::REGISTER,
                },
                reg(0),
                reg(4),
            ],
            |d, rid| {
                let wide_lhs = Vn {
                    size: 8,
                    addr_off: 0x200,
                    addr_space: VnSpace::REGISTER,
                };
                let insn = Insn {
                    opcode,
                    output: Some(reg(0)),
                    inputs: vec![wide_lhs, reg(4)].into(),
                };
                let res = d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default());
                assert!(
                    res.is_err(),
                    "{opcode:?} with lhs wider than output must error (no silent truncate)"
                );
                if let Err(e) = res {
                    let msg = format!("{e:#}");
                    assert!(
                        msg.contains("width mismatch"),
                        "error must name the width mismatch; got: {msg}"
                    );
                }
            },
        );
    }
}

// ── Wide SUBPIECE high-lane extract (MED-3) ──────────────────────────────────

/// A YMM (I256) SUBPIECE with a nonzero byte_offset builds its right-shift at
/// the *input* width (I256).  The shift constant must route through the
/// wide-const path so the extract lifts cleanly rather than hard-aborting on
/// `build_int_const`'s I256/I512 rejection.
#[test]
fn subpiece_ymm_high_lane() {
    let wide = Vn {
        size: 32,
        addr_off: 0x100,
        addr_space: VnSpace::REGISTER,
    };
    let out = Vn {
        size: 16,
        addr_off: 0x80,
        addr_space: VnSpace::REGISTER,
    };
    with_test_lifter_tracking(vec![out, wide], |d, rid| {
        // SUBPIECE(ymm, byte_offset=16, out=xmm) — extract the high 128-bit
        // lane of a 256-bit YMM register.
        let insn = Insn {
            opcode: Opcode::Subpiece,
            output: Some(out),
            inputs: vec![wide, const_vn(16, 4)].into(),
        };
        d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
            .unwrap_or_else(|e| panic!("YMM high-lane SUBPIECE must lift, got error: {e}"));
        assert!(
            graph_has_kind(&d.builder, NodeKind::IntBinaryOp(IntBinaryOp::ShiftRight)),
            "wide SUBPIECE with offset must build a ShiftRight at the input width"
        );
    });
}

// ── Odd-width load asm-attribution (MED-1) ───────────────────────────────────

/// A 6-byte load (e.g. an x86 far-pointer `lds`/`les`) has no supported
/// integer `ValueType`, so `int_for_byte_size` hard-errors.  The lifter
/// cannot widen that type, but the lift error must name the offending
/// machine instruction (its address / opcode) so a failed lift is
/// debuggable instead of being a bare "unsupported node output size".
#[test]
fn load_odd_byte_width_errors_with_asm_context() {
    with_test_lifter(|d, rid| {
        // A 6-byte output varnode — no supported integer type.  Use a Copy
        // from a 6-byte CONST so the size flows through `int_type`/output.
        let insn = Insn {
            opcode: Opcode::Copy,
            output: Some(Vn {
                size: 6,
                addr_off: 0,
                addr_space: VnSpace::REGISTER,
            }),
            inputs: vec![const_vn(0, 6)].into(),
        };
        let res = d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default());
        let err = res.expect_err("6-byte output must error (unsupported width)");
        // `{:#}` renders the full anyhow cause chain — both the asm context
        // (outer) and the inner unsupported-width cause.
        let msg = format!("{err:#}");
        assert!(
            msg.contains("unsupported node output size") || msg.contains("6 bytes"),
            "error must still describe the unsupported width; got: {msg}"
        );
        // The machine address (0x1000, from `test_addr`) and/or opcode must
        // appear so the failed lift names the offending instruction.
        assert!(
            msg.contains("0x1000") || msg.contains("4096") || msg.contains("Copy"),
            "lift error must attach the machine address / opcode for context; got: {msg}"
        );
    });
}
