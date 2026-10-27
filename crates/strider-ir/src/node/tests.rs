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

/// Exactly the integer types count as integers; the non-value kinds do not.
#[test]
fn is_integer_for_all_integer_output_types() {
    for ty in ValueType::SUPPORTED {
        assert_eq!(ValueKind::Typed(ty).is_integer(), !ty.is_float(), "{ty:?}");
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

/// Independent restatement of `has_side_effects`, spelled out per variant
/// rather than derived from `has_control_flow`.
fn legacy_has_side_effects(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Entry
            | NodeKind::Region
            | NodeKind::If
            | NodeKind::Switch(_)
            | NodeKind::Call { .. }
            | NodeKind::CallOther { .. }
            | NodeKind::Return
            | NodeKind::IndirectBranch
            | NodeKind::Unreachable
            | NodeKind::Store(_)
            | NodeKind::CPoolRef
            | NodeKind::New
    )
}

#[test]
fn has_side_effects_matches_legacy() {
    for k in every_node_kind() {
        assert_eq!(
            k.has_side_effects(),
            legacy_has_side_effects(&k),
            "has_side_effects disagrees with legacy for {k:?}"
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

#[test]
fn the_shifts_are_the_three_shift_ops() {
    let shifts: Vec<IntBinaryOp> = IntBinaryOp::ALL
        .into_iter()
        .filter(|op| op.is_shift())
        .collect();
    assert_eq!(
        shifts,
        [
            IntBinaryOp::ShiftRight,
            IntBinaryOp::SShiftRight,
            IntBinaryOp::ShiftLeft
        ]
    );
}
