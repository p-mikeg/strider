use rsleigh::{Vn, VnSpace};

use strider_ir::node::{ExtendOp, IntBinaryOp, IntCmpOp, NodeKind, ValueKind, ValueType};
use strider_ir::{IRBuilderExt, IRViewer, Value};

use super::handler_tests::{lift_bytes, with_test_lifter_tracking_arch};
use crate::lift::FunctionLifter;

type TestReader = rsleigh::mem_readers::BufMemReader<Vec<u8>>;

fn x86() -> strider_target::SleighArch {
    strider_target::SleighArch::x86()
}
fn x86_term() -> Vec<u8> {
    vec![0xc3]
}

/// `blr` is a return with no delay slot.
fn ppc32be() -> strider_target::SleighArch {
    strider_target::SleighArch::ppc32be()
}
fn ppc32be_term() -> Vec<u8> {
    vec![0x4e, 0x80, 0x00, 0x20]
}

fn reg_vn(off: u64, size: u32) -> Vn {
    Vn {
        size,
        addr_off: off,
        addr_space: VnSpace::REGISTER,
    }
}

fn unique_vn(off: u64, size: u32) -> Vn {
    Vn {
        size,
        addr_off: off,
        addr_space: VnSpace::UNIQUE,
    }
}

fn pk(d: &FunctionLifter<'_, TestReader>, v: Value) -> NodeKind {
    let f = d.builder.function();
    *f.node_kind(f.producer(v))
}

/// Splits a merge `Or`'s operands into `(preserve, insert)`: the preserve arm
/// is the `And` consuming the pre-write container value `initial`.
fn classify_preserve_insert_arms(
    d: &FunctionLifter<'_, TestReader>,
    lhs: Value,
    rhs: Value,
    initial: Value,
) -> (Value, Value) {
    for (arm, other) in [(lhs, rhs), (rhs, lhs)] {
        let consumes_container = pk(d, arm) == NodeKind::IntBinaryOp(IntBinaryOp::And)
            && d.builder
                .function()
                .node_inputs(d.builder.function().producer(arm))
                .into_iter()
                .any(|input| input == initial);
        if consumes_container {
            return (arm, other);
        }
    }
    panic!("one Or arm must mask the pre-write container value")
}

/// The constant inputs of `value`'s producer, sorted.
fn arm_consts(d: &FunctionLifter<'_, TestReader>, value: Value) -> Vec<u128> {
    let f = d.builder.function();
    let mut consts: Vec<u128> = f
        .node_inputs(f.producer(value))
        .into_iter()
        .filter_map(|input| f.int_const_u128(input))
        .collect();
    consts.sort_unstable();
    consts
}

/// A sub-register read when only the container is tracked must route through
/// the container scan rather than minting a fresh variable.
#[test]
fn read_subregister_routes_through_container_map() {
    let rax = reg_vn(0x0, 8);
    let eax = reg_vn(0x0, 4);
    with_test_lifter_tracking_arch(x86(), x86_term(), vec![rax], |d, _| {
        let v = d.read_reg_vn(&eax).unwrap();
        assert_eq!(
            d.builder.function().value_type(v).unwrap(),
            ValueType::I32,
            "eax read yields I32 sliced from the rax container",
        );
    });
}

/// `AL` out of `RAX` under little-endian is shift 0, so a direct `Truncate`
/// with no `ShiftRight`.
#[test]
fn read_reg_vn_truncates_subregister_of_tracked_container() {
    let rax = reg_vn(0x100, 8);
    let al = reg_vn(0x100, 1); // low byte, same offset -> shift 0
    with_test_lifter_tracking_arch(x86(), x86_term(), vec![rax], |d, _| {
        let read = d.read_reg_vn(&al).unwrap();
        assert_eq!(
            d.builder.function().value_kind(read),
            ValueKind::Typed(ValueType::I8),
            "AL read must be I8-typed"
        );
        let read_node = d.builder.function().producer(read);
        assert!(
            matches!(
                d.builder.function().node_kind(read_node),
                NodeKind::Truncate
            ),
            "AL (offset 0, shift 0) read must be a direct Truncate of the container read, got {:?}",
            d.builder.function().node_kind(read_node)
        );
    });
}

/// The merge must preserve `rax`'s high bytes.
#[test]
fn write_subregister_merge_preserves_container_high_bytes() {
    let rax = reg_vn(0x100, 8);
    let al = reg_vn(0x100, 1); // low byte -> LE shift 0
    with_test_lifter_tracking_arch(x86(), x86_term(), vec![rax], |d, _| {
        let initial_rax = d.builder.read_variable(&rax).unwrap();
        let byte_val = d.builder.build_int_const(0xABu64, ValueType::I8).unwrap();
        d.write_reg_vn(&al, byte_val).unwrap();

        let merged = d.read_reg_vn(&rax).unwrap();
        assert_eq!(
            d.builder.function().value_type(merged).unwrap(),
            ValueType::I64
        );
        assert_eq!(pk(d, merged), NodeKind::IntBinaryOp(IntBinaryOp::Or));
        let [lhs, rhs] = d
            .builder
            .function()
            .node_inputs_exact::<2>(d.builder.function().producer(merged))
            .unwrap();

        let (preserve_arm, insert_arm) = classify_preserve_insert_arms(d, lhs, rhs, initial_rax);
        assert_eq!(
            arm_consts(d, preserve_arm),
            vec![0xFFFF_FFFF_FFFF_FF00],
            "the keep-mask must be positioned in container coordinates"
        );
        assert_eq!(
            d.builder.function().int_const_u128(insert_arm),
            Some(0xAB),
            "at shift 0 the zero-extended byte IS the insert arm"
        );
    });
}

/// x87 `I80` container: the mask must be the 80-bit `(1<<80)-1`.
#[test]
fn write_subregister_into_x87_80bit_container_preserves_high_bits() {
    let st = reg_vn(0x200, 10); // x87 80-bit extended register
    let lo4 = reg_vn(0x200, 4); // low 4 bytes -> LE shift 0
    with_test_lifter_tracking_arch(x86(), x86_term(), vec![st], |d, _| {
        let initial = d.builder.read_variable(&st).unwrap();
        let val = d
            .builder
            .build_int_const(0xDEAD_BEEFu64, ValueType::I32)
            .unwrap();
        d.write_reg_vn(&lo4, val).unwrap();

        let merged = d.read_reg_vn(&st).unwrap();
        assert_eq!(
            d.builder.function().value_type(merged).unwrap(),
            ValueType::I80,
            "the 10-byte container reads back as I80",
        );
        assert_eq!(pk(d, merged), NodeKind::IntBinaryOp(IntBinaryOp::Or));
        let [lhs, rhs] = d
            .builder
            .function()
            .node_inputs_exact::<2>(d.builder.function().producer(merged))
            .unwrap();

        let (preserve_arm, insert_arm) = classify_preserve_insert_arms(d, lhs, rhs, initial);
        let container_mask = (1u128 << 80) - 1;
        let keep_mask = container_mask & !0xFFFF_FFFFu128;
        assert_eq!(
            arm_consts(d, preserve_arm),
            vec![keep_mask],
            "the preserve mask spans the whole 80-bit container",
        );
        assert_eq!(
            d.builder.function().int_const_u128(insert_arm),
            Some(0xDEAD_BEEF),
            "at shift 0 the zero-extended word IS the insert arm"
        );
    });
}

/// `ah` sits at offset 1, so LE shift 8 and a mask at bits 8..16.
#[test]
fn write_high_byte_subregister_positions_mask_and_shift() {
    let rax = reg_vn(0x100, 8);
    let ah = reg_vn(0x101, 1); // offset 1 byte -> LE shift 8
    with_test_lifter_tracking_arch(x86(), x86_term(), vec![rax], |d, _| {
        let initial_rax = d.builder.read_variable(&rax).unwrap();
        let byte_val = d.builder.build_int_const(0xABu64, ValueType::I8).unwrap();
        d.write_reg_vn(&ah, byte_val).unwrap();

        let merged = d.read_reg_vn(&rax).unwrap();
        assert_eq!(
            d.builder.function().value_type(merged).unwrap(),
            ValueType::I64
        );
        assert_eq!(pk(d, merged), NodeKind::IntBinaryOp(IntBinaryOp::Or));
        let [lhs, rhs] = d
            .builder
            .function()
            .node_inputs_exact::<2>(d.builder.function().producer(merged))
            .unwrap();

        let (preserve_arm, insert_arm) = classify_preserve_insert_arms(d, lhs, rhs, initial_rax);

        assert_eq!(
            arm_consts(d, preserve_arm),
            vec![0xFFFF_FFFF_FFFF_00FF],
            "keep-mask must clear only bits 8..16, preserving the low byte"
        );

        assert_eq!(
            pk(d, insert_arm),
            NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft),
            "the written value must be shifted into position"
        );
        let [shl_value, shl_amount] = d
            .builder
            .function()
            .node_inputs_exact::<2>(d.builder.function().producer(insert_arm))
            .unwrap();
        assert_eq!(
            d.builder.function().int_const_u128(shl_amount),
            Some(8),
            "left-shift amount must be 8 (one byte)"
        );
        assert_eq!(
            d.builder.function().int_const_u128(shl_value),
            Some(0xAB),
            "the zero-extended written byte feeds the shift"
        );
    });
}

/// Under big-endian the offset-0 byte of a 4-byte container is the HIGH byte,
/// so the shift is 24, not 0.
#[test]
fn write_high_byte_subregister_big_endian_positions_mask_and_shift() {
    let container = reg_vn(0x100, 4);
    let sub = reg_vn(0x100, 1); // BE: offset-0 byte is the HIGH byte -> shift 24
    with_test_lifter_tracking_arch(ppc32be(), ppc32be_term(), vec![container], |d, _| {
        let initial = d.builder.read_variable(&container).unwrap();
        let byte_val = d.builder.build_int_const(0xABu64, ValueType::I8).unwrap();
        d.write_reg_vn(&sub, byte_val).unwrap();

        let merged = d.read_reg_vn(&container).unwrap();
        assert_eq!(
            d.builder.function().value_type(merged).unwrap(),
            ValueType::I32
        );
        assert_eq!(pk(d, merged), NodeKind::IntBinaryOp(IntBinaryOp::Or));
        let [lhs, rhs] = d
            .builder
            .function()
            .node_inputs_exact::<2>(d.builder.function().producer(merged))
            .unwrap();

        let (preserve_arm, insert_arm) = classify_preserve_insert_arms(d, lhs, rhs, initial);

        assert_eq!(
            arm_consts(d, preserve_arm),
            vec![0x00FF_FFFF],
            "BE keep-mask must clear only the high byte (bits 24..32)"
        );

        assert_eq!(
            pk(d, insert_arm),
            NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft),
            "the written value must be shifted into the high-byte position"
        );
        let [_shl_value, shl_amount] = d
            .builder
            .function()
            .node_inputs_exact::<2>(d.builder.function().producer(insert_arm))
            .unwrap();
        assert_eq!(
            d.builder.function().int_const_u128(shl_amount),
            Some(24),
            "BE left-shift amount must be 24 (high byte of a 4-byte container)"
        );
    });
}

/// Read counterpart: `Truncate(ShiftRight(container, 24))`.
#[test]
fn read_high_byte_subregister_big_endian_shifts_then_truncates() {
    let container = reg_vn(0x100, 4);
    let sub = reg_vn(0x100, 1); // BE high byte -> shift 24
    with_test_lifter_tracking_arch(ppc32be(), ppc32be_term(), vec![container], |d, _| {
        let read = d.read_reg_vn(&sub).unwrap();
        assert_eq!(
            d.builder.function().value_type(read).unwrap(),
            ValueType::I8
        );
        assert_eq!(pk(d, read), NodeKind::Truncate);
        let [shifted] = d
            .builder
            .function()
            .node_inputs_exact::<1>(d.builder.function().producer(read))
            .unwrap();
        assert_eq!(
            pk(d, shifted),
            NodeKind::IntBinaryOp(IntBinaryOp::ShiftRight),
            "BE high-byte read must shift right before truncating"
        );
        let [_shr_value, shr_amount] = d
            .builder
            .function()
            .node_inputs_exact::<2>(d.builder.function().producer(shifted))
            .unwrap();
        assert_eq!(
            d.builder.function().int_const_u128(shr_amount),
            Some(24),
            "BE right-shift amount must be 24 (high byte of a 4-byte container)"
        );
    });
}

/// The direct-container branch must zero-extend an `I1` to the reg width.
#[test]
fn write_i1_into_register_zero_extends_to_container_width() {
    let reg = reg_vn(0x200, 8); // tracked 8-byte register -> I64
    with_test_lifter_tracking_arch(x86(), x86_term(), vec![reg], |d, _| {
        let lhs = d.builder.build_int_const(1u64, ValueType::I32).unwrap();
        let rhs = d.builder.build_int_const(2u64, ValueType::I32).unwrap();
        let i1_cmp = d
            .builder
            .build_int_cmp_operation(lhs, rhs, IntCmpOp::Equal, ValueType::I32)
            .unwrap();
        assert_eq!(
            d.builder.function().value_type(i1_cmp).unwrap(),
            ValueType::I1,
            "the comparison output is I1"
        );

        d.write_reg_vn(&reg, i1_cmp).unwrap();

        let stored = d.read_reg_vn(&reg).unwrap();
        assert_eq!(
            d.builder.function().value_type(stored).unwrap(),
            ValueType::I64,
            "an I1 written into an 8-byte register must be stored at I64 width"
        );
        assert_eq!(
            pk(d, stored),
            NodeKind::Extend(ExtendOp::ZeroExtend),
            "the stored value's producer must be a ZeroExtend of the I1"
        );
        let [extended_input] = d
            .builder
            .function()
            .node_inputs_exact::<1>(d.builder.function().producer(stored))
            .unwrap();
        assert_eq!(
            extended_input, i1_cmp,
            "the ZeroExtend must consume the I1 comparison result directly"
        );
    });
}

/// UNIQUE space takes the same aliasing path as REGISTER.
#[test]
fn read_unique_subslice_of_tracked_unique_container() {
    let container = unique_vn(0x400, 8);
    let sub = unique_vn(0x404, 4); // upper 4 bytes -> LE shift 32
    with_test_lifter_tracking_arch(x86(), x86_term(), vec![container], |d, _| {
        let read = d.read_reg_vn(&sub).unwrap();
        assert_eq!(
            d.builder.function().value_type(read).unwrap(),
            ValueType::I32
        );
        assert_eq!(pk(d, read), NodeKind::Truncate);
        let [shifted] = d
            .builder
            .function()
            .node_inputs_exact::<1>(d.builder.function().producer(read))
            .unwrap();
        assert_eq!(
            pk(d, shifted),
            NodeKind::IntBinaryOp(IntBinaryOp::ShiftRight),
            "mid-container UNIQUE slice must shift before truncating"
        );
    });
}

/// In a >16-byte container the READ slices via shift+truncate with no mask,
/// and the WRITE masks at the container's own `I256` / `I512` width.
#[test]
fn wide_container_subregister_read_slices_and_write_masks_at_container_width() {
    let ymm = reg_vn(0x1000, 32);
    let mid8 = reg_vn(0x1008, 8); // strict sub-slice at offset 8 -> LE shift 64
    with_test_lifter_tracking_arch(x86(), x86_term(), vec![ymm], |d, _| {
        let read = d
            .read_reg_vn(&mid8)
            .expect("read of a ymm sub-slice must slice the wide container");
        assert_eq!(
            d.builder.function().value_type(read).unwrap(),
            ValueType::I64
        );
        assert_eq!(pk(d, read), NodeKind::Truncate);
        let [shifted] = d
            .builder
            .function()
            .node_inputs_exact::<1>(d.builder.function().producer(read))
            .unwrap();
        assert_eq!(
            pk(d, shifted),
            NodeKind::IntBinaryOp(IntBinaryOp::ShiftRight),
            "mid-container wide slice must shift before truncating"
        );

        let initial_ymm = d.builder.read_variable(&ymm).unwrap();
        let val = d.builder.build_int_const(1u64, ValueType::I64).unwrap();
        d.write_reg_vn(&mid8, val)
            .expect("write of a ymm sub-slice masks at I256");

        let merged = d.read_reg_vn(&ymm).unwrap();
        assert_eq!(
            d.builder.function().value_type(merged).unwrap(),
            ValueType::I256
        );
        assert_eq!(pk(d, merged), NodeKind::IntBinaryOp(IntBinaryOp::Or));
        let [lhs, rhs] = d
            .builder
            .function()
            .node_inputs_exact::<2>(d.builder.function().producer(merged))
            .unwrap();
        let (preserve, insert) = classify_preserve_insert_arms(d, lhs, rhs, initial_ymm);
        // Bytes 8..16 are the slice, everything else is preserved.
        let mut want_preserve = vec![0xFFu8; 32];
        want_preserve[8..16].fill(0);
        assert_eq!(
            wide_mask_bytes(d, preserve),
            Some(want_preserve),
            "preserve arm masks exactly the container bytes outside the sub-register"
        );
        assert_eq!(
            pk(d, insert),
            NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft),
            "insert arm shifts the slice to byte 8 and needs no mask of its own"
        );
    });
}

/// The 256-bit mask has no `u128` form, so the wide masks are read back as
/// little-endian bytes.
fn wide_mask_bytes(d: &FunctionLifter<'_, TestReader>, and_val: Value) -> Option<Vec<u8>> {
    let f = d.builder.function();
    f.node_inputs(f.producer(and_val))
        .into_iter()
        .find_map(|input| f.int_const_wide_le_bytes(f.producer(input)))
}

/// A function mixing a 256-bit and a 128-bit VEX form pulls `ZMM0` into the
/// tracked set, so the 32-byte `YMM0` write becomes a sub-register write into
/// a 64-byte container.
#[test]
fn mixed_width_avx_function_lifts() {
    // vmovaps %ymm1,%ymm0 ; vaddss %xmm2,%xmm2,%xmm0 ; ret
    let bytes = vec![0xc5, 0xfc, 0x28, 0xc1, 0xc5, 0xea, 0x58, 0xc2, 0xc3];
    let f = lift_bytes(
        strider_target::SleighArch::x86_64(),
        strider_target::CallingConvention::x86_64_systemv(),
        bytes,
    )
    .expect("mixed-width AVX function must lift");
    assert!(f.graph().all_node_ids().count() > 0);
}

/// AArch64 has the same containment shape as x86-64 AVX: `d0` sits inside
/// `q0` inside the 32-byte SVE `z0`.
#[test]
fn aarch64_wide_container_subregister_write_masks_at_container_width() {
    let z = reg_vn(0x1000, 32);
    let d = reg_vn(0x1000, 8); // register-endianness is little on aarch64 -> shift 0
    with_test_lifter_tracking_arch(
        strider_target::SleighArch::aarch64(),
        vec![0xc0, 0x03, 0x5f, 0xd6], // ret
        vec![z],
        |d_lifter, _| {
            let initial_z = d_lifter.builder.read_variable(&z).unwrap();
            let val = d_lifter
                .builder
                .build_int_const(0xABu64, ValueType::I64)
                .unwrap();
            d_lifter
                .write_reg_vn(&d, val)
                .expect("write of an SVE sub-slice masks at I256");

            let merged = d_lifter.read_reg_vn(&z).unwrap();
            assert_eq!(
                d_lifter.builder.function().value_type(merged).unwrap(),
                ValueType::I256
            );
            assert_eq!(pk(d_lifter, merged), NodeKind::IntBinaryOp(IntBinaryOp::Or));
            let [lhs, rhs] = d_lifter
                .builder
                .function()
                .node_inputs_exact::<2>(d_lifter.builder.function().producer(merged))
                .unwrap();
            let (preserve, _insert) = classify_preserve_insert_arms(d_lifter, lhs, rhs, initial_z);
            let mut want_preserve = vec![0xFFu8; 32];
            want_preserve[0..8].fill(0);
            assert_eq!(
                wide_mask_bytes(d_lifter, preserve),
                Some(want_preserve),
                "preserve arm keeps the container bytes outside the sub-register"
            );
        },
    );
}

/// An `I1` sub-register write must behave like the direct-container arm.
#[test]
fn write_reg_vn_subregister_accepts_i1_like_direct_arm() {
    let container = reg_vn(0x0, 8);
    let sub = reg_vn(0x0, 1);
    with_test_lifter_tracking_arch(x86(), x86_term(), vec![container], |d, _| {
        let flag = d.builder.build_boolean_const(true);
        d.write_reg_vn(&sub, flag).unwrap();

        let read_back = d.read_reg_vn(&container).unwrap();
        assert_eq!(
            d.builder.function().value_type(read_back).unwrap(),
            ValueType::I64,
            "container reads back at its natural width after the I1 sub-register write"
        );
    });
}

/// A float sub-register write must raise the SAME "bitcast required first"
/// diagnostic as the direct-container arm.
#[test]
fn write_reg_vn_subregister_float_errors_like_direct_arm() {
    let container = reg_vn(0x0, 8);
    let sub = reg_vn(0x0, 1);
    with_test_lifter_tracking_arch(x86(), x86_term(), vec![container], |d, _| {
        let f = d
            .builder
            .build_float_const(0x4000_0000_0000_0000u64, ValueType::F64);
        let sub_err = d.write_reg_vn(&sub, f).unwrap_err().to_string();
        assert!(
            sub_err.contains("bitcast is required first"),
            "sub-register float write must raise the direct-arm's bitcast error, got: {sub_err}"
        );

        let f2 = d
            .builder
            .build_float_const(0x4000_0000_0000_0000u64, ValueType::F64);
        let direct_err = d.write_reg_vn(&container, f2).unwrap_err().to_string();
        assert!(
            direct_err.contains("bitcast is required first"),
            "direct-container float write raises the bitcast error, got: {direct_err}"
        );
    });
}
