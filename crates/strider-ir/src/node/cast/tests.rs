use super::*;
use crate::{ExtendOp, FloatBinaryOp, IntBinaryOp, IntUnaryOp};
use cranelift_entity::EntityRef;

#[test]
fn cast_mask_of_zero_extend() {
    assert_eq!(
        cast_mask_of(&NodeKind::Extend(ExtendOp::ZeroExtend)),
        CastMask::ZERO_EXTEND
    );
}

#[test]
fn cast_mask_of_sign_extend() {
    assert_eq!(
        cast_mask_of(&NodeKind::Extend(ExtendOp::SignExtend)),
        CastMask::SIGN_EXTEND
    );
}

#[test]
fn cast_mask_of_truncate() {
    assert_eq!(cast_mask_of(&NodeKind::Truncate), CastMask::TRUNCATE);
}

#[test]
fn cast_mask_of_int_bits_to_float() {
    assert_eq!(
        cast_mask_of(&NodeKind::IntBitsToFloat),
        CastMask::INT_BITS_TO_FLOAT
    );
}

#[test]
fn cast_mask_of_float_bits_to_int() {
    assert_eq!(
        cast_mask_of(&NodeKind::FloatBitsToInt),
        CastMask::FLOAT_BITS_TO_INT
    );
}

/// Spot check only; exhaustiveness is enforced by the no-`_` match in
/// `NodeKind::flags`. Float conversions change the value, unlike bit-level
/// casts, so they stay out of the walk-through set too.
#[test]
fn cast_mask_of_returns_empty_for_non_cast_kinds() {
    let non_casts = [
        NodeKind::Entry,
        NodeKind::IntConst(crate::node::IntConstId::new(0_usize)),
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        NodeKind::IntBinaryOp(IntBinaryOp::Mul),
        NodeKind::IntUnaryOp(IntUnaryOp::Neg),
        NodeKind::FloatBinaryOp(FloatBinaryOp::Add),
        NodeKind::FloatToFloat,
        NodeKind::FloatToInt,
        NodeKind::IntToFloat,
        NodeKind::Return,
        NodeKind::Region,
        NodeKind::MemPhi,
        NodeKind::If,
        NodeKind::Switch(crate::node::SwitchTableId::from_u32(0)),
        NodeKind::Call { cc: None },
    ];
    for k in non_casts {
        assert_eq!(
            cast_mask_of(&k),
            CastMask::empty(),
            "expected cast_mask_of({k:?}) = empty"
        );
    }
}

/// Every kind against a frozen table: the five casts carry their bit, every
/// other kind none.
#[test]
fn cast_mask_of_matches_frozen_table() {
    let casts = [
        (
            NodeKind::Extend(ExtendOp::ZeroExtend),
            CastMask::ZERO_EXTEND,
        ),
        (
            NodeKind::Extend(ExtendOp::SignExtend),
            CastMask::SIGN_EXTEND,
        ),
        (NodeKind::Truncate, CastMask::TRUNCATE),
        (NodeKind::IntBitsToFloat, CastMask::INT_BITS_TO_FLOAT),
        (NodeKind::FloatBitsToInt, CastMask::FLOAT_BITS_TO_INT),
    ];
    let every = crate::node::signature::every_node_kind();
    for kind in every.into_iter().chain(casts.map(|(k, _)| k)) {
        let expected = casts
            .iter()
            .find(|&&(k, _)| k == kind)
            .map_or(CastMask::empty(), |&(_, m)| m);
        assert_eq!(cast_mask_of(&kind), expected, "{kind:?}");
    }
}
