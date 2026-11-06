use super::*;
use crate::node::signature::every_node_kind;
use cranelift_entity::EntityRef;

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
    assert!(NodeKind::IntConst(crate::node::IntConstId::new(42_usize)).is_const());
    assert!(NodeKind::FloatConst(0).is_const());
    assert!(!NodeKind::Entry.is_const());
    assert!(!NodeKind::Return.is_const());
}

/// Independent restatement of `is_cacheable` as a single negated `matches!`.
fn expected_is_cacheable(kind: &NodeKind) -> bool {
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
fn expected_asm_fingerprint_exempt(kind: &NodeKind) -> bool {
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
fn is_cacheable_matches_expected() {
    for k in every_node_kind() {
        assert_eq!(
            k.is_cacheable(),
            expected_is_cacheable(&k),
            "is_cacheable disagrees with the expected set for {k:?}"
        );
    }
}

#[test]
fn asm_fingerprint_exempt_matches_expected() {
    for k in every_node_kind() {
        assert_eq!(
            k.asm_fingerprint_exempt(),
            expected_asm_fingerprint_exempt(&k),
            "asm_fingerprint_exempt disagrees with the expected set for {k:?}"
        );
    }
}

/// Independent restatement of `has_side_effects`, spelled out per variant
/// rather than derived from `has_control_flow`.
fn expected_has_side_effects(kind: &NodeKind) -> bool {
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
fn has_side_effects_matches_expected() {
    for k in every_node_kind() {
        assert_eq!(
            k.has_side_effects(),
            expected_has_side_effects(&k),
            "has_side_effects disagrees with the expected set for {k:?}"
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
    assert_eq!(id80, id128, "equal value must share one IntConstId");
    // Same IntConstId, but the differing output type must keep them separate.
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
