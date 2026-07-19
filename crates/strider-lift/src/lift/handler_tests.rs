//! Value-opcode lifting tests over hand-built `rsleigh::Insn` structs, not
//! bytes decoded from a binary.  The CFG and calling convention are throwaway
//! scaffolding: value lifting touches only the IR builder and Sleigh context,
//! and never consults the region id or the region map.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use rsleigh::mem_readers::BufMemReader;
use rsleigh::{Insn, Opcode, Vn, VnSpace};

use strider_ir::node::{NodeId, NodeKind};
use strider_ir::{FunctionBuilder, IRBuilderExt, IRViewer, IntBinaryOp, IntCmpOp, IntUnaryOp};

use crate::lift::{FunctionLifter, Lifter};

type TestReader = BufMemReader<Vec<u8>>;

const TEST_ENDIAN: strider_target::Endianness = strider_target::Endianness::Little;

/// 4-byte register at the given REGISTER-space offset.
fn reg(off: u64) -> Vn {
    Vn {
        size: 4,
        addr_off: off,
        addr_space: VnSpace::REGISTER,
    }
}

fn const_vn(val: u64, size: u32) -> Vn {
    Vn {
        size,
        addr_off: val,
        addr_space: VnSpace::CONST,
    }
}

/// Throwaway address for tests that don't inspect asm fingerprints.  The
/// funnel's bracket leaves the builder back at `lift_addr = None`.
fn test_addr() -> strider_cfg::PcodeInsnAddr {
    strider_cfg::PcodeInsnAddr::at_machine_start(0x1000)
}

/// No args, no clobbers.  The value handlers never consult the convention.
/// Struct-literal construction skips ABI-disjointness validation, fine here.
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
        no_return: false,
    }
}

/// Runs `f` with a lifter tracking three 4-byte REGISTER vars at 0/4/8 and a
/// single entry region.  The helper owns every borrowed local, so the lifter
/// that borrows them need not be returned.
fn with_test_lifter(f: impl FnOnce(&mut FunctionLifter<'_, TestReader>, strider_cfg::RegionId)) {
    with_test_lifter_tracking(vec![reg(0), reg(4), reg(8)], f);
}

/// Explicit tracked-varnode list, for tests using registers wider than the
/// default 4-byte regs.  `all_vns` must be pre-sorted by
/// `(space, offset, size)`.
fn with_test_lifter_tracking(
    all_vns: Vec<Vn>,
    f: impl FnOnce(&mut FunctionLifter<'_, TestReader>, strider_cfg::RegionId),
) {
    with_test_lifter_tracking_arch(strider_target::SleighArch::x86(), vec![0xc3], all_vns, f);
}

/// Arbitrary `arch`, so the aliasing tests can exercise `read_reg_vn` /
/// `write_reg_vn` under both endiannesses.  `term_bytes` must terminate the
/// single-instruction throwaway CFG cleanly (`0xc3` ret on x86, `4e 80 00 20`
/// blr on big-endian PowerPC).
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
    // Value opcodes never consult the region id or map, so any valid id works.
    let region_id = cfg.entry();
    let cc = empty_cc();
    let lifter = Lifter::new(arch, sleigh).expect("lifter");
    let no_overrides = rustc_hash::FxHashMap::default();
    let mut driver =
        FunctionLifter::new(&lifter, cc, &cfg, all_vns, &no_overrides).expect("driver");
    // Clear the lift address so tests start from `lift_addr = None`.
    driver.builder.set_lift_addr(None);
    driver.builder.build_entry().expect("build_entry");
    let region = driver.builder.create_region_all().expect("create_region");
    driver
        .builder
        .set_entry_region_all(region)
        .expect("set_entry_region");
    driver.builder.set_region(region);
    f(&mut driver, region_id);
}

/// Caller-provided CC instead of `empty_cc()`.  The projection tests need this:
/// the other helpers seed `empty_cc`'s `stack_vn` (0x9000) into the tracked
/// set, and a test CC that neither owns nor callee-saves it would misclassify
/// it as an extra clobber, polluting exact clobber-list assertions.
pub(super) fn with_test_lifter_cc(
    cc: strider_target::BuiltCallingConvention,
    all_vns: Vec<Vn>,
    f: impl FnOnce(&mut FunctionLifter<'_, TestReader>, strider_cfg::RegionId),
) {
    let arch = strider_target::SleighArch::x86();
    let mut sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        BufMemReader::new(vec![0xc3], 0x1000),
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
    let region_id = cfg.entry();
    let lifter = Lifter::new(arch, sleigh).expect("lifter");
    let no_overrides = rustc_hash::FxHashMap::default();
    let mut driver =
        FunctionLifter::new(&lifter, cc, &cfg, all_vns, &no_overrides).expect("driver");
    driver.builder.set_lift_addr(None);
    driver.builder.build_entry().expect("build_entry");
    let region = driver.builder.create_region_all().expect("create_region");
    driver
        .builder
        .set_entry_region_all(region)
        .expect("set_entry_region");
    driver.builder.set_region(region);
    f(&mut driver, region_id);
}

/// Lifts one hand-built `Insn` and asserts it succeeds.
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

#[test]
fn lift_int_mul_of_consts() {
    assert_lifts_one(
        Opcode::IntMul,
        Some(reg(0)),
        vec![const_vn(6, 4), const_vn(7, 4)],
    );
}

/// Narrow `IntNeg` lowers to `Xor(x, all_ones)` with the constant inline.
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
        assert!(
            find_int_const_node(&d.builder, 0xFFFF_FFFF).is_some(),
            "narrow IntNeg must materialise the I32 all-ones constant"
        );
    });
}

/// A YMM/ZMM-wide `IntNeg` is a well-defined bitwise complement and must lift,
/// with the all-ones operand routed through the wide-const path rather than
/// `build_int_const`, which rejects I256/I512.
#[test]
fn lift_int_neg_register_wide_lowers_to_xor_all_ones() {
    let cases: [(&str, u32, strider_ir::ValueType); 2] = [
        ("ymm_i256", 32, strider_ir::ValueType::I256),
        ("zmm_i512", 64, strider_ir::ValueType::I512),
    ];
    for (label, width, _ty) in cases {
        // Its own container, so direct access with no sub-register masking.
        // Offset clear of the default regs at 0/4/8, and tracked so the read
        // resolves to its InitialVar.
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

#[test]
fn lift_truncate_extracts_low_bits() {
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
    // lsb 28 + len 8 = 36 bits, past the 32-bit input.
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

#[test]
fn lift_float_add_of_consts() {
    with_test_lifter(|d, rid| {
        let insn = Insn {
            opcode: Opcode::FloatAdd,
            output: Some(reg(0)),
            // 4-byte varnodes read as F32; const space carries any bits.
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

// `Load` is deliberately untested here: `VnSpace::by_id` expects inputs[0] to
// hold a raw pointer to a Sleigh AddrSpace, which a synthetic test cannot
// construct safely.  The per-arch tests cover real-decoded Loads.

#[test]
fn lift_segment_op_recognised() {
    assert_lifts_one(
        Opcode::SegmentOp,
        Some(reg(0)),
        vec![const_vn(0, 4), const_vn(0, 2), const_vn(0, 4)],
    );
}

fn graph_has_kind(builder: &FunctionBuilder, target: NodeKind) -> bool {
    builder
        .function()
        .graph()
        .all_node_ids()
        .any(|id| builder.function().node_kind(id) == &target)
}

fn find_first_node(builder: &FunctionBuilder, target: NodeKind) -> Option<NodeId> {
    builder
        .function()
        .graph()
        .all_node_ids()
        .find(|id| builder.function().node_kind(*id) == &target)
}

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
    // A 2-byte dividend 0xFFFF is -1, so a signed op must SIGN-extend it to
    // 0xFFFF_FFFF at the 4-byte op width, not zero-extend it to 0x0000_FFFF.
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
            find_int_const_node(&d.builder, 0xFFFF_FFFF).is_some(),
            "the 2-byte -1 dividend must be SIGN-extended to the 4-byte op width (0xFFFF_FFFF)"
        );
    });
}

#[test]
fn int_signed_cmp_uses_max_width_and_sign_extends_narrower_operand() {
    // Compare at the MAX of the two widths so the 8-byte operand is never
    // truncated, and sign-extend the 4-byte -1 so it stays
    // 0xFFFF_FFFF_FFFF_FFFF rather than 0x0000_0000_FFFF_FFFF.
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
            find_int_const_node(&d.builder, 0xFFFF_FFFF_FFFF_FFFF).is_some(),
            "the 4-byte -1 operand must be SIGN-extended to the 8-byte max width \
             (0xFFFF_FFFF_FFFF_FFFF) — proving max-width comparison + sign-correct extension"
        );
    });
}

#[test]
fn lift_with_set_lift_addr_records_asm_fingerprint() {
    with_test_lifter(|d, rid| {
        // `process_insn` owns the funnel, so drive the fingerprint via its
        // `addr` argument rather than a manual `set_lift_addr`.
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
        let fp = d.builder.function().side_tables().asm_fingerprint(add_node);
        assert_eq!(
            fp,
            rustc_hash::FxHashSet::from_iter([0x4242]),
            "Add node fingerprint should record 0x4242"
        );
        let const3 = find_int_const_node(&d.builder, 3).expect("IntConst(3) must be present");
        let const4 = find_int_const_node(&d.builder, 4).expect("IntConst(4) must be present");
        assert_eq!(
            d.builder.function().side_tables().asm_fingerprint(const3),
            rustc_hash::FxHashSet::from_iter([0x4242])
        );
        assert_eq!(
            d.builder.function().side_tables().asm_fingerprint(const4),
            rustc_hash::FxHashSet::from_iter([0x4242])
        );
    });
}

#[test]
fn lift_without_lift_addr_leaves_fingerprint_empty() {
    // Pins the funnel's reset arm: a node built after `process_insn` returns
    // carries an empty fingerprint.
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
        let outside = d
            .builder
            .build_int_const(0x55u64, strider_ir::ValueType::I32)
            .unwrap();
        let outside_node = d.builder.function().producer(outside);
        assert!(
            d.builder
                .function()
                .side_tables()
                .asm_fingerprint(outside_node)
                .is_empty(),
            "a node built after process_insn returns should have an empty fingerprint \
             (the funnel reset lift_addr to None)"
        );
    });
}

#[test]
fn lift_dedup_unions_two_addresses() {
    // The same insn from two addresses dedups to one NodeId, so both
    // contributors must be unioned into its fingerprint.
    with_test_lifter(|d, rid| {
        let insn = Insn {
            opcode: Opcode::IntAdd,
            output: Some(reg(0)),
            inputs: vec![const_vn(3, 4), const_vn(4, 4)].into(),
        };
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
        let fp = d.builder.function().side_tables().asm_fingerprint(add_node);
        assert_eq!(
            fp,
            rustc_hash::FxHashSet::from_iter([0x1000, 0x2000]),
            "both addresses should be unioned"
        );
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
        // Canonical shape is `Xor(IntLess(_, _), IntConst(1)):I1`.
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

/// The lowering SWAPS operands.  Two distinct register operands are needed to
/// see it: the const-fed tests above cannot catch a dropped swap.
#[test]
fn lift_int_less_equal_swaps_operands() {
    with_test_lifter(|d, rid| {
        {
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
        // An entry-region register read materialises as a Phi whose
        // source-varnode tag names the register.
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

/// Variable operands are the strict dedup case: constant-operand lifts dedup
/// trivially because the `IntConst` keys match.  Guards against the lowering
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
            // Same inputs, different output reg.
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

/// Const-operand companion to the variable-operand cache test.
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
            // Same operands, different output reg.
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

/// Lifts an operand-less `Insn` and asserts the dispatch result.  `expect_ok`
/// is false for handlers that read an operand and so error on an absent one.
/// `label` appears in the failure message.
fn assert_process_insn(opcode: Opcode, expect_ok: bool, label: &str) {
    with_test_lifter(|d, rid| {
        let insn = Insn {
            opcode,
            output: None,
            inputs: Default::default(),
        };
        let res = d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default());
        assert_eq!(res.is_ok(), expect_ok, "{label}");
    });
}

#[test]
fn process_insn_no_operand_dispatch_routing() {
    // The empty region map is never consulted: CondBranch errors on its
    // missing condition operand before any lookup.
    let cases: &[(Opcode, bool, &str)] = &[
        (
            Opcode::Branch,
            true,
            "Branch dispatches to the no-op handle_branch",
        ),
        (
            Opcode::CondBranch,
            false,
            "CondBranch reads its condition operand and errors when absent",
        ),
        (
            Opcode::BranchIndirect,
            true,
            "BranchIndirect shares the CC Return handler (link-register return)",
        ),
        (
            Opcode::Return,
            true,
            "Return dispatches to the CC return handler",
        ),
        (
            Opcode::Call,
            false,
            "Call reads its target operand and errors when absent",
        ),
        (
            Opcode::CallIndirect,
            false,
            "CallIndirect reads its target operand and errors when absent",
        ),
        (
            Opcode::CallOther,
            false,
            "CallOther reads its user-op id operand and errors when absent",
        ),
        (
            Opcode::Store,
            false,
            "Store reads its address/data operands and errors when absent",
        ),
        (Opcode::Nop, true, "Nop dispatches to the empty arm"),
        (
            Opcode::CallOther,
            false,
            "CallOther dispatches to handle_call_other and errors on the missing user-op id",
        ),
    ];
    for &(opcode, expect_ok, label) in cases {
        assert_process_insn(opcode, expect_ok, label);
    }
}

#[test]
fn read_vn_unknown_returns_initial_var_or_phi() {
    // Either kind is correct: `InitialVar` is the entry value, `Phi` the
    // merge node the builder lazily inserts at a region entry pointing back to
    // it.  What matters is that the producer is not some arithmetic node.
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
        let const_42 = d
            .builder
            .build_int_const(42u64, strider_ir::ValueType::I32)
            .unwrap();
        d.write_vn(&reg(0), const_42).expect("write_vn");
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

#[test]
fn lift_subpiece_out_of_range_errors() {
    with_test_lifter(|d, rid| {
        let insn = Insn {
            opcode: Opcode::Subpiece,
            output: Some(Vn {
                size: 1,
                addr_off: 0,
                addr_space: VnSpace::REGISTER,
            }),
            // byte_offset 5 exceeds the 4-byte input.
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
    // Guards the checked-accessor conversion: a missing inputs[1] must error,
    // not panic on an out-of-bounds index.
    with_test_lifter(|d, rid| {
        let insn = Insn {
            opcode: Opcode::IntAdd,
            output: Some(reg(0)),
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
    // NaN-aware: both children are false on NaN, so the Or is false.
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

/// `FloatNan(x)` lowers to `Xor(FloatEqual(x, x), 1):I1`, so the cmp's two
/// inputs must be the SAME value and the wrap must be at I1.
#[test]
fn lift_float_nan_lowers_to_self_inequality() {
    with_test_lifter(|d, rid| {
        {
            // Read from a register so both operands resolve to one value.
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

/// A LHS wider than the output must error rather than be truncated before the
/// signed division, which would drop high bits with no sign awareness.
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
            // 8-byte lhs, 4-byte rhs and output: `extend_if_needed` cannot
            // narrow, so an unguarded path truncates before the divide.
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
                // `{:#}` renders the full cause chain past the asm context.
                let msg = format!("{e:#}");
                assert!(
                    msg.contains("width mismatch"),
                    "error must name the width mismatch; got: {msg}"
                );
            }
        },
    );
}

/// The unsigned divide and remainder need the same guard as their signed
/// counterparts: their low bits are not width-agnostic, so a silently
/// truncated wider dividend yields the wrong quotient.
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

/// A wide SUBPIECE with a nonzero byte_offset shifts at the INPUT width, so the
/// shift constant must route through the wide-const path rather than hitting
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
        // Extract the high 128-bit lane of a 256-bit YMM.
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

/// A 7-byte width has no integer `ValueType`, so the lift must fail.  The
/// error has to name the offending machine instruction, otherwise a failed
/// whole-function lift is just a bare "unsupported node output size".
#[test]
fn load_odd_byte_width_errors_with_asm_context() {
    with_test_lifter(|d, rid| {
        // Copy from a 7-byte CONST so the size flows through `int_type`.
        let insn = Insn {
            opcode: Opcode::Copy,
            output: Some(Vn {
                size: 7,
                addr_off: 0,
                addr_space: VnSpace::REGISTER,
            }),
            inputs: vec![const_vn(0, 7)].into(),
        };
        let res = d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default());
        let err = res.expect_err("7-byte output must error (unsupported width)");
        // `{:#}` renders the outer asm context and the inner width cause.
        let msg = format!("{err:#}");
        assert!(
            msg.contains("unsupported node output size") || msg.contains("7 bytes"),
            "error must still describe the unsupported width; got: {msg}"
        );
        assert!(
            msg.contains("0x1000") || msg.contains("4096") || msg.contains("Copy"),
            "lift error must attach the machine address / opcode for context; got: {msg}"
        );
    });
}
