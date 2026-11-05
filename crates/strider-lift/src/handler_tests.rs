use rsleigh::mem_readers::BufMemReader;
use rsleigh::{Insn, Opcode, Vn, VnSpace};

use strider_ir::node::{NodeId, NodeKind, ValueType};
use strider_ir::{FunctionBuilder, IRBuilderExt, IRViewer, IntBinaryOp, IntCmpOp, IntUnaryOp};

use strider_ir_test_utils::reg_vn;

use crate::{FunctionLifter, Lifter};

type TestReader = BufMemReader<Vec<u8>>;

/// 4-byte register at the given REGISTER-space offset.
fn reg(off: u64) -> Vn {
    reg_vn(off, 4)
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
    strider_target::BuiltCallingConvention {
        arg_passing_regs: Vec::new(),
        callee_saved_regs: Vec::new(),
        ret_val_regs: Vec::new(),
        ret_val_regs_float: Vec::new(),
        stack_ptr: Vn {
            size: 4,
            addr_off: 0x9000,
            addr_space: VnSpace::REGISTER,
        },
        stack_args: None,
        ret_stack_pop: 0,
        link_register: None,
        preserves_memory: false,
        preserves_all_registers: false,
        ..Default::default()
    }
}

/// Runs `f` with a lifter tracking three 4-byte REGISTER vars at 0/4/8 and a
/// single entry region.  The helper owns every borrowed local, so the lifter
/// that borrows them need not be returned.
fn with_test_lifter(f: impl FnOnce(&mut FunctionLifter<'_, TestReader>, strider_cfg::RegionId)) {
    with_test_lifter_tracking(vec![reg(0), reg(4), reg(8)], f);
}

/// Explicit tracked-varnode list, for tests using registers wider than the
/// default 4-byte regs.
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
    with_test_lifter_arch_cc(
        arch,
        term_bytes,
        empty_cc(),
        all_vns,
        &rustc_hash::FxHashMap::default(),
        f,
    );
}

/// Caller-provided CC instead of `empty_cc()`.  The projection tests need this:
/// the other helpers seed `empty_cc`'s `stack_ptr` (0x9000) into the tracked
/// set, and a test CC that neither owns nor callee-saves it would misclassify
/// it as an extra clobber, polluting exact clobber-list assertions.
pub(super) fn with_test_lifter_cc(
    cc: strider_target::BuiltCallingConvention,
    all_vns: Vec<Vn>,
    f: impl FnOnce(&mut FunctionLifter<'_, TestReader>, strider_cfg::RegionId),
) {
    with_test_lifter_arch_cc(
        strider_target::SleighArch::x86(),
        vec![0xc3],
        cc,
        all_vns,
        &rustc_hash::FxHashMap::default(),
        f,
    );
}

fn with_test_lifter_arch_cc(
    arch: strider_target::SleighArch,
    term_bytes: Vec<u8>,
    cc: strider_target::BuiltCallingConvention,
    all_vns: Vec<Vn>,
    per_address_ccs: &rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>,
    f: impl FnOnce(&mut FunctionLifter<'_, TestReader>, strider_cfg::RegionId),
) {
    let mut lifter = Lifter::new(arch, BufMemReader::new(term_bytes, 0x1000)).expect("lifter");
    let cfg = lifter
        .build_cfg(0x1000u64.into(), &strider_cfg::CfgOptions::default())
        .expect("throwaway cfg");
    // Value opcodes never consult the region id or map, so any valid id works.
    let region_id = cfg.entry();
    let cfg_opts = strider_cfg::CfgOptions::default();
    let conventions = lifter
        .convention_vns(&cfg, &cc, &cfg_opts, per_address_ccs)
        .expect("conventions");
    let mut driver = FunctionLifter::new(
        &lifter,
        cc,
        &cfg,
        all_vns.into_iter().chain(conventions).collect(),
        per_address_ccs,
        &cfg_opts,
        &[],
    )
    .expect("driver");
    driver.builder.set_lift_addr(None);
    let region = driver.builder.create_region_all().expect("create_region");
    driver
        .builder
        .set_entry_region_all(region)
        .expect("set_entry_region");
    driver.builder.set_region(region);
    f(&mut driver, region_id);
}

/// `value`'s producer tree: a tracked register's entry value prints as
/// `r<offset>`, a constant as `<hex>:<type>`, any other node as
/// `<kind>:<type>(<operands>)`.
fn operand_tree(f: &strider_ir::Function, value: strider_ir::node::ValueId) -> String {
    let node = f.producer(value);
    let ty = f.value_type(value).expect("a typed value");
    match f.node_kind(node) {
        NodeKind::IntConst(_) => format!(
            "{:#x}:{ty}",
            f.int_const_u128(value).expect("a narrow constant")
        ),
        NodeKind::InitialVar(id) => format!("r{}", f.initial_vn(*id).addr_off),
        NodeKind::Phi => format!(
            "r{}",
            f.get_vn_for_value(value).expect("a register phi").addr_off
        ),
        kind => {
            let operands: Vec<String> = f.value_inputs(node).map(|v| operand_tree(f, v)).collect();
            format!("{kind:?}:{ty}({})", operands.join(", "))
        }
    }
}

/// Each plain opcode's lowering, read back from the varnode it wrote.
#[test]
fn plain_opcodes_lower_to_their_ir_shape() {
    let (r0, r4, r8) = (reg(0), reg(4), reg(8));
    let (b16, b17, b18) = (reg_vn(16, 1), reg_vn(17, 1), reg_vn(18, 1));
    let (q24, q32, q40) = (reg_vn(24, 8), reg_vn(32, 8), reg_vn(40, 8));
    let cases: &[(Opcode, Vn, &[Vn], &str)] = &[
        (Opcode::IntZext, r8, &[b16], "Extend(ZeroExtend):i32(r16)"),
        (Opcode::IntSext, r8, &[b16], "Extend(SignExtend):i32(r16)"),
        (Opcode::IntZext, r8, &[const_vn(0xff, 1)], "0xff:i32"),
        (Opcode::IntSext, r8, &[const_vn(0xff, 1)], "0xffffffff:i32"),
        (Opcode::Copy, r8, &[r0], "r0"),
        (Opcode::Copy, r8, &[const_vn(42, 4)], "0x2a:i32"),
        (Opcode::Popcount, r8, &[r0], "Popcount:i32(r0)"),
        (Opcode::Lzcount, r8, &[r0], "Lzcount:i32(r0)"),
        (
            Opcode::BoolAnd,
            b18,
            &[b16, b17],
            "Extend(ZeroExtend):i8(IntBinaryOp(And):i1(Truncate:i1(r16), Truncate:i1(r17)))",
        ),
        (
            Opcode::BoolOr,
            b18,
            &[b16, b17],
            "Extend(ZeroExtend):i8(IntBinaryOp(Or):i1(Truncate:i1(r16), Truncate:i1(r17)))",
        ),
        (
            Opcode::BoolXor,
            b18,
            &[b16, b17],
            "Extend(ZeroExtend):i8(IntBinaryOp(Xor):i1(Truncate:i1(r16), Truncate:i1(r17)))",
        ),
        (
            Opcode::BoolNeg,
            b18,
            &[b16],
            "Extend(ZeroExtend):i8(IntBinaryOp(Xor):i1(Truncate:i1(r16), 0x1:i1))",
        ),
        (
            Opcode::Subpiece,
            b18,
            &[const_vn(0x1234_5678, 4), const_vn(0, 4)],
            "0x78:i8",
        ),
        (
            Opcode::Subpiece,
            b18,
            &[r0, const_vn(1, 4)],
            "Truncate:i8(IntBinaryOp(ShiftRight):i32(r0, 0x8:i32))",
        ),
        (
            Opcode::BoolNeg,
            b18,
            &[const_vn(1, 1)],
            "Extend(ZeroExtend):i8(IntBinaryOp(Xor):i1(0x1:i1, 0x1:i1))",
        ),
        (
            Opcode::Piece,
            r8,
            &[const_vn(0xAA, 2), const_vn(0x80BB, 2)],
            "IntBinaryOp(Or):i32(IntBinaryOp(ShiftLeft):i32(0xaa:i32, 0x10:i32), 0x80bb:i32)",
        ),
        (
            Opcode::IntCarry,
            b18,
            &[r0, r4],
            "Extend(ZeroExtend):i8(IntCmpOp(Carry):i1(r0, r4))",
        ),
        (
            Opcode::IntEqual,
            b18,
            &[r0, r4],
            "Extend(ZeroExtend):i8(IntCmpOp(Equal):i1(r0, r4))",
        ),
        (
            Opcode::IntLess,
            b18,
            &[r0, r4],
            "Extend(ZeroExtend):i8(IntCmpOp(Less):i1(r0, r4))",
        ),
        (
            Opcode::IntSless,
            b18,
            &[r0, r4],
            "Extend(ZeroExtend):i8(IntCmpOp(Sless):i1(r0, r4))",
        ),
        (
            Opcode::IntScarry,
            b18,
            &[r0, r4],
            "Extend(ZeroExtend):i8(IntCmpOp(Scarry):i1(r0, r4))",
        ),
        (
            Opcode::IntSborrow,
            b18,
            &[r0, r4],
            "Extend(ZeroExtend):i8(IntCmpOp(Sborrow):i1(r0, r4))",
        ),
        (Opcode::Int2Comp, r8, &[r0], "IntUnaryOp(Neg):i32(r0)"),
        (
            Opcode::FloatAdd,
            r8,
            &[r0, r4],
            "FloatBitsToInt:i32(FloatBinaryOp(Add):f32(IntBitsToFloat:f32(r0), IntBitsToFloat:f32(r4)))",
        ),
        (
            Opcode::FloatMul,
            r8,
            &[r0, r4],
            "FloatBitsToInt:i32(FloatBinaryOp(Mul):f32(IntBitsToFloat:f32(r0), IntBitsToFloat:f32(r4)))",
        ),
        (
            Opcode::FloatDiv,
            r8,
            &[r0, r4],
            "FloatBitsToInt:i32(FloatBinaryOp(Div):f32(IntBitsToFloat:f32(r0), IntBitsToFloat:f32(r4)))",
        ),
        (
            Opcode::FloatNeg,
            r8,
            &[r0],
            "FloatBitsToInt:i32(FloatUnaryOp(Neg):f32(IntBitsToFloat:f32(r0)))",
        ),
        (
            Opcode::FloatAbs,
            r8,
            &[r0],
            "FloatBitsToInt:i32(FloatUnaryOp(Abs):f32(IntBitsToFloat:f32(r0)))",
        ),
        (
            Opcode::FloatSqrt,
            r8,
            &[r0],
            "FloatBitsToInt:i32(FloatUnaryOp(Sqrt):f32(IntBitsToFloat:f32(r0)))",
        ),
        (
            Opcode::FloatCeil,
            r8,
            &[r0],
            "FloatBitsToInt:i32(FloatUnaryOp(Ceil):f32(IntBitsToFloat:f32(r0)))",
        ),
        (
            Opcode::FloatFloor,
            r8,
            &[r0],
            "FloatBitsToInt:i32(FloatUnaryOp(Floor):f32(IntBitsToFloat:f32(r0)))",
        ),
        (
            Opcode::FloatRound,
            r8,
            &[r0],
            "FloatBitsToInt:i32(FloatUnaryOp(Round):f32(IntBitsToFloat:f32(r0)))",
        ),
        (
            Opcode::FloatEqual,
            b18,
            &[r0, r4],
            "Extend(ZeroExtend):i8(FloatCmpOp(Equal):i1(IntBitsToFloat:f32(r0), IntBitsToFloat:f32(r4)))",
        ),
        (
            Opcode::FloatLess,
            b18,
            &[r0, r4],
            "Extend(ZeroExtend):i8(FloatCmpOp(Less):i1(IntBitsToFloat:f32(r0), IntBitsToFloat:f32(r4)))",
        ),
        (
            Opcode::FloatInt2Float,
            q24,
            &[r0],
            "FloatBitsToInt:i64(IntToFloat:f64(r0))",
        ),
        (
            Opcode::FloatFloat2Float,
            q24,
            &[r0],
            "FloatBitsToInt:i64(FloatToFloat:f64(IntBitsToFloat:f32(r0)))",
        ),
        (
            Opcode::FloatTrunc,
            r8,
            &[q24],
            "FloatToInt:i32(IntBitsToFloat:f64(r24))",
        ),
        (Opcode::Cast, r8, &[r0], "r0"),
        (
            Opcode::IntNotEqual,
            b18,
            &[r0, r4],
            "Extend(ZeroExtend):i8(IntBinaryOp(Xor):i1(IntCmpOp(Equal):i1(r0, r4), 0x1:i1))",
        ),
        (
            Opcode::FloatAdd,
            q40,
            &[q24, q32],
            "FloatBitsToInt:i64(FloatBinaryOp(Add):f64(IntBitsToFloat:f64(r24), IntBitsToFloat:f64(r32)))",
        ),
        (
            Opcode::FloatNeg,
            q40,
            &[q24],
            "FloatBitsToInt:i64(FloatUnaryOp(Neg):f64(IntBitsToFloat:f64(r24)))",
        ),
        (
            Opcode::FloatLess,
            b18,
            &[q24, q32],
            "Extend(ZeroExtend):i8(FloatCmpOp(Less):i1(IntBitsToFloat:f64(r24), IntBitsToFloat:f64(r32)))",
        ),
        (
            Opcode::IntSlessEqual,
            b18,
            &[r0, r4],
            "Extend(ZeroExtend):i8(IntBinaryOp(Xor):i1(IntCmpOp(Sless):i1(r4, r0), 0x1:i1))",
        ),
        (
            Opcode::FloatSub,
            r8,
            &[r0, r4],
            "FloatBitsToInt:i32(FloatBinaryOp(Add):f32(IntBitsToFloat:f32(r0), FloatUnaryOp(Neg):f32(IntBitsToFloat:f32(r4))))",
        ),
        (
            Opcode::FloatNotEqual,
            b18,
            &[r0, r4],
            "Extend(ZeroExtend):i8(IntBinaryOp(Xor):i1(FloatCmpOp(Equal):i1(IntBitsToFloat:f32(r0), IntBitsToFloat:f32(r4)), 0x1:i1))",
        ),
        (
            Opcode::FloatLessEqual,
            b18,
            &[r0, r4],
            "Extend(ZeroExtend):i8(IntBinaryOp(Or):i1(FloatCmpOp(Less):i1(IntBitsToFloat:f32(r0), IntBitsToFloat:f32(r4)), FloatCmpOp(Equal):i1(IntBitsToFloat:f32(r0), IntBitsToFloat:f32(r4))))",
        ),
    ];
    // `r8 = op(r0, r4)`.
    let binary: &[(Opcode, &str)] = &[
        (Opcode::IntAdd, "IntBinaryOp(Add):i32(r0, r4)"),
        (Opcode::IntMul, "IntBinaryOp(Mul):i32(r0, r4)"),
        (Opcode::IntAnd, "IntBinaryOp(And):i32(r0, r4)"),
        (Opcode::IntXor, "IntBinaryOp(Xor):i32(r0, r4)"),
        (Opcode::IntOr, "IntBinaryOp(Or):i32(r0, r4)"),
        (Opcode::IntDiv, "IntBinaryOp(Div):i32(r0, r4)"),
        (Opcode::IntSdiv, "IntBinaryOp(Sdiv):i32(r0, r4)"),
        (Opcode::IntRight, "IntBinaryOp(ShiftRight):i32(r0, r4)"),
        (Opcode::IntSright, "IntBinaryOp(SShiftRight):i32(r0, r4)"),
        (Opcode::IntLeft, "IntBinaryOp(ShiftLeft):i32(r0, r4)"),
        (Opcode::IntRem, "IntBinaryOp(Rem):i32(r0, r4)"),
        (Opcode::IntSrem, "IntBinaryOp(Srem):i32(r0, r4)"),
    ];
    let r0r4 = [r0, r4];
    let rows = cases.iter().copied().chain(
        binary
            .iter()
            .map(|&(opcode, expected)| (opcode, r8, &r0r4[..], expected)),
    );
    for (opcode, output, inputs, expected) in rows {
        with_test_lifter_tracking(vec![r0, r4, r8, b16, b17, b18, q24, q32, q40], |d, rid| {
            let insn = Insn {
                opcode,
                output: Some(output),
                inputs: inputs.to_vec().into(),
            };
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .unwrap_or_else(|e| panic!("{opcode:?} must lift: {e}"));
            let written = d.read_vn(&output).expect("read back the output");
            assert_eq!(
                operand_tree(d.builder.function(), written),
                expected,
                "{opcode:?} over {inputs:?}"
            );
        });
    }
}

/// Every binary op's operands match the output width, in either direction,
/// except a shift count.
#[test]
fn a_binary_op_operand_off_the_output_width_errs() {
    let (narrow, wide) = (reg_vn(16, 2), reg_vn(0x200, 8));
    let ops = [
        Opcode::IntAdd,
        Opcode::IntMul,
        Opcode::IntAnd,
        Opcode::IntOr,
        Opcode::IntXor,
        Opcode::IntDiv,
        Opcode::IntRem,
        Opcode::IntSdiv,
        Opcode::IntSrem,
        Opcode::IntLeft,
        Opcode::IntRight,
        Opcode::IntSright,
    ];
    for opcode in ops {
        let mut cases = vec![vec![narrow, reg(4)], vec![wide, reg(4)]];
        if !matches!(
            opcode,
            Opcode::IntLeft | Opcode::IntRight | Opcode::IntSright
        ) {
            cases.extend([vec![reg(4), narrow], vec![reg(4), wide]]);
        }
        for inputs in cases {
            with_test_lifter_tracking(vec![reg(0), reg(4), narrow, wide], |d, rid| {
                let insn = Insn {
                    opcode,
                    output: Some(reg(0)),
                    inputs: inputs.clone().into(),
                };
                let res = d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default());
                let msg = format!("{:#}", res.expect_err("an off-width operand must error"));
                assert!(
                    msg.contains("width mismatch"),
                    "{opcode:?} {inputs:?}: {msg}"
                );
            });
        }
    }
}

/// A shift count narrower than the output zero-extends, even under
/// `SShiftRight`.
#[test]
fn a_narrow_shift_count_zero_extends() {
    let (r0, r8) = (reg(0), reg(8));
    let count = reg_vn(16, 1);
    for (opcode, expected) in [
        (
            Opcode::IntLeft,
            "IntBinaryOp(ShiftLeft):i32(r0, Extend(ZeroExtend):i32(r16))",
        ),
        (
            Opcode::IntSright,
            "IntBinaryOp(SShiftRight):i32(r0, Extend(ZeroExtend):i32(r16))",
        ),
    ] {
        with_test_lifter_tracking(vec![r0, r8, count], |d, rid| {
            let insn = Insn {
                opcode,
                output: Some(r8),
                inputs: vec![r0, count].into(),
            };
            d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .unwrap_or_else(|e| panic!("{opcode:?} must lift: {e}"));
            let written = d.read_vn(&r8).expect("read back the output");
            assert_eq!(
                operand_tree(d.builder.function(), written),
                expected,
                "{opcode:?}"
            );
        });
    }
}

/// Narrow `IntNeg` lowers to `Xor(x, all_ones)` with the constant inline.
#[test]
fn lift_int_neg_narrow_lowers_to_xor_all_ones() {
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
        ("ymm_i256", 32, strider_ir::ValueType::int(256)),
        ("zmm_i512", 64, strider_ir::ValueType::int(512)),
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

/// `clz w0, w0` on a 64-bit arch lifts to an 8-byte output over a 4-byte input.
/// The count is over the INPUT's width, so widening the operand would add
/// `out_bits - in_bits` to every nonzero result.
#[test]
fn lzcount_keeps_its_input_width() {
    let wide = Vn {
        size: 8,
        addr_off: 0,
        addr_space: VnSpace::REGISTER,
    };
    with_test_lifter_tracking(vec![wide], |d, rid| {
        let insn = Insn {
            opcode: Opcode::Lzcount,
            output: Some(wide),
            inputs: vec![Vn {
                size: 4,
                addr_off: 0,
                addr_space: VnSpace::REGISTER,
            }]
            .into(),
        };
        d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
            .expect("lzcount lifts");

        let node = find_first_node(&d.builder, NodeKind::Lzcount).expect("an Lzcount node");
        let f = d.builder.function();
        let input = f.node_inputs(node)[0];
        assert_eq!(
            f.value_type(input).expect("typed input"),
            ValueType::int(32),
            "the operand must keep its own 4-byte width",
        );
        assert_eq!(
            f.value_type(f.node_outputs(node)[0]).expect("typed output"),
            ValueType::int(64),
            "the result is sized by the output varnode",
        );
    });
}

/// Same shape for `popcount`: the operand width decides the count's range.
#[test]
fn popcount_keeps_its_input_width() {
    let wide = Vn {
        size: 8,
        addr_off: 0,
        addr_space: VnSpace::REGISTER,
    };
    with_test_lifter_tracking(vec![wide], |d, rid| {
        let insn = Insn {
            opcode: Opcode::Popcount,
            output: Some(wide),
            inputs: vec![Vn {
                size: 4,
                addr_off: 0,
                addr_space: VnSpace::REGISTER,
            }]
            .into(),
        };
        d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
            .expect("popcount lifts");

        let node = find_first_node(&d.builder, NodeKind::Popcount).expect("a Popcount node");
        let f = d.builder.function();
        let input = f.node_inputs(node)[0];
        assert_eq!(
            f.value_type(input).expect("typed input"),
            ValueType::int(32),
            "the operand must keep its own 4-byte width",
        );
    });
}

/// A `Load` whose space id names no space of the engine errs; a real
/// decoded id is covered by the per-arch tests.
#[test]
fn lift_load_from_an_unknown_space_id_fails() {
    with_test_lifter(|d, rid| {
        let insn = Insn {
            opcode: Opcode::Load,
            output: Some(reg(0)),
            inputs: vec![const_vn(0x1234, 8), reg(8)].into(),
        };
        let err = d
            .process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
            .expect_err("an id outside the engine's table");
        assert!(
            format!("{err:#}").contains("names no space of this engine"),
            "{err:#}"
        );
    });
}

/// No SLEIGH production emits these; the decompiler mints them, `SegmentOp`
/// with a host `AddrSpace*` in input 0.
#[test]
fn lift_decompiler_internal_opcodes_fail_closed() {
    for opcode in [
        Opcode::Extract,
        Opcode::Insert,
        Opcode::PtrAdd,
        Opcode::PtrSub,
        Opcode::SegmentOp,
        Opcode::Indirect,
        Opcode::MultiEqual,
    ] {
        with_test_lifter(|d, rid| {
            let insn = Insn {
                opcode,
                output: Some(reg(0)),
                inputs: vec![const_vn(0, 4), const_vn(0, 2), const_vn(0, 4)].into(),
            };
            let err = d
                .process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
                .expect_err("decompiler-internal opcode must be rejected");
            assert!(
                format!("{err:#}").contains("decompiler-internal"),
                "error must name the contract, got: {err:#}"
            );
        });
    }
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
fn lift_with_set_lift_addr_records_asm_fingerprint() {
    with_test_lifter(|d, rid| {
        // `process_insn` owns the funnel, so the fingerprint comes from its
        // `addr` argument.
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
            .build_int_const(0x55u64, strider_ir::ValueType::int(32))
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

/// The canonical shape is `Xor(IntLess(b, a), IntConst(1)):I1`, operands
/// SWAPPED.  Two distinct register operands are needed to see the swap: a
/// const-fed lift cannot catch a dropped one.
#[test]
fn lift_int_less_equal_lowers_to_xor_of_a_swapped_less() {
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
        assert!(
            graph_has_kind(
                &d.builder,
                NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::Xor)
            ),
            "expected IntBinaryOp::Xor in graph (the I1 logical-NOT wrap)"
        );
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

/// `expect_ok` is false for handlers that read an operand and so error on an
/// absent one.
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
        (Opcode::Branch, true, "Branch lifts to nothing"),
        (
            Opcode::CondBranch,
            false,
            "CondBranch reads its condition operand and errors when absent",
        ),
        (
            Opcode::BranchIndirect,
            false,
            "BranchIndirect shares the Return handler and errors on an absent target",
        ),
        (
            Opcode::Return,
            false,
            "Return reads its target operand and errors when absent",
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
    ];
    for &(opcode, expect_ok, label) in cases {
        assert_process_insn(opcode, expect_ok, label);
    }
}

#[test]
fn read_vn_unknown_returns_initial_var_or_phi() {
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
            .build_int_const(42u64, strider_ir::ValueType::int(32))
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

/// A sub-register write masks the CONTAINER only.  The inserted value is
/// already narrowed to the register's width and shifted into its slot, so a
/// second mask over it clears nothing.
#[test]
fn sub_register_write_masks_the_container_only() {
    let container = Vn {
        size: 8,
        addr_off: 0,
        addr_space: VnSpace::REGISTER,
    };
    let half = Vn {
        size: 4,
        addr_off: 4,
        addr_space: VnSpace::REGISTER,
    };
    with_test_lifter_tracking(vec![container], |d, _rid| {
        let val = d
            .builder
            .build_int_const(0xdead_beefu64, strider_ir::ValueType::int(32))
            .unwrap();
        d.write_vn(&half, val).expect("sub-register write");
        let ands = d
            .builder
            .function()
            .graph()
            .all_node_ids()
            .filter(|id| {
                matches!(
                    d.builder.function().node_kind(*id),
                    NodeKind::IntBinaryOp(IntBinaryOp::And)
                )
            })
            .count();
        assert_eq!(ands, 1, "only the container mask is load-bearing");
    });
}

#[test]
fn write_vn_to_const_space_errors() {
    with_test_lifter(|d, _rid| {
        let val = d
            .builder
            .build_int_const(0u64, strider_ir::ValueType::int(32))
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
            "FloatNan(x) compares x against ITSELF, so both FloatEqual \
             operands must be the identical value"
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

/// Sleigh emits every integer compare equal-width, so a mixed-width one
/// errors rather than extending and comparing.
#[test]
fn compare_inputs_must_match_in_width() {
    let wide = Vn {
        size: 8,
        addr_off: 0x200,
        addr_space: VnSpace::REGISTER,
    };
    let flag = Vn {
        size: 1,
        addr_off: 0x300,
        addr_space: VnSpace::REGISTER,
    };
    for opcode in [
        Opcode::IntEqual,
        Opcode::IntLess,
        Opcode::IntSless,
        Opcode::IntCarry,
        Opcode::IntScarry,
        Opcode::IntSborrow,
        Opcode::IntNotEqual,
        Opcode::IntLessEqual,
        Opcode::IntSlessEqual,
    ] {
        with_test_lifter_tracking(vec![wide, reg(0), flag], |d, rid| {
            let insn = Insn {
                opcode,
                output: Some(flag),
                inputs: vec![reg(0), wide].into(),
            };
            let res = d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default());
            let msg = format!("{:#}", res.expect_err("mixed widths must error"));
            assert!(msg.contains("width mismatch"), "{opcode:?}: {msg}");
        });
    }
}

/// `IntSub`'s output is held to its inputs' width.
#[test]
fn int_sub_output_width_must_match_the_inputs() {
    let wide = Vn {
        size: 8,
        addr_off: 0x200,
        addr_space: VnSpace::REGISTER,
    };
    with_test_lifter_tracking(vec![wide, reg(0), reg(4)], |d, rid| {
        let insn = Insn {
            opcode: Opcode::IntSub,
            output: Some(wide),
            inputs: vec![reg(0), reg(4)].into(),
        };
        let res = d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default());
        let msg = format!("{:#}", res.expect_err("a wider output must error"));
        assert!(msg.contains("output width mismatch"), "{msg}");
    });
}

/// `OpBehaviorCopy::evaluateUnary` returns `in1` unchanged, so Sleigh contracts
/// equal input and output sizes.  Unguarded, `write_vn` truncates a wider
/// operand and zero-extends a narrower one, which is wrong for a signed
/// carrier.
#[test]
fn copy_input_width_must_match_the_output() {
    let wide = Vn {
        size: 8,
        addr_off: 0x200,
        addr_space: VnSpace::REGISTER,
    };
    for (label, input, output) in [
        ("narrower input", reg(0), wide),
        ("wider input", wide, reg(0)),
    ] {
        with_test_lifter_tracking(vec![wide, reg(0), reg(8)], |d, rid| {
            let insn = Insn {
                opcode: Opcode::Copy,
                output: Some(output),
                inputs: vec![input].into(),
            };
            let res = d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default());
            assert!(res.is_err(), "{label} must error");
            let msg = format!("{:#}", res.unwrap_err());
            assert!(
                msg.contains("width mismatch"),
                "{label}: error must name the width mismatch; got: {msg}"
            );
        });
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

/// An 11-byte width has no integer `ValueType`, so the lift must fail.  The
/// error has to name the offending machine instruction, otherwise a failed
/// whole-function lift reports only a bare "unsupported integer varnode size".
#[test]
fn load_odd_byte_width_errors_with_asm_context() {
    with_test_lifter(|d, rid| {
        // Copy from an 11-byte CONST so the size flows through `int_type`.
        let insn = Insn {
            opcode: Opcode::Copy,
            output: Some(Vn {
                size: 11,
                addr_off: 0,
                addr_space: VnSpace::REGISTER,
            }),
            inputs: vec![const_vn(0, 11)].into(),
        };
        let res = d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default());
        let err = res.expect_err("11-byte output must error (unsupported width)");
        // `{:#}` renders the outer asm context and the inner width cause.
        let msg = format!("{err:#}");
        assert!(
            msg.contains("unsupported integer varnode size: 11 bytes"),
            "error must still describe the unsupported width; got: {msg}"
        );
        assert!(
            msg.contains("0x1000") || msg.contains("4096") || msg.contains("Copy"),
            "lift error must attach the machine address / opcode for context; got: {msg}"
        );
    });
}

/// Full lift of `bytes` placed at 0x1000, entered at 0x1000.
pub(super) fn lift_bytes(
    arch: strider_target::SleighArch,
    cc: strider_target::CallingConvention,
    bytes: Vec<u8>,
) -> anyhow::Result<strider_ir::Function> {
    lift_bytes_with_opts(arch, cc, bytes, strider_cfg::CfgOptions::default())
}

/// [`lift_bytes`] under caller-chosen CFG options: the size bound is what
/// decides whether a trailing call falls through or ends the function.
pub(super) fn lift_bytes_with_opts(
    arch: strider_target::SleighArch,
    cc: strider_target::CallingConvention,
    bytes: Vec<u8>,
    opts: strider_cfg::CfgOptions,
) -> anyhow::Result<strider_ir::Function> {
    let mut lifter = Lifter::new(arch, rsleigh::mem_readers::BufMemReader::new(bytes, 0x1000))?;
    let cfg = lifter.build_cfg(0x1000u64.into(), &opts)?;
    let built = cc.build(lifter.sleigh_regs())?;
    Ok(lifter
        .build_ir_with(
            &cfg,
            built,
            &opts,
            &rustc_hash::FxHashMap::default(),
            &strider_ir::AssumptionOptions::none(),
            &[],
        )?
        .function)
}

/// A size-bounded function whose last instruction is a call ends `NoReturn`,
/// and the lifter sinks the open control edge into `Unreachable`, for
/// `Opcode::Call` and `Opcode::CallIndirect` alike.
#[test]
fn trailing_call_sinks_into_unreachable_direct_and_indirect() {
    for (label, bytes) in [
        ("call rel32", vec![0xe8u8, 0xfb, 0x0f, 0x00, 0x00]),
        ("call rax", vec![0xffu8, 0xd0]),
    ] {
        let opts = strider_cfg::CfgOptions {
            bound: strider_cfg::FunctionBound::size(bytes.len() as u64),
            ..strider_cfg::CfgOptions::default()
        };
        let f = lift_bytes_with_opts(
            strider_target::SleighArch::x86_64(),
            strider_target::CallingConvention::x86_64_systemv(),
            bytes,
            opts,
        )
        .unwrap_or_else(|e| panic!("{label}: {e}"));
        assert_eq!(
            f.graph()
                .all_node_ids()
                .filter(|n| matches!(f.node_kind(*n), NodeKind::Unreachable))
                .count(),
            1,
            "{label}: the no-return call gets exactly one sink"
        );
    }
}

/// A LOAD from the constant space has its address AS its value: GHIDRA's
/// `MemoryState::getValue` returns the offset for `IPTR_CONSTANT` without
/// consulting any memory bank.  PowerPC's 64-bit `rotmask_SH` exports its
/// rotate mask through a computed unique varnode, so an opaque read there
/// makes every `rldimi` mask operand unknown.
#[test]
fn const_space_load_yields_its_address() {
    // rldimi r3,r4,8,16 ; blr
    let bytes = vec![0x78, 0x83, 0x44, 0x0c, 0x4e, 0x80, 0x00, 0x20];
    let f = lift_bytes(
        strider_target::SleighArch::ppc64be(),
        strider_target::CallingConvention::powerpc64_elf_v1(),
        bytes,
    )
    .expect("rldimi must lift");
    assert!(
        !f.graph()
            .all_node_ids()
            .any(|id| matches!(f.node_kind(id), NodeKind::Load(_))),
        "rldimi reads no memory: its const-space mask LOAD must become its address"
    );
}

/// An intrinsic whose pcode output is a memory operand (x86 `sgdt [mem]` and
/// its `lidt` / `sldt` / `str` siblings) lifts: the ram output slot is dropped
/// and the write is carried by the memory clobber the ABI already declares.
/// Without this the whole function fails, which is how `save_processor_state`
/// went unliftable on every x86-64 kernel.
#[test]
fn call_other_writing_a_memory_operand_drops_the_ram_output() {
    with_test_lifter(|d, rid| {
        let names = d.lifter.user_op_names().to_vec();
        let Some(id) = names
            .iter()
            .position(|n| n == "GlobalDescriptorTableRegister")
        else {
            return; // sla without the op; nothing to pin here
        };
        let ram_out = Vn {
            addr_space: rsleigh::VnSpace::RAM,
            addr_off: 0x1_0000,
            size: 8,
        };
        let insn = Insn {
            opcode: Opcode::CallOther,
            output: Some(ram_out),
            inputs: vec![const_vn(id as u64, 4)].into(),
        };
        d.process_insn(rid, &insn, test_addr(), &super::RegionMap::default())
            .expect("a ram-output CallOther must lift, not fail the function");
        assert!(
            graph_has_kind(
                &d.builder,
                NodeKind::CallOther {
                    user_op_id: id as u64
                }
            ),
            "the CallOther node must still be built"
        );
    });
}

/// The count operand producers of every shift at `out_ty`, in graph order.
fn shift_count_producers(
    f: &strider_ir::Function,
    op: IntBinaryOp,
    out_ty: ValueType,
) -> Vec<NodeKind> {
    f.graph()
        .all_node_ids()
        .filter(|id| f.node_kind(*id) == &NodeKind::IntBinaryOp(op))
        .filter(|id| f.value_type(f.node_outputs(*id)[0]).ok() == Some(out_ty))
        .map(|id| {
            let count = f
                .node_inputs(id)
                .get(1)
                .copied()
                .expect("shift has a count");
            *f.node_kind(f.producer(count))
        })
        .collect()
}

/// x86 SIMD shift-by-register reads its count from the full `SRC[63:0]`, so a
/// count of `0x1_0000_0000` against a 4-byte lane must still read as
/// out-of-range. Truncating it to the lane width instead makes the shift a
/// no-op, which p-code defines as a zero / sign fill.
#[test]
fn a_shift_count_wider_than_the_output_saturates() {
    // psrad xmm0,xmm1 ; ret
    let f = lift_bytes(
        strider_target::SleighArch::x86_64(),
        strider_target::CallingConvention::x86_64_systemv(),
        vec![0x66, 0x0f, 0xe2, 0xc1, 0xc3],
    )
    .expect("psrad must lift");
    let producers = shift_count_producers(&f, IntBinaryOp::SShiftRight, ValueType::int(32));
    assert!(!producers.is_empty(), "psrad lifts to per-lane SShiftRight");
    for k in &producers {
        assert_eq!(
            *k,
            NodeKind::IntBinaryOp(IntBinaryOp::Or),
            "an over-wide count must reach the shift through the saturating Or"
        );
    }
    // The saturating addend is `Neg(too_big) & 32`, so the bound is materialised.
    assert!(
        f.graph()
            .all_node_ids()
            .flat_map(|id| f.node_outputs(id))
            .any(|&v| f.int_const_u128(v) == Some(32)),
        "the I32 out-of-range bound must be materialised"
    );
}

/// `shl eax,cl` reads a count NARROWER than its output, which p-code already
/// keeps in range: it takes the plain zero-extend, no saturation.
#[test]
fn a_shift_count_narrower_than_the_output_is_left_alone() {
    // shl eax,cl ; ret
    let f = lift_bytes(
        strider_target::SleighArch::x86_64(),
        strider_target::CallingConvention::x86_64_systemv(),
        vec![0xd3, 0xe0, 0xc3],
    )
    .expect("shl must lift");
    let producers = shift_count_producers(&f, IntBinaryOp::ShiftLeft, ValueType::int(32));
    assert!(!producers.is_empty(), "shl eax,cl lifts to a ShiftLeft");
    for k in &producers {
        assert_eq!(*k, NodeKind::Extend(strider_ir::ExtendOp::ZeroExtend));
    }
}

/// A `per_address_ccs` entry is keyed by a direct call's target ADDRESS, so a
/// `CALL` whose operand names a register offset selects none, as the CFG's
/// no-return rule already held. `call_override` serves the lifted call and the
/// def-site mirror alike.
#[test]
fn a_call_operand_outside_the_code_space_selects_no_override() {
    let ccs: rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention> =
        [(0x10, empty_cc())].into_iter().collect();
    with_test_lifter_arch_cc(
        strider_target::SleighArch::x86(),
        vec![0xc3],
        empty_cc(),
        vec![reg(0)],
        &ccs,
        |d, _| {
            let call_to = |addr_space| Insn {
                opcode: Opcode::Call,
                output: None,
                inputs: vec![Vn {
                    size: 4,
                    addr_off: 0x10,
                    addr_space,
                }]
                .into(),
            };
            let code_space = d.lifter.sleigh().default_code_space();
            assert!(
                d.call_override(&call_to(code_space)).is_some(),
                "premise: a code-space target selects its entry"
            );
            assert!(d.call_override(&call_to(VnSpace::REGISTER)).is_none());
        },
    );
}
