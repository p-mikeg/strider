//! White-box tests for the `node` submodules.

use super::*;

// ── NodeOutputType ───────────────────────────────────────────────────────

/// `get_unsigned_int` must mask the value to the declared width.
/// Bits above the type's width must be cleared even if they are set.
#[test]
fn unsigned_int_masks_to_declared_width() {
    let wide: u128 = u128::MAX;
    assert_eq!(
        NodeOutputType::U8.get_unsigned_int(wide),
        Some(u128::from(u8::MAX))
    );
    assert_eq!(
        NodeOutputType::U16.get_unsigned_int(wide),
        Some(u128::from(u16::MAX))
    );
    assert_eq!(
        NodeOutputType::U32.get_unsigned_int(wide),
        Some(u128::from(u32::MAX))
    );
    assert_eq!(
        NodeOutputType::U64.get_unsigned_int(wide),
        Some(u128::from(u64::MAX))
    );
}

/// `get_unsigned_int` must return `None` for `Bool` because a boolean is
/// not an integer representation.
#[test]
fn unsigned_int_is_none_for_bool() {
    assert_eq!(NodeOutputType::Bool.get_unsigned_int(1), None);
}

/// `get_signed_int` must sign-extend values.  The MSB of the declared
/// width acts as the sign bit, so a value with the MSB set must come out
/// negative.
#[test]
fn signed_int_sign_extends_from_declared_width() {
    assert_eq!(
        NodeOutputType::U8.get_signed_int(u128::from(u8::MAX)),
        Some(-1)
    );
    assert_eq!(
        NodeOutputType::U8.get_signed_int(u128::from(i8::MIN as u8)),
        Some(i128::from(i8::MIN))
    );
    assert_eq!(
        NodeOutputType::U8.get_signed_int(i8::MAX as u128),
        Some(i128::from(i8::MAX))
    );
    assert_eq!(
        NodeOutputType::U16.get_signed_int(u128::from(i16::MIN as u16)),
        Some(i128::from(i16::MIN))
    );
    assert_eq!(
        NodeOutputType::U32.get_signed_int(u128::from(u32::MAX)),
        Some(-1)
    );
}

/// `get_signed_int` must return `None` for `Bool`.
#[test]
fn signed_int_is_none_for_bool() {
    assert_eq!(NodeOutputType::Bool.get_signed_int(1), None);
}

/// `bit_width` must equal `byte_size * 8` for every variant.
#[test]
fn bit_width_is_eight_times_byte_size() {
    for ty in [
        NodeOutputType::Bool,
        NodeOutputType::U8,
        NodeOutputType::U16,
        NodeOutputType::U32,
        NodeOutputType::U64,
        NodeOutputType::U80,
        NodeOutputType::U128,
        NodeOutputType::U256,
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
    assert!(NodeOutputKind::OutputType(NodeOutputType::U64).is_value());
    assert!(!NodeOutputKind::Control.is_value());
    assert!(!NodeOutputKind::PhiToken.is_value());
    assert!(!NodeOutputKind::Memory.is_value());
}

/// `is_bool` must be `true` only when the wrapped type is `Bool`.
#[test]
fn is_bool_only_for_bool_output_type() {
    assert!(NodeOutputKind::OutputType(NodeOutputType::Bool).is_bool());
    assert!(!NodeOutputKind::OutputType(NodeOutputType::U8).is_bool());
    assert!(!NodeOutputKind::Control.is_bool());
}

/// `is_integer` must be `true` for all integer `OutputType` variants and
/// `false` for `Bool`, `Control`, `PhiToken`, `Memory`, and floats.
#[test]
fn is_integer_for_all_integer_output_types() {
    for ty in [
        NodeOutputType::U8,
        NodeOutputType::U16,
        NodeOutputType::U32,
        NodeOutputType::U64,
        NodeOutputType::U80,
        NodeOutputType::U128,
        NodeOutputType::U256,
    ] {
        assert!(
            NodeOutputKind::OutputType(ty).is_integer(),
            "{ty:?} should be integer"
        );
    }
    for ty in [
        NodeOutputType::Bool,
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

/// Only `BoolConst` and `IntConst` should be considered constants; all
/// other variants must not.
#[test]
fn is_const_only_for_constant_kinds() {
    assert!(NodeKind::BoolConst(true).is_const());
    assert!(NodeKind::IntConst(42).is_const());
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
    assert!(NodeKind::BoolConst(false).is_cacheable());
    assert!(NodeKind::IntBinaryOp(crate::ops::IntBinaryOp::Add).is_cacheable());
    assert!(NodeKind::IntUnaryOp(crate::ops::IntUnaryOp::BitNot).is_cacheable());
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
    assert!(!NodeOutputType::U32.is_float());
    assert!(!NodeOutputType::U64.is_float());
    assert!(!NodeOutputType::Bool.is_float());
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
    let kind = NodeOutputKind::OutputType(NodeOutputType::U32);
    assert_eq!(kind.as_value_or_err().unwrap(), NodeOutputType::U32);
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
    let kind = NodeOutputKind::OutputType(NodeOutputType::U64);
    assert_eq!(kind.as_integer_or_err().unwrap(), NodeOutputType::U64);
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
        (NodeOutputType::Bool, "bool", 1, false, true, false),
        (NodeOutputType::U8,   "u8",   1, true,  false, false),
        (NodeOutputType::U16,  "u16",  2, true,  false, false),
        (NodeOutputType::U32,  "u32",  4, true,  false, false),
        (NodeOutputType::U64,  "u64",  8, true,  false, false),
        (NodeOutputType::U80,  "u80",  10, true, false, false),
        (NodeOutputType::U128, "u128", 16, true, false, false),
        (NodeOutputType::U256, "u256", 32, true, false, false),
        (NodeOutputType::F32,  "f32",  4, false, false, true),
        (NodeOutputType::F64,  "f64",  8, false, false, true),
        (NodeOutputType::F80,  "f80",  10, false, false, true),
    ];
    for (ty, name, size, is_int, is_bool, is_float) in cases {
        assert_eq!(ty.as_str(), *name);
        assert_eq!(ty.byte_size(), *size);
        assert_eq!(ty.bit_width(), *size * 8);
        assert_eq!(ty.is_integer(), *is_int);
        assert_eq!(ty.is_bool(), *is_bool);
        assert_eq!(ty.is_float(), *is_float);
    }
}

#[test]
fn try_from_u32_size_to_node_output_type() {
    assert_eq!(NodeOutputType::try_from(1u32).unwrap(), NodeOutputType::U8);
    assert_eq!(NodeOutputType::try_from(2u32).unwrap(), NodeOutputType::U16);
    assert_eq!(NodeOutputType::try_from(4u32).unwrap(), NodeOutputType::U32);
    assert_eq!(NodeOutputType::try_from(8u32).unwrap(), NodeOutputType::U64);
    assert_eq!(NodeOutputType::try_from(16u32).unwrap(), NodeOutputType::U128);
    assert_eq!(NodeOutputType::try_from(32u32).unwrap(), NodeOutputType::U256);
    assert_eq!(NodeOutputType::try_from(64u32).unwrap(), NodeOutputType::U512);
    for bad in [0u32, 3, 5, 7, 9, 15, 17, 33, 65] {
        let err = NodeOutputType::try_from(bad).expect_err("invalid size");
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
        BoolBinaryOp, BoolUnaryOp, ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp,
        IntBinaryOp, IntCmpOp, IntUnaryOp,
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
        NodeKind::IntUnaryOp(IntUnaryOp::BitNot),
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        NodeKind::IntCmpOp(IntCmpOp::Equal),
        NodeKind::Truncate,
        NodeKind::Extend(ExtendOp::ZeroExtend),
        NodeKind::Popcount,
        NodeKind::Lzcount,
        NodeKind::CastToInt,
        // pure value: bool
        NodeKind::BoolConst(false),
        NodeKind::BoolUnaryOp(BoolUnaryOp::Neg),
        NodeKind::BoolBinaryOp(BoolBinaryOp::And),
        NodeKind::CastToBool,
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
        NodeKind::CastToFloat,
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

