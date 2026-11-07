use super::*;
use crate::ExtendOp;

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
