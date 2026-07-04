//! Register-aliasing unit tests exercising the REAL production aliasing
//! code on [`FunctionLifter`] (`read_reg_vn` / `write_reg_vn` and the
//! shift/mask machinery in [`crate::lift::vn_io`]).
//!
//! These were ported from `strider-ir`'s builder tests, which previously
//! ran against a byte-identical *copy* of the aliasing logic on
//! `FunctionBuilder`.  That copy is being deleted; the behavioural
//! coverage lives here now, against the prod `FunctionLifter` path.
//!
//! Each test drives the aliasing code through the shared
//! [`super::handler_tests::with_test_lifter_tracking_arch`] harness, which
//! seeds a `FunctionLifter` whose IR builder tracks the given container
//! varnodes and has a single entry region.  Little-endian tests use the
//! x86 harness; big-endian tests use the `ppc32be` harness (`blr`
//! terminator, no delay slot).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use rsleigh::{Vn, VnSpace};

use strider_ir::node::{ExtendOp, IntBinaryOp, IntCmpOp, NodeKind, ValueKind, ValueType};
use strider_ir::{IRBuilderExt, IRViewer, Value};

use super::handler_tests::with_test_lifter_tracking_arch;
use crate::lift::FunctionLifter;

type TestReader = rsleigh::mem_readers::BufMemReader<Vec<u8>>;

/// x86 little-endian throwaway-CFG scaffolding (`ret`).
fn x86() -> strider_target::SleighArch {
    strider_target::SleighArch::x86()
}
fn x86_term() -> Vec<u8> {
    vec![0xc3]
}

/// Big-endian PowerPC scaffolding (`blr` — a return, no delay slot).
fn ppc32be() -> strider_target::SleighArch {
    strider_target::SleighArch::ppc32be()
}
fn ppc32be_term() -> Vec<u8> {
    vec![0x4e, 0x80, 0x00, 0x20]
}

/// REGISTER-space varnode of byte width `size` at offset `off`.
fn reg_vn(off: u64, size: u32) -> Vn {
    Vn {
        size,
        addr_off: off,
        addr_space: VnSpace::REGISTER,
    }
}

/// UNIQUE-space varnode of byte width `size` at offset `off`.
fn unique_vn(off: u64, size: u32) -> Vn {
    Vn {
        size,
        addr_off: off,
        addr_space: VnSpace::UNIQUE,
    }
}

/// The node kind of `value`'s producer (inline replacement for the
/// strider-ir `producer_kind` helper).
fn pk(d: &FunctionLifter<'_, TestReader>, v: Value) -> NodeKind {
    let f = d.builder.function();
    *f.node_kind(f.producer(v))
}

// ── ported tests ────────────────────────────────────────────────────────────

/// Reading a sub-register when only the wider container is tracked routes
/// through the tracked-container scan, shifting/masking out of the container.
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

/// Reading a 1-byte sub-register (`AL`) out of a tracked 8-byte container
/// (`RAX`) under little-endian: shift 0 → a direct `Truncate` of the
/// container read with no `ShiftRight` in between.
#[test]
fn read_reg_vn_truncates_subregister_of_tracked_container() {
    let rax = reg_vn(0x100, 8);
    let al = reg_vn(0x100, 1); // low byte, same offset → shift 0
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

/// Writing a 1-byte sub-register (`al`) into a tracked 8-byte container
/// (`rax`) merges via the positioned-mask shape and preserves the high bytes.
#[test]
fn write_subregister_merge_preserves_container_high_bytes() {
    let rax = reg_vn(0x100, 8);
    let al = reg_vn(0x100, 1); // low byte → LE shift 0
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

        let mut consts: Vec<u128> = Vec::new();
        let mut saw_initial_container = false;
        for and_val in [lhs, rhs] {
            assert_eq!(
                pk(d, and_val),
                NodeKind::IntBinaryOp(IntBinaryOp::And),
                "each Or operand is an And"
            );
            for input in d
                .builder
                .function()
                .node_inputs(d.builder.function().producer(and_val))
            {
                if let Some(c) = d.builder.function().int_const_u128(input) {
                    consts.push(c);
                }
                if input == initial_rax {
                    saw_initial_container = true;
                }
            }
        }
        consts.sort_unstable();
        assert_eq!(
            consts,
            vec![0xAB, 0xFF, 0xFFFF_FFFF_FFFF_FF00],
            "masks must be positioned in container coordinates"
        );
        assert!(
            saw_initial_container,
            "the preserve arm must consume the pre-write container value"
        );
    });
}

/// Writing a 4-byte sub-register into a 10-byte x87 extended container
/// (`I80`) merges via the positioned-mask `Or` shape with the 80-bit
/// container mask `(1<<80)-1`.
#[test]
fn write_subregister_into_x87_80bit_container_preserves_high_bits() {
    let st = reg_vn(0x200, 10); // x87 80-bit extended register
    let lo4 = reg_vn(0x200, 4); // low 4 bytes → LE shift 0
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

        let mut consts: Vec<u128> = Vec::new();
        let mut saw_initial = false;
        for and_val in [lhs, rhs] {
            assert_eq!(
                pk(d, and_val),
                NodeKind::IntBinaryOp(IntBinaryOp::And),
                "each Or operand is an And"
            );
            for input in d
                .builder
                .function()
                .node_inputs(d.builder.function().producer(and_val))
            {
                if let Some(c) = d.builder.function().int_const_u128(input) {
                    consts.push(c);
                }
                if input == initial {
                    saw_initial = true;
                }
            }
        }
        consts.sort_unstable();
        let container_mask = (1u128 << 80) - 1;
        let keep_mask = container_mask & !0xFFFF_FFFFu128;
        assert_eq!(
            consts,
            vec![0xDEAD_BEEF, 0xFFFF_FFFF, keep_mask],
            "byte mask 0xFFFFFFFF + 80-bit preserve mask + the written value",
        );
        assert!(
            saw_initial,
            "the preserve arm must consume the pre-write 80-bit container value"
        );
    });
}

/// Writing the x86 high-byte sub-register `ah` (offset 1 → LE shift 8) into
/// `rax` positions the byte mask at bits 8..16 and left-shifts the value by 8.
#[test]
fn write_high_byte_subregister_positions_mask_and_shift() {
    let rax = reg_vn(0x100, 8);
    let ah = reg_vn(0x101, 1); // offset 1 byte → LE shift 8
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

        let mut preserve_arm = None;
        let mut insert_arm = None;
        for and_val in [lhs, rhs] {
            assert_eq!(
                pk(d, and_val),
                NodeKind::IntBinaryOp(IntBinaryOp::And),
                "each Or operand is an And"
            );
            let consumes_container = d
                .builder
                .function()
                .node_inputs(d.builder.function().producer(and_val))
                .into_iter()
                .any(|input| input == initial_rax);
            if consumes_container {
                preserve_arm = Some(and_val);
            } else {
                insert_arm = Some(and_val);
            }
        }
        let preserve_arm = preserve_arm.expect("one And arm preserves the container");
        let insert_arm = insert_arm.expect("one And arm inserts the shifted value");

        let preserve_consts: Vec<u128> = d
            .builder
            .function()
            .node_inputs(d.builder.function().producer(preserve_arm))
            .into_iter()
            .filter_map(|v| d.builder.function().int_const_u128(v))
            .collect();
        assert_eq!(
            preserve_consts,
            vec![0xFFFF_FFFF_FFFF_00FF],
            "keep-mask must clear only bits 8..16, preserving the low byte"
        );

        let [im_a, im_b] = d
            .builder
            .function()
            .node_inputs_exact::<2>(d.builder.function().producer(insert_arm))
            .unwrap();
        let (reg_mask_val, shifted_val) = if d.builder.function().int_const_u128(im_a).is_some() {
            (im_a, im_b)
        } else {
            (im_b, im_a)
        };
        assert_eq!(
            d.builder.function().int_const_u128(reg_mask_val),
            Some(0xFF00),
            "byte mask must be positioned at bits 8..16"
        );
        assert_eq!(
            pk(d, shifted_val),
            NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft),
            "the written value must be shifted into position"
        );

        let [shl_value, shl_amount] = d
            .builder
            .function()
            .node_inputs_exact::<2>(d.builder.function().producer(shifted_val))
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

/// Big-endian sub-register WRITE: the offset-0 byte inside a 4-byte container
/// is the HIGH byte under BE → shift 24.
#[test]
fn write_high_byte_subregister_big_endian_positions_mask_and_shift() {
    let container = reg_vn(0x100, 4);
    let sub = reg_vn(0x100, 1); // BE: offset-0 byte is the HIGH byte → shift 24
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

        let mut preserve_arm = None;
        let mut insert_arm = None;
        for and_val in [lhs, rhs] {
            assert_eq!(
                pk(d, and_val),
                NodeKind::IntBinaryOp(IntBinaryOp::And),
                "each Or operand is an And"
            );
            let consumes_container = d
                .builder
                .function()
                .node_inputs(d.builder.function().producer(and_val))
                .into_iter()
                .any(|input| input == initial);
            if consumes_container {
                preserve_arm = Some(and_val);
            } else {
                insert_arm = Some(and_val);
            }
        }
        let preserve_arm = preserve_arm.expect("one And arm preserves the container");
        let insert_arm = insert_arm.expect("one And arm inserts the shifted value");

        let preserve_consts: Vec<u128> = d
            .builder
            .function()
            .node_inputs(d.builder.function().producer(preserve_arm))
            .into_iter()
            .filter_map(|v| d.builder.function().int_const_u128(v))
            .collect();
        assert_eq!(
            preserve_consts,
            vec![0x00FF_FFFF],
            "BE keep-mask must clear only the high byte (bits 24..32)"
        );

        let [im_a, im_b] = d
            .builder
            .function()
            .node_inputs_exact::<2>(d.builder.function().producer(insert_arm))
            .unwrap();
        let (reg_mask_val, shifted_val) = if d.builder.function().int_const_u128(im_a).is_some() {
            (im_a, im_b)
        } else {
            (im_b, im_a)
        };
        assert_eq!(
            d.builder.function().int_const_u128(reg_mask_val),
            Some(0xFF00_0000),
            "BE byte mask must be positioned at bits 24..32"
        );
        assert_eq!(
            pk(d, shifted_val),
            NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft),
            "the written value must be shifted into the high-byte position"
        );
        let [_shl_value, shl_amount] = d
            .builder
            .function()
            .node_inputs_exact::<2>(d.builder.function().producer(shifted_val))
            .unwrap();
        assert_eq!(
            d.builder.function().int_const_u128(shl_amount),
            Some(24),
            "BE left-shift amount must be 24 (high byte of a 4-byte container)"
        );
    });
}

/// Big-endian sub-register READ companion: `Truncate(ShiftRight(container, 24))`.
#[test]
fn read_high_byte_subregister_big_endian_shifts_then_truncates() {
    let container = reg_vn(0x100, 4);
    let sub = reg_vn(0x100, 1); // BE high byte → shift 24
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

/// Writing an `I1` value directly into a tracked full-width register goes
/// through the direct-container branch and zero-extends the I1 to I64.
#[test]
fn write_i1_into_register_zero_extends_to_container_width() {
    let reg = reg_vn(0x200, 8); // tracked 8-byte register → I64
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

/// Reading a UNIQUE-space sub-slice of a tracked UNIQUE container routes
/// through the same aliasing path: `Truncate(ShiftRight(container, 32))`.
#[test]
fn read_unique_subslice_of_tracked_unique_container() {
    let container = unique_vn(0x400, 8);
    let sub = unique_vn(0x404, 4); // upper 4 bytes → LE shift 32
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

/// Sub-register access inside a >16-byte (ymm-like) container: the READ slices
/// the wide container via `Truncate(ShiftRight(container, off))` (no mask
/// needed — this is what unblocks SSE `palignr`-style low-slice Copies), while
/// the WRITE still fails closed (a >16-byte mask has no u128 representation).
#[test]
fn wide_container_subregister_read_slices_but_write_fails_closed() {
    let ymm = reg_vn(0x1000, 32);
    let mid8 = reg_vn(0x1008, 8); // strict sub-slice at offset 8 → LE shift 64
    with_test_lifter_tracking_arch(x86(), x86_term(), vec![ymm], |d, _| {
        // READ now succeeds: the wide container is sliced via shift + truncate.
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

        // WRITE still fails closed: `build_masked_insert` would need a 256-bit
        // mask, which `u128` cannot represent.
        let val = d.builder.build_int_const(1u64, ValueType::I64).unwrap();
        let write_err = d
            .write_reg_vn(&mid8, val)
            .expect_err("write of a ymm sub-slice must error");
        assert!(
            write_err.to_string().contains("wide (32-byte) container"),
            "write error must name the wide-container limitation; got: {write_err}"
        );
    });
}

/// A sub-register write of a 1-bit `I1` value must succeed exactly like the
/// direct-container arm: zero-extended to the sub-register width and merged.
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

/// A sub-register write of a NON-integer (float) value must fail with the SAME
/// "bitcast required first" diagnostic the direct-container arm raises.
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
