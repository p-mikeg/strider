use super::*;
use crate::node_signature::every_node_kind;
use cranelift_entity::EntityRef;

#[test]
fn signed_int_sign_extends_from_declared_width() {
    assert_eq!(
        ValueType::int(8).get_signed_int(u128::from(u8::MAX)),
        Some(-1)
    );
    assert_eq!(
        ValueType::int(8).get_signed_int(u128::from(i8::MIN as u8)),
        Some(i128::from(i8::MIN))
    );
    assert_eq!(
        ValueType::int(8).get_signed_int(i8::MAX as u128),
        Some(i128::from(i8::MAX))
    );
    assert_eq!(
        ValueType::int(16).get_signed_int(u128::from(i16::MIN as u16)),
        Some(i128::from(i16::MIN))
    );
    assert_eq!(
        ValueType::int(32).get_signed_int(u128::from(u32::MAX)),
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
fn is_value_only_for_output_type() {
    assert!(ValueKind::Typed(ValueType::int(64)).is_value());
    assert!(!ValueKind::Control.is_value());
    assert!(!ValueKind::PhiToken.is_value());
    assert!(!ValueKind::Memory.is_value());
}

#[test]
fn is_bool_only_for_bool_output_type() {
    assert!(ValueKind::Typed(ValueType::I1).is_bool());
    assert!(!ValueKind::Typed(ValueType::int(8)).is_bool());
    assert!(!ValueKind::Control.is_bool());
}

/// `I1` counts as an integer; floats and the non-value kinds do not.
#[test]
fn is_integer_for_all_integer_output_types() {
    for ty in [
        ValueType::I1,
        ValueType::int(8),
        ValueType::int(16),
        ValueType::int(24),
        ValueType::int(32),
        ValueType::int(40),
        ValueType::int(48),
        ValueType::int(56),
        ValueType::int(64),
        ValueType::int(72),
        ValueType::int(80),
        ValueType::int(96),
        ValueType::int(112),
        ValueType::int(128),
        ValueType::int(256),
        ValueType::int(512),
    ] {
        assert!(
            ValueKind::Typed(ty).is_integer(),
            "{ty:?} should be integer"
        );
    }
    for ty in [
        ValueType::float(16),
        ValueType::float(32),
        ValueType::float(64),
        ValueType::float(80),
        ValueType::float(128),
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

#[test]
fn get_unsigned_int_returns_none_for_floats() {
    assert_eq!(ValueType::float(32).get_unsigned_int(0x3F800000), None);
    assert_eq!(
        ValueType::float(64).get_unsigned_int(0x3FF0000000000000),
        None
    );
}

#[test]
fn get_signed_int_returns_none_for_floats() {
    assert_eq!(ValueType::float(32).get_signed_int(0x3F800000), None);
    assert_eq!(
        ValueType::float(64).get_signed_int(0x3FF0000000000000),
        None
    );
}

#[test]
fn as_value_or_err_value_case() {
    let kind = ValueKind::Typed(ValueType::int(32));
    assert_eq!(kind.as_value_or_err().unwrap(), ValueType::int(32));
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
        (ValueType::int(8), "i8", 1, true, false, false),
        (ValueType::int(16), "i16", 2, true, false, false),
        (ValueType::int(24), "i24", 3, true, false, false),
        (ValueType::int(32), "i32", 4, true, false, false),
        (ValueType::int(40), "i40", 5, true, false, false),
        (ValueType::int(48), "i48", 6, true, false, false),
        (ValueType::int(56), "i56", 7, true, false, false),
        (ValueType::int(64), "i64", 8, true, false, false),
        (ValueType::int(72), "i72", 9, true, false, false),
        (ValueType::int(80), "i80", 10, true, false, false),
        (ValueType::int(96), "i96", 12, true, false, false),
        (ValueType::int(112), "i112", 14, true, false, false),
        (ValueType::int(128), "i128", 16, true, false, false),
        (ValueType::int(256), "i256", 32, true, false, false),
        (ValueType::int(512), "i512", 64, true, false, false),
        (ValueType::float(16), "f16", 2, false, false, true),
        (ValueType::float(32), "f32", 4, false, false, true),
        (ValueType::float(64), "f64", 8, false, false, true),
        (ValueType::float(80), "f80", 10, false, false, true),
        (ValueType::float(128), "f128", 16, false, false, true),
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
    assert_eq!(ValueType::int_for_byte_size(1).unwrap(), ValueType::int(8));
    assert_eq!(ValueType::int_for_byte_size(2).unwrap(), ValueType::int(16));
    assert_eq!(ValueType::int_for_byte_size(3).unwrap(), ValueType::int(24));
    assert_eq!(ValueType::int_for_byte_size(4).unwrap(), ValueType::int(32));
    assert_eq!(ValueType::int_for_byte_size(5).unwrap(), ValueType::int(40));
    assert_eq!(ValueType::int_for_byte_size(7).unwrap(), ValueType::int(56));
    assert_eq!(ValueType::int_for_byte_size(8).unwrap(), ValueType::int(64));
    assert_eq!(ValueType::int_for_byte_size(9).unwrap(), ValueType::int(72));
    assert_eq!(
        ValueType::int_for_byte_size(16).unwrap(),
        ValueType::int(128)
    );
    assert_eq!(
        ValueType::int_for_byte_size(32).unwrap(),
        ValueType::int(256)
    );
    assert_eq!(
        ValueType::int_for_byte_size(64).unwrap(),
        ValueType::int(512)
    );
    for bad in [0u32, 11, 13, 15, 17, 33, 65] {
        let err = ValueType::int_for_byte_size(bad).expect_err("invalid size");
        let msg = err.to_string();
        assert!(
            msg.contains(&format!("unsupported node output size: {bad} bytes")),
            "wrong error for {bad}: {err:?}"
        );
    }
}

/// Independent restatement of `is_cacheable` as a single negated `matches!`.
fn legacy_is_cacheable(kind: &NodeKind) -> bool {
    !matches!(
        kind,
        NodeKind::If
            | NodeKind::Return
            | NodeKind::IndirectBranch
            | NodeKind::Unreachable
            | NodeKind::Switch(_)
            | NodeKind::Region
            | NodeKind::MemPhi
            | NodeKind::Phi
            | NodeKind::Call { .. }
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
    for k in every_node_kind() {
        assert_eq!(
            k.is_cacheable(),
            legacy_is_cacheable(&k),
            "is_cacheable disagrees with legacy for {k:?}"
        );
    }
}

#[test]
fn asm_fingerprint_exempt_matches_legacy() {
    for k in every_node_kind() {
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
    let v80 = f.build_int_const(42u128, ValueType::int(80)).unwrap();
    let v128 = f.build_int_const(42u128, ValueType::int(128)).unwrap();
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
