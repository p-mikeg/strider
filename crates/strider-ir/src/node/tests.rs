use super::*;
use cranelift_entity::EntityRef;

/// Bits above the declared width must be cleared even when set.
#[test]
fn unsigned_int_masks_to_declared_width() {
    let wide: u128 = u128::MAX;
    assert_eq!(
        ValueType::I8.get_unsigned_int(wide),
        Some(u128::from(u8::MAX))
    );
    assert_eq!(
        ValueType::I16.get_unsigned_int(wide),
        Some(u128::from(u16::MAX))
    );
    assert_eq!(
        ValueType::I32.get_unsigned_int(wide),
        Some(u128::from(u32::MAX))
    );
    assert_eq!(
        ValueType::I64.get_unsigned_int(wide),
        Some(u128::from(u64::MAX))
    );
}

#[test]
fn unsigned_int_masks_i1_to_low_bit() {
    assert_eq!(ValueType::I1.get_unsigned_int(1), Some(1));
    assert_eq!(ValueType::I1.get_unsigned_int(0), Some(0));
    assert_eq!(ValueType::I1.get_unsigned_int(0xFE), Some(0));
}

/// The MSB of the declared width is the sign bit.
#[test]
fn signed_int_sign_extends_from_declared_width() {
    assert_eq!(ValueType::I8.get_signed_int(u128::from(u8::MAX)), Some(-1));
    assert_eq!(
        ValueType::I8.get_signed_int(u128::from(i8::MIN as u8)),
        Some(i128::from(i8::MIN))
    );
    assert_eq!(
        ValueType::I8.get_signed_int(i8::MAX as u128),
        Some(i128::from(i8::MAX))
    );
    assert_eq!(
        ValueType::I16.get_signed_int(u128::from(i16::MIN as u16)),
        Some(i128::from(i16::MIN))
    );
    assert_eq!(
        ValueType::I32.get_signed_int(u128::from(u32::MAX)),
        Some(-1)
    );
}

/// Read signed, a 1-bit integer holds only 0 and -1.
#[test]
fn signed_int_for_i1_is_one_bit() {
    assert_eq!(ValueType::I1.get_signed_int(1), Some(-1));
    assert_eq!(ValueType::I1.get_signed_int(0), Some(0));
}

#[test]
fn bit_width_is_eight_times_byte_size_except_i1() {
    assert_eq!(ValueType::I1.bit_width(), 1);
    assert_eq!(ValueType::I1.byte_size(), 1);
    for ty in [
        ValueType::I8,
        ValueType::I16,
        ValueType::I24,
        ValueType::I32,
        ValueType::I40,
        ValueType::I48,
        ValueType::I56,
        ValueType::I64,
        ValueType::I72,
        ValueType::I80,
        ValueType::I96,
        ValueType::I112,
        ValueType::I128,
        ValueType::I256,
        ValueType::I512,
        ValueType::F16,
        ValueType::F32,
        ValueType::F64,
        ValueType::F80,
        ValueType::F128,
    ] {
        assert_eq!(
            ty.bit_width(),
            ty.byte_size() * 8,
            "bit_width mismatch for {ty:?}"
        );
    }
}

#[test]
fn is_value_only_for_output_type() {
    assert!(ValueKind::Typed(ValueType::I64).is_value());
    assert!(!ValueKind::Control.is_value());
    assert!(!ValueKind::PhiToken.is_value());
    assert!(!ValueKind::Memory.is_value());
}

#[test]
fn is_bool_only_for_bool_output_type() {
    assert!(ValueKind::Typed(ValueType::I1).is_bool());
    assert!(!ValueKind::Typed(ValueType::I8).is_bool());
    assert!(!ValueKind::Control.is_bool());
}

/// `I1` counts as an integer; floats and the non-value kinds do not.
#[test]
fn is_integer_for_all_integer_output_types() {
    for ty in [
        ValueType::I1,
        ValueType::I8,
        ValueType::I16,
        ValueType::I24,
        ValueType::I32,
        ValueType::I40,
        ValueType::I48,
        ValueType::I56,
        ValueType::I64,
        ValueType::I72,
        ValueType::I80,
        ValueType::I96,
        ValueType::I112,
        ValueType::I128,
        ValueType::I256,
        ValueType::I512,
    ] {
        assert!(
            ValueKind::Typed(ty).is_integer(),
            "{ty:?} should be integer"
        );
    }
    for ty in [
        ValueType::F16,
        ValueType::F32,
        ValueType::F64,
        ValueType::F80,
        ValueType::F128,
    ] {
        assert!(
            !ValueKind::Typed(ty).is_integer(),
            "{ty:?} must not be integer"
        );
    }
    assert!(!ValueKind::Control.is_integer());
    assert!(!ValueKind::Memory.is_integer());
}

#[test]
fn is_const_only_for_constant_kinds() {
    assert!(NodeKind::IntConst(crate::node::const_value::ConstId::new(42_usize)).is_const());
    assert!(NodeKind::FloatConst(0).is_const());
    assert!(!NodeKind::Entry.is_const());
    assert!(!NodeKind::Return.is_const());
}

/// Every kind that receives inputs after creation must be non-cacheable.
#[test]
fn non_cacheable_kinds_are_not_cacheable() {
    let space = rsleigh::VnSpace::RAM;
    let non_cacheable = [
        NodeKind::Return,
        NodeKind::Region,
        NodeKind::MemPhi,
        NodeKind::Phi,
        NodeKind::Call,
    ];
    let _ = space;
    for kind in non_cacheable {
        assert!(!kind.is_cacheable(), "{kind:?} should not be cacheable");
    }
}

/// Equal operands give equal results, so these always dedup.
#[test]
fn arithmetic_kinds_are_cacheable() {
    assert!(NodeKind::IntConst(crate::node::const_value::ConstId::new(0_usize)).is_cacheable());
    assert!(NodeKind::IntBinaryOp(crate::node::IntBinaryOp::Add).is_cacheable());
    assert!(NodeKind::IntUnaryOp(crate::node::IntUnaryOp::Neg).is_cacheable());
    assert!(NodeKind::If.is_cacheable());
}

#[test]
fn float_byte_sizes() {
    assert_eq!(ValueType::F32.byte_size(), 4);
    assert_eq!(ValueType::F64.byte_size(), 8);
}

#[test]
fn float_bit_widths() {
    assert_eq!(ValueType::F32.bit_width(), 32);
    assert_eq!(ValueType::F64.bit_width(), 64);
}

#[test]
fn float_as_str() {
    assert_eq!(ValueType::F32.as_str(), "f32");
    assert_eq!(ValueType::F64.as_str(), "f64");
}

#[test]
fn is_float_only_for_float_types() {
    assert!(ValueType::F32.is_float());
    assert!(ValueType::F64.is_float());
    assert!(!ValueType::I32.is_float());
    assert!(!ValueType::I64.is_float());
    assert!(!ValueType::I1.is_float());
}

#[test]
fn is_integer_false_for_float_types() {
    assert!(!ValueType::F32.is_integer());
    assert!(!ValueType::F64.is_integer());
}

#[test]
fn get_unsigned_int_returns_none_for_floats() {
    assert_eq!(ValueType::F32.get_unsigned_int(0x3F800000), None);
    assert_eq!(ValueType::F64.get_unsigned_int(0x3FF0000000000000), None);
}

#[test]
fn get_signed_int_returns_none_for_floats() {
    assert_eq!(ValueType::F32.get_signed_int(0x3F800000), None);
    assert_eq!(ValueType::F64.get_signed_int(0x3FF0000000000000), None);
}

#[test]
fn float_const_is_const_and_cacheable() {
    let fc = NodeKind::FloatConst(0x3F800000);
    assert!(fc.is_const());
    assert!(fc.is_cacheable());
}

#[test]
fn float_ops_are_cacheable() {
    assert!(NodeKind::FloatBinaryOp(crate::node::FloatBinaryOp::Add).is_cacheable());
    assert!(NodeKind::FloatUnaryOp(crate::node::FloatUnaryOp::Neg).is_cacheable());
    assert!(NodeKind::FloatCmpOp(crate::node::FloatCmpOp::Equal).is_cacheable());
    assert!(NodeKind::IntToFloat.is_cacheable());
    assert!(NodeKind::FloatToInt.is_cacheable());
    assert!(NodeKind::FloatToFloat.is_cacheable());
    assert!(NodeKind::IntBitsToFloat.is_cacheable());
    assert!(NodeKind::FloatBitsToInt.is_cacheable());
}

#[test]
fn as_value_or_err_value_case() {
    let kind = ValueKind::Typed(ValueType::I32);
    assert_eq!(kind.as_value_or_err().unwrap(), ValueType::I32);
}

#[test]
fn as_value_or_err_control_case() {
    let kind = ValueKind::Control;
    let err = kind.as_value_or_err().unwrap_err();
    assert!(
        err.to_string().contains("expected value output"),
        "got: {err}"
    );
}

#[test]
fn type_info_table_matches_variants() {
    let cases: &[(ValueType, &str, usize, bool, bool, bool)] = &[
        (ValueType::I1, "i1", 1, true, true, false),
        (ValueType::I8, "i8", 1, true, false, false),
        (ValueType::I16, "i16", 2, true, false, false),
        (ValueType::I24, "i24", 3, true, false, false),
        (ValueType::I32, "i32", 4, true, false, false),
        (ValueType::I40, "i40", 5, true, false, false),
        (ValueType::I48, "i48", 6, true, false, false),
        (ValueType::I56, "i56", 7, true, false, false),
        (ValueType::I64, "i64", 8, true, false, false),
        (ValueType::I72, "i72", 9, true, false, false),
        (ValueType::I80, "i80", 10, true, false, false),
        (ValueType::I96, "i96", 12, true, false, false),
        (ValueType::I112, "i112", 14, true, false, false),
        (ValueType::I128, "i128", 16, true, false, false),
        (ValueType::I256, "i256", 32, true, false, false),
        (ValueType::I512, "i512", 64, true, false, false),
        (ValueType::F16, "f16", 2, false, false, true),
        (ValueType::F32, "f32", 4, false, false, true),
        (ValueType::F64, "f64", 8, false, false, true),
        (ValueType::F80, "f80", 10, false, false, true),
        (ValueType::F128, "f128", 16, false, false, true),
    ];
    for (ty, name, size, is_int, is_bool, is_float) in cases {
        assert_eq!(ty.as_str(), *name);
        assert_eq!(ty.byte_size(), *size);
        // I1 is the lone exception: 1 byte but 1 bit wide.
        let expected_bits = if *ty == ValueType::I1 { 1 } else { *size * 8 };
        assert_eq!(ty.bit_width(), expected_bits);
        assert_eq!(ty.is_integer(), *is_int);
        assert_eq!(ty.is_bool(), *is_bool);
        assert_eq!(ty.is_float(), *is_float);
    }
}

#[test]
fn int_for_byte_size_to_node_output_type() {
    assert_eq!(ValueType::int_for_byte_size(1).unwrap(), ValueType::I8);
    assert_eq!(ValueType::int_for_byte_size(2).unwrap(), ValueType::I16);
    assert_eq!(ValueType::int_for_byte_size(3).unwrap(), ValueType::I24);
    assert_eq!(ValueType::int_for_byte_size(4).unwrap(), ValueType::I32);
    assert_eq!(ValueType::int_for_byte_size(5).unwrap(), ValueType::I40);
    assert_eq!(ValueType::int_for_byte_size(7).unwrap(), ValueType::I56);
    assert_eq!(ValueType::int_for_byte_size(8).unwrap(), ValueType::I64);
    assert_eq!(ValueType::int_for_byte_size(9).unwrap(), ValueType::I72);
    assert_eq!(ValueType::int_for_byte_size(16).unwrap(), ValueType::I128);
    assert_eq!(ValueType::int_for_byte_size(32).unwrap(), ValueType::I256);
    assert_eq!(ValueType::int_for_byte_size(64).unwrap(), ValueType::I512);
    for bad in [0u32, 11, 13, 15, 17, 33, 65] {
        let err = ValueType::int_for_byte_size(bad).expect_err("invalid size");
        let msg = err.to_string();
        assert!(
            msg.contains(&format!("unsupported node output size: {bad} bytes")),
            "wrong error for {bad}: {err:?}"
        );
    }
}

/// Hand-maintained: a new variant must be appended here or the equivalence
/// tests below silently stop covering it.
fn every_node_kind_smoke() -> Vec<NodeKind> {
    use crate::node::{
        ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp,
    };
    use cranelift_entity::EntityRef;
    let space = rsleigh::VnSpace::RAM;
    vec![
        NodeKind::Entry,
        NodeKind::InitialMemory,
        NodeKind::InitialVar(crate::node::InitialVnId::from_index(0)),
        NodeKind::Region,
        NodeKind::MemPhi,
        NodeKind::Phi,
        NodeKind::If,
        NodeKind::Switch,
        NodeKind::Call,
        NodeKind::Return,
        NodeKind::IndirectBranch,
        NodeKind::CallOther { user_op_id: 0 },
        NodeKind::Load(space),
        NodeKind::Store(space),
        NodeKind::IntConst(crate::node::const_value::ConstId::new(0_usize)),
        NodeKind::IntConst(crate::node::const_value::ConstId::new(0)),
        NodeKind::IntUnaryOp(IntUnaryOp::Neg),
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        NodeKind::IntCmpOp(IntCmpOp::Equal),
        NodeKind::Truncate,
        NodeKind::Extend(ExtendOp::ZeroExtend),
        NodeKind::Popcount,
        NodeKind::Lzcount,
        NodeKind::FloatConst(0),
        NodeKind::FloatBinaryOp(FloatBinaryOp::Add),
        NodeKind::FloatUnaryOp(FloatUnaryOp::Neg),
        NodeKind::FloatCmpOp(FloatCmpOp::Equal),
        NodeKind::IntToFloat,
        NodeKind::IntBitsToFloat,
        NodeKind::FloatToInt,
        NodeKind::FloatBitsToInt,
        NodeKind::FloatToFloat,
        NodeKind::SegmentOp { op_id: 0 },
        NodeKind::CPoolRef,
        NodeKind::New,
    ]
}

/// Independent restatement of `is_cacheable` as a single negated `matches!`.
fn legacy_is_cacheable(kind: &NodeKind) -> bool {
    !matches!(
        kind,
        NodeKind::Return
            | NodeKind::IndirectBranch
            | NodeKind::Switch
            | NodeKind::Region
            | NodeKind::MemPhi
            | NodeKind::Phi
            | NodeKind::Call
            | NodeKind::CallOther { .. }
            | NodeKind::CPoolRef
            | NodeKind::New
    )
}

/// Independent restatement of `asm_fingerprint_exempt`.
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

#[test]
fn same_value_distinct_width_shares_const_id_distinct_node() {
    use crate::node::{NodeKind, ValueType};
    use crate::{IRBuilderExt, IRViewer};
    let mut f = crate::function::test_function();
    // Interning is by value, and both widths hold 42.
    let v80 = f.build_int_const(42u128, ValueType::I80).unwrap();
    let v128 = f.build_int_const(42u128, ValueType::I128).unwrap();
    let n80 = f.producer(v80);
    let n128 = f.producer(v128);
    let (NodeKind::IntConst(id80), NodeKind::IntConst(id128)) =
        (*f.node_kind(n80), *f.node_kind(n128))
    else {
        panic!("expected IntConst nodes")
    };
    assert_eq!(id80, id128, "equal value must share one ConstId");
    // Same ConstId, but the differing output type must keep them separate.
    assert_ne!(
        n80, n128,
        "different declared widths must be distinct nodes"
    );
    assert_eq!(f.int_const_u128(v80), Some(42));
    assert_eq!(f.int_const_u128(v128), Some(42));
}
