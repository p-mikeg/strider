//! White-box tests for the `node` submodules.

use super::*;

// ── NodeOutputType ───────────────────────────────────────────────────────

/// `get_unsigned_int` must mask the value to the declared width.
/// Bits above the type's width must be cleared even if they are set.
#[test]
fn unsigned_int_masks_to_declared_width() {
    let wide: u128 = u128::MAX;
    assert_eq!(
        NodeOutputType::I8.get_unsigned_int(wide),
        Some(u128::from(u8::MAX))
    );
    assert_eq!(
        NodeOutputType::I16.get_unsigned_int(wide),
        Some(u128::from(u16::MAX))
    );
    assert_eq!(
        NodeOutputType::I32.get_unsigned_int(wide),
        Some(u128::from(u32::MAX))
    );
    assert_eq!(
        NodeOutputType::I64.get_unsigned_int(wide),
        Some(u128::from(u64::MAX))
    );
}

/// `I1` is a 1-bit integer, so `get_unsigned_int` masks to the low bit.
#[test]
fn unsigned_int_masks_i1_to_low_bit() {
    assert_eq!(NodeOutputType::I1.get_unsigned_int(1), Some(1));
    assert_eq!(NodeOutputType::I1.get_unsigned_int(0), Some(0));
    assert_eq!(NodeOutputType::I1.get_unsigned_int(0xFE), Some(0));
}

/// `get_signed_int` must sign-extend values.  The MSB of the declared
/// width acts as the sign bit, so a value with the MSB set must come out
/// negative.
#[test]
fn signed_int_sign_extends_from_declared_width() {
    assert_eq!(
        NodeOutputType::I8.get_signed_int(u128::from(u8::MAX)),
        Some(-1)
    );
    assert_eq!(
        NodeOutputType::I8.get_signed_int(u128::from(i8::MIN as u8)),
        Some(i128::from(i8::MIN))
    );
    assert_eq!(
        NodeOutputType::I8.get_signed_int(i8::MAX as u128),
        Some(i128::from(i8::MAX))
    );
    assert_eq!(
        NodeOutputType::I16.get_signed_int(u128::from(i16::MIN as u16)),
        Some(i128::from(i16::MIN))
    );
    assert_eq!(
        NodeOutputType::I32.get_signed_int(u128::from(u32::MAX)),
        Some(-1)
    );
}

/// `I1` is a 1-bit signed integer: bit 0 set reads as `-1`, clear as `0`.
#[test]
fn signed_int_for_i1_is_one_bit() {
    assert_eq!(NodeOutputType::I1.get_signed_int(1), Some(-1));
    assert_eq!(NodeOutputType::I1.get_signed_int(0), Some(0));
}

/// `bit_width` equals `byte_size * 8` for every variant except `I1`, which
/// is 1 bit despite occupying 1 byte.
#[test]
fn bit_width_is_eight_times_byte_size_except_i1() {
    assert_eq!(NodeOutputType::I1.bit_width(), 1);
    assert_eq!(NodeOutputType::I1.byte_size(), 1);
    for ty in [
        NodeOutputType::I8,
        NodeOutputType::I16,
        NodeOutputType::I32,
        NodeOutputType::I64,
        NodeOutputType::I80,
        NodeOutputType::I128,
        NodeOutputType::I256,
        NodeOutputType::F32,
        NodeOutputType::F64,
        NodeOutputType::F80,
    ] {
        assert_eq!(
            ty.bit_width(),
            ty.byte_size() * 8,
            "bit_width mismatch for {ty:?}"
        );
    }
}

// ── NodeOutputKind ───────────────────────────────────────────────────────

/// `is_value` must be `true` only for `OutputType` variants.
#[test]
fn is_value_only_for_output_type() {
    assert!(NodeOutputKind::OutputType(NodeOutputType::I64).is_value());
    assert!(!NodeOutputKind::Control.is_value());
    assert!(!NodeOutputKind::PhiToken.is_value());
    assert!(!NodeOutputKind::Memory.is_value());
}

/// `is_bool` must be `true` only when the wrapped type is `Bool`.
#[test]
fn is_bool_only_for_bool_output_type() {
    assert!(NodeOutputKind::OutputType(NodeOutputType::I1).is_bool());
    assert!(!NodeOutputKind::OutputType(NodeOutputType::I8).is_bool());
    assert!(!NodeOutputKind::Control.is_bool());
}

/// `is_integer` must be `true` for all integer `OutputType` variants
/// (including the 1-bit `I1`) and `false` for `Control`, `PhiToken`,
/// `Memory`, and floats.
#[test]
fn is_integer_for_all_integer_output_types() {
    for ty in [
        NodeOutputType::I1,
        NodeOutputType::I8,
        NodeOutputType::I16,
        NodeOutputType::I32,
        NodeOutputType::I64,
        NodeOutputType::I80,
        NodeOutputType::I128,
        NodeOutputType::I256,
    ] {
        assert!(
            NodeOutputKind::OutputType(ty).is_integer(),
            "{ty:?} should be integer"
        );
    }
    for ty in [
        NodeOutputType::F32,
        NodeOutputType::F64,
        NodeOutputType::F80,
    ] {
        assert!(
            !NodeOutputKind::OutputType(ty).is_integer(),
            "{ty:?} must not be integer"
        );
    }
    assert!(!NodeOutputKind::Control.is_integer());
    assert!(!NodeOutputKind::Memory.is_integer());
}

// ── NodeKind ─────────────────────────────────────────────────────────────

/// Only constant kinds (`IntConst`, `IntConstWide`, `FloatConst`) should be
/// considered constants; all other variants must not.  Booleans are
/// `IntConst` values typed `I1`.
#[test]
fn is_const_only_for_constant_kinds() {
    assert!(NodeKind::IntConst(42).is_const());
    assert!(NodeKind::FloatConst(0).is_const());
    assert!(!NodeKind::Entry.is_const());
    assert!(!NodeKind::Return.is_const());
}

/// Non-cacheable node kinds must cover all nodes that receive inputs
/// dynamically after creation.
#[test]
fn non_cacheable_kinds_are_not_cacheable() {
    let space = rsleigh::VnSpace::RAM;
    // Entry / InitialMemory / InitialVar are cacheable (identity fully
    // determined by NodeKind fields; dedup prevents accidental
    // duplicates).  Region / MemPhi / Phi / Return / Call remain
    // non-cacheable: their identity depends on construction context.
    let non_cacheable = [
        NodeKind::Return,
        NodeKind::Region,
        NodeKind::MemPhi,
        NodeKind::Phi,
        NodeKind::Call,
    ];
    let _ = space; // silence unused variable warning
    for kind in non_cacheable {
        assert!(!kind.is_cacheable(), "{kind:?} should not be cacheable");
    }
}

/// Arithmetic and logical operations are always cacheable — equal nodes
/// with equal inputs produce the same result and can be deduplicated.
#[test]
fn arithmetic_kinds_are_cacheable() {
    assert!(NodeKind::IntConst(0).is_cacheable());
    assert!(NodeKind::IntBinaryOp(crate::ops::IntBinaryOp::Add).is_cacheable());
    assert!(NodeKind::IntUnaryOp(crate::ops::IntUnaryOp::Neg).is_cacheable());
    assert!(NodeKind::If.is_cacheable());
}

// ── Float NodeOutputType ─────────────────────────────────────────────────

#[test]
fn float_byte_sizes() {
    assert_eq!(NodeOutputType::F32.byte_size(), 4);
    assert_eq!(NodeOutputType::F64.byte_size(), 8);
}

#[test]
fn float_bit_widths() {
    assert_eq!(NodeOutputType::F32.bit_width(), 32);
    assert_eq!(NodeOutputType::F64.bit_width(), 64);
}

#[test]
fn float_as_str() {
    assert_eq!(NodeOutputType::F32.as_str(), "f32");
    assert_eq!(NodeOutputType::F64.as_str(), "f64");
}

#[test]
fn is_float_only_for_float_types() {
    assert!(NodeOutputType::F32.is_float());
    assert!(NodeOutputType::F64.is_float());
    assert!(!NodeOutputType::I32.is_float());
    assert!(!NodeOutputType::I64.is_float());
    assert!(!NodeOutputType::I1.is_float());
}

#[test]
fn is_integer_false_for_float_types() {
    assert!(!NodeOutputType::F32.is_integer());
    assert!(!NodeOutputType::F64.is_integer());
}

#[test]
fn get_unsigned_int_returns_none_for_floats() {
    assert_eq!(NodeOutputType::F32.get_unsigned_int(0x3F800000), None);
    assert_eq!(
        NodeOutputType::F64.get_unsigned_int(0x3FF0000000000000),
        None
    );
}

#[test]
fn get_signed_int_returns_none_for_floats() {
    assert_eq!(NodeOutputType::F32.get_signed_int(0x3F800000), None);
    assert_eq!(NodeOutputType::F64.get_signed_int(0x3FF0000000000000), None);
}

// ── Float NodeKind ───────────────────────────────────────────────────────

#[test]
fn float_const_is_const_and_cacheable() {
    let fc = NodeKind::FloatConst(0x3F800000);
    assert!(fc.is_const());
    assert!(fc.is_cacheable());
}

#[test]
fn float_ops_are_cacheable() {
    assert!(NodeKind::FloatBinaryOp(crate::ops::FloatBinaryOp::Add).is_cacheable());
    assert!(NodeKind::FloatUnaryOp(crate::ops::FloatUnaryOp::Neg).is_cacheable());
    assert!(NodeKind::FloatCmpOp(crate::ops::FloatCmpOp::Equal).is_cacheable());
    assert!(NodeKind::IntToFloat.is_cacheable());
    assert!(NodeKind::FloatToInt.is_cacheable());
    assert!(NodeKind::FloatToFloat.is_cacheable());
    assert!(NodeKind::IntBitsToFloat.is_cacheable());
    assert!(NodeKind::FloatBitsToInt.is_cacheable());
}

// ── as_value_or_err / as_integer_or_err ────────────────────────────────

#[test]
fn as_value_or_err_value_case() {
    let kind = NodeOutputKind::OutputType(NodeOutputType::I32);
    assert_eq!(kind.as_value_or_err().unwrap(), NodeOutputType::I32);
}

#[test]
fn as_value_or_err_control_case() {
    let kind = NodeOutputKind::Control;
    let err = kind.as_value_or_err().unwrap_err();
    assert!(
        err.to_string().contains("expected value output"),
        "got: {err}"
    );
}

#[test]
fn as_integer_or_err_int_case() {
    let kind = NodeOutputKind::OutputType(NodeOutputType::I64);
    assert_eq!(kind.as_integer_or_err().unwrap(), NodeOutputType::I64);
}

#[test]
fn as_integer_or_err_float_case() {
    let kind = NodeOutputKind::OutputType(NodeOutputType::F32);
    let err = kind.as_integer_or_err().unwrap_err();
    assert!(
        err.to_string().contains("is not an integer type"),
        "got: {err}"
    );
}

#[test]
fn type_info_table_matches_variants() {
    // Table indices must match discriminant order. Enumerate every variant
    // explicitly and check `info().name` / category.
    let cases: &[(NodeOutputType, &str, usize, bool, bool, bool)] = &[
        (NodeOutputType::I1,   "i1",   1, true,  true,  false),
        (NodeOutputType::I8,   "i8",   1, true,  false, false),
        (NodeOutputType::I16,  "i16",  2, true,  false, false),
        (NodeOutputType::I32,  "i32",  4, true,  false, false),
        (NodeOutputType::I64,  "i64",  8, true,  false, false),
        (NodeOutputType::I80,  "i80",  10, true, false, false),
        (NodeOutputType::I128, "i128", 16, true, false, false),
        (NodeOutputType::I256, "i256", 32, true, false, false),
        (NodeOutputType::F32,  "f32",  4, false, false, true),
        (NodeOutputType::F64,  "f64",  8, false, false, true),
        (NodeOutputType::F80,  "f80",  10, false, false, true),
    ];
    for (ty, name, size, is_int, is_bool, is_float) in cases {
        assert_eq!(ty.as_str(), *name);
        assert_eq!(ty.byte_size(), *size);
        // I1 is the lone exception: 1 byte but 1 bit wide.
        let expected_bits = if *ty == NodeOutputType::I1 { 1 } else { *size * 8 };
        assert_eq!(ty.bit_width(), expected_bits);
        assert_eq!(ty.is_integer(), *is_int);
        assert_eq!(ty.is_bool(), *is_bool);
        assert_eq!(ty.is_float(), *is_float);
    }
}

#[test]
fn int_for_byte_size_to_node_output_type() {
    assert_eq!(NodeOutputType::int_for_byte_size(1).unwrap(), NodeOutputType::I8);
    assert_eq!(NodeOutputType::int_for_byte_size(2).unwrap(), NodeOutputType::I16);
    assert_eq!(NodeOutputType::int_for_byte_size(4).unwrap(), NodeOutputType::I32);
    assert_eq!(NodeOutputType::int_for_byte_size(8).unwrap(), NodeOutputType::I64);
    assert_eq!(NodeOutputType::int_for_byte_size(16).unwrap(), NodeOutputType::I128);
    assert_eq!(NodeOutputType::int_for_byte_size(32).unwrap(), NodeOutputType::I256);
    assert_eq!(NodeOutputType::int_for_byte_size(64).unwrap(), NodeOutputType::I512);
    for bad in [0u32, 3, 5, 7, 9, 15, 17, 33, 65] {
        let err = NodeOutputType::int_for_byte_size(bad).expect_err("invalid size");
        let msg = err.to_string();
        assert!(
            msg.contains(&format!("unsupported node output size: {bad} bytes")),
            "wrong error for {bad}: {err:?}"
        );
    }
}

// ── NodeKind predicates ───────────────────────────────────────────────────

/// Returns one constructor for every [`NodeKind`] variant.  Hand-maintained;
/// adding a new variant requires appending it here so the equivalence tests
/// below continue to cover every kind.  The exhaustive matches in
/// `is_cacheable` and `asm_fingerprint_exempt` catch new variants at compile
/// time, but a forgotten append here would silently shrink runtime coverage.
fn every_node_kind_smoke() -> Vec<NodeKind> {
    use crate::ops::{
        ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp,
    };
    use cranelift_entity::EntityRef;
    let space = rsleigh::VnSpace::RAM;
    let vn = rsleigh::Vn {
        addr_off: 0,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    vec![
        // initial state
        NodeKind::Entry,
        NodeKind::InitialMemory,
        NodeKind::InitialVar(vn),
        // region
        NodeKind::Region,
        // phis
        NodeKind::MemPhi,
        NodeKind::Phi,
        // terminator
        NodeKind::If,
        NodeKind::Call,
        NodeKind::Return,
        NodeKind::IndirectBranch,
        NodeKind::CallOther { user_op_id: 0 },
        // memory operations
        NodeKind::Load(space),
        NodeKind::Store(space),
        // pure value: integer
        NodeKind::IntConst(0),
        NodeKind::IntConstWide(crate::wide_const::WideConstId::new(0)),
        NodeKind::IntUnaryOp(IntUnaryOp::Neg),
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        NodeKind::IntCmpOp(IntCmpOp::Equal),
        NodeKind::Truncate,
        NodeKind::Extend(ExtendOp::ZeroExtend),
        NodeKind::Popcount,
        NodeKind::Lzcount,
        // pure value: float
        NodeKind::FloatConst(0),
        NodeKind::FloatBinaryOp(FloatBinaryOp::Add),
        NodeKind::FloatUnaryOp(FloatUnaryOp::Neg),
        NodeKind::FloatCmpOp(FloatCmpOp::Equal),
        // pure value: conversions
        NodeKind::IntToFloat,
        NodeKind::IntBitsToFloat,
        NodeKind::FloatToInt,
        NodeKind::FloatBitsToInt,
        NodeKind::FloatToFloat,
        // pure value: sleigh pure user-op
        NodeKind::SegmentOp { op_id: 0 },
        // if is pure-value above; opaque user-ops left:
        // opaque call
        NodeKind::CPoolRef,
        NodeKind::New,
    ]
}

/// Original (pre-refactor) hand-written `is_cacheable` predicate.  Pinned
/// here so the new direct implementation can be checked for
/// byte-identical behaviour on every NodeKind variant.
fn legacy_is_cacheable(kind: &NodeKind) -> bool {
    !matches!(
        kind,
        NodeKind::Return
            | NodeKind::IndirectBranch
            | NodeKind::Region
            | NodeKind::MemPhi
            | NodeKind::Phi
            | NodeKind::Call
            | NodeKind::CallOther { .. }
            | NodeKind::CPoolRef
            | NodeKind::New
    )
}

/// Original (pre-refactor) hand-written `asm_fingerprint_exempt` predicate.
fn legacy_asm_fingerprint_exempt(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Entry
            | NodeKind::InitialMemory
            | NodeKind::InitialVar(_)
            | NodeKind::Region
            | NodeKind::MemPhi
            | NodeKind::Phi
    )
}

/// `NodeKind::is_cacheable` must agree with the legacy hand-written
/// predicate on every NodeKind variant.
#[test]
fn is_cacheable_matches_legacy() {
    for k in every_node_kind_smoke() {
        assert_eq!(
            k.is_cacheable(),
            legacy_is_cacheable(&k),
            "is_cacheable disagrees with legacy for {k:?}"
        );
    }
}

/// `NodeKind::asm_fingerprint_exempt` must agree with the legacy
/// hand-written predicate on every NodeKind variant.
#[test]
fn asm_fingerprint_exempt_matches_legacy() {
    for k in every_node_kind_smoke() {
        assert_eq!(
            k.asm_fingerprint_exempt(),
            legacy_asm_fingerprint_exempt(&k),
            "asm_fingerprint_exempt disagrees with legacy for {k:?}"
        );
    }
}

