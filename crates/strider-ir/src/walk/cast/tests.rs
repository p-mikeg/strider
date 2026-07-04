//! Semantics tests for [`CastMask`] and [`cast_mask_of`].

use super::*;
use crate::{ExtendOp, FloatBinaryOp, IntBinaryOp, IntUnaryOp};
use cranelift_entity::EntityRef;

// ── Bit-distinctness ────────────────────────────────────────────────────────

/// All individual flags must use distinct, non-overlapping bits.
/// If any two share a bit, `bit_a & bit_b` will be non-empty.
#[test]
fn individual_flags_use_distinct_bits() {
    let flags = [
        CastMask::ZERO_EXTEND,
        CastMask::SIGN_EXTEND,
        CastMask::TRUNCATE,
        CastMask::INT_BITS_TO_FLOAT,
        CastMask::FLOAT_BITS_TO_INT,
    ];
    for (i, &a) in flags.iter().enumerate() {
        // Each flag must be non-empty (a single bit).
        assert!(!a.is_empty(), "flag #{i} ({a:?}) should be a single bit");
        for (j, &b) in flags.iter().enumerate() {
            if i == j {
                continue;
            }
            assert!(
                (a & b).is_empty(),
                "flags #{i} ({a:?}) and #{j} ({b:?}) overlap"
            );
        }
    }
}

/// `EXTEND` is exactly the union of `ZERO_EXTEND | SIGN_EXTEND`.
#[test]
fn extend_is_zero_or_sign_extend() {
    assert_eq!(
        CastMask::EXTEND,
        CastMask::ZERO_EXTEND | CastMask::SIGN_EXTEND
    );
    assert!(CastMask::EXTEND.contains(CastMask::ZERO_EXTEND));
    assert!(CastMask::EXTEND.contains(CastMask::SIGN_EXTEND));
}

// ── all() / empty() ─────────────────────────────────────────────────────────

/// `CastMask::all()` must contain every individual flag.
#[test]
fn all_contains_every_individual_flag() {
    let individuals = [
        CastMask::ZERO_EXTEND,
        CastMask::SIGN_EXTEND,
        CastMask::TRUNCATE,
        CastMask::INT_BITS_TO_FLOAT,
        CastMask::FLOAT_BITS_TO_INT,
    ];
    let all = CastMask::all();
    for f in individuals {
        assert!(all.contains(f), "CastMask::all() missing {f:?}");
    }
}

/// `CastMask::empty().is_empty()`; `CastMask::all().is_empty()` is false.
#[test]
fn empty_and_all_predicates() {
    assert!(CastMask::empty().is_empty());
    assert!(!CastMask::all().is_empty());
}

// ── Bit operators round-trip ────────────────────────────────────────────────

/// `BitOr` adds bits; `BitAnd` intersects; `Not` flips within the
/// defined-flags universe (bitflags 2.x semantics).
#[test]
fn bit_operators_round_trip() {
    let trunc = CastMask::TRUNCATE;
    let zext = CastMask::ZERO_EXTEND;

    // OR adds.
    let both = trunc | zext;
    assert!(both.contains(trunc));
    assert!(both.contains(zext));

    // AND intersects.
    assert_eq!(both & trunc, trunc);
    assert_eq!(both & zext, zext);
    assert_eq!(trunc & zext, CastMask::empty());

    // NOT (within defined bits): bitflags 2.x defines `!x` as
    // `Self::all() ^ x`, so `!empty == all` and `!all == empty`.
    assert_eq!(!CastMask::empty(), CastMask::all());
    assert_eq!(!CastMask::all(), CastMask::empty());

    // `!trunc | trunc == all` (set complement is full).
    assert_eq!((!trunc) | trunc, CastMask::all());
    // `!trunc & trunc == empty`.
    assert_eq!((!trunc) & trunc, CastMask::empty());
}

// ── cast_mask_of: cast kinds ────────────────────────────────────────────────

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

// ── cast_mask_of: non-cast kinds ────────────────────────────────────────────

/// A representative selection of non-cast `NodeKind`s must yield
/// `CastMask::empty()`.  (Exhaustive coverage is enforced by the no-`_`
/// match in `cast_mask_of` itself — adding a `NodeKind` variant without
/// classifying it is a compile error.)
#[test]
fn cast_mask_of_non_cast_kinds_is_empty() {
    let non_casts = [
        NodeKind::Entry,
        NodeKind::IntConst(crate::node::const_value::ConstId::new(0_usize)),
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        NodeKind::FloatToFloat,
        NodeKind::Region,
    ];
    for k in non_casts {
        assert_eq!(
            cast_mask_of(&k),
            CastMask::empty(),
            "expected cast_mask_of({k:?}) = empty()"
        );
    }
}

/// All value-passthrough cast kinds must yield a non-empty mask.
#[test]
fn cast_mask_of_returns_non_empty_for_all_cast_kinds() {
    let casts = [
        NodeKind::Extend(ExtendOp::ZeroExtend),
        NodeKind::Extend(ExtendOp::SignExtend),
        NodeKind::Truncate,
        NodeKind::IntBitsToFloat,
        NodeKind::FloatBitsToInt,
    ];
    for k in casts {
        assert!(
            !cast_mask_of(&k).is_empty(),
            "expected cast_mask_of({k:?}) to be non-empty"
        );
    }
}

/// A broader representative selection of non-cast kinds must yield
/// empty.  Complements `cast_mask_of_non_cast_kinds_is_empty` above
/// by covering more node families (unary/binary ops, bools, floats,
/// memory, region/control, call).
#[test]
fn cast_mask_of_returns_empty_for_non_cast_kinds() {
    let non_casts = [
        NodeKind::Entry,
        NodeKind::IntConst(crate::node::const_value::ConstId::new(0_usize)),
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
        NodeKind::Switch,
        NodeKind::Call,
    ];
    for k in non_casts {
        assert_eq!(
            cast_mask_of(&k),
            CastMask::empty(),
            "expected cast_mask_of({k:?}) = empty"
        );
    }
}

/// Sanity check on FloatToFloat / FloatToInt / IntToFloat — these
/// are float **conversions** (semantic value change), not bit-level
/// casts.  They must NOT be in the walk-through set: a pattern
/// looking for a Mul should not silently match through a
/// FloatToInt that semantically changed the value.
#[test]
fn cast_mask_of_excludes_float_conversions() {
    assert_eq!(cast_mask_of(&NodeKind::FloatToFloat), CastMask::empty());
    assert_eq!(cast_mask_of(&NodeKind::FloatToInt), CastMask::empty());
    assert_eq!(cast_mask_of(&NodeKind::IntToFloat), CastMask::empty());
}
