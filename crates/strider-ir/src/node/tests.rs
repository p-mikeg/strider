use super::*;
use crate::node::signature::every_node_kind;

/// Exactly the integer types count as integers; the non-value kinds do not.
#[test]
fn is_integer_for_all_integer_output_types() {
    for ty in ValueType::SUPPORTED {
        assert_eq!(ValueKind::Typed(ty).is_integer(), !ty.is_float(), "{ty:?}");
    }
    assert!(!ValueKind::Control.is_integer());
    assert!(!ValueKind::Memory.is_integer());
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
