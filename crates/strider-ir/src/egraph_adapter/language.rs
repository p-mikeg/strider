//! `StriderLang` — `egg::Language` impl for the acyclic value-slice egraph.
//!
//! Phase 1 Task 1.5 spike — populated in step 2 of the task.
//!
//! # Design
//!
//! Two categories of variants:
//!
//! 1. **Opaque leaves** — `Opaque(u64)`. Every leaf carries a unique
//!    stable id (derived from the source `NodeId`) so distinct strider
//!    nodes can never share an e-class. The plan's "phi nodes are
//!    leaves" invariant is enforced because phi-rooted opaque ids are
//!    distinct from each other and from `InitialVar` / `Load` / `Call`
//!    leaves.
//!
//! 2. **Internal e-nodes** — `IntConst(value, ty)`, `BoolConst(b)`,
//!    `FloatConst(value, ty)`, and the value-producing arithmetic /
//!    comparison / cast / boolean / float ops. These carry their
//!    full payload (op variant + output type) so that two structurally
//!    equal nodes hash and compare equal in egg's dedup cache.
//!
//! The op-bearing variants store an output-type discriminant
//! (`TypeKey`) so that an `IntBinaryOp(Add)` of `U32` and one of `U64`
//! never collide. The original strider graph distinguishes those via
//! the `NodeOutputKind` of the value output; we mirror that here.
//!
//! Unary and binary ops are flattened into single variants tagged by
//! the underlying `IntBinaryOp` / `BoolBinaryOp` / etc. discriminants
//! (kept stable as `u8` rather than rsleigh's `Vn` so the variant
//! payload is `Hash + Eq + Ord` cheap).

use egg::Id;

use crate::{
    BoolBinaryOp, BoolUnaryOp, ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp,
    IntCmpOp, IntUnaryOp,
    node::NodeOutputType,
};

/// A compact, `Hash + Eq + Ord`-friendly discriminator for `NodeOutputType`.
///
/// `NodeOutputType` already derives `Hash + Eq + Ord` so this is mostly a
/// terminology marker — the wrapper is here to highlight where output-type
/// disambiguation matters for egraph hashing.
pub type TypeKey = NodeOutputType;

/// The acyclic value-slice egraph language.  See module doc for the design.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StriderLang {
    // ── Opaque leaves ──────────────────────────────────────────────────────
    /// Opaque leaf — represents `VarPhi`, `MemPhi`, `InitialVar`,
    /// `InitialMemory`, `FunctionArg`, `Load` value output, or a
    /// `Call`/`CallOther` value output.  The `u64` is a stable id derived
    /// from the originating strider `NodeId`'s arena index so distinct
    /// strider opaque nodes never collide in the egraph (different phi
    /// sites stay in different e-classes — V1 verification invariant).
    Opaque(u64),

    // ── Constants ──────────────────────────────────────────────────────────
    /// Integer constant.  Payload is `(value, type)` so a `0u32` and a
    /// `0u64` constant produce distinct e-classes (matching strider's
    /// per-width dedup semantics).
    IntConst(u128, TypeKey),
    /// Boolean constant.
    BoolConst(bool),
    /// Float constant (raw bit pattern in a `u64`; `F32` zeroes the upper
    /// 32 bits, `F64` uses the full payload).  Payload is `(bits, type)`.
    FloatConst(u64, TypeKey),

    // ── Integer ops ────────────────────────────────────────────────────────
    /// Integer binary op tagged by `IntBinaryOp` and output type.
    IntBin(IntBinaryOp, TypeKey, [Id; 2]),
    /// Integer unary op tagged by `IntUnaryOp` and output type.
    IntUn(IntUnaryOp, TypeKey, [Id; 1]),
    /// Integer comparison op tagged by `IntCmpOp`.  Output is always `Bool`,
    /// so no output-type payload is needed.
    IntCmp(IntCmpOp, [Id; 2]),
    /// `CastToInt` — single value input, output type follows the cast target.
    CastToInt(TypeKey, [Id; 1]),
    /// `Truncate` — narrower integer output type.
    Truncate(TypeKey, [Id; 1]),
    /// `Popcount` — counts set bits; output type tracks the result width.
    Popcount(TypeKey, [Id; 1]),
    /// `Lzcount` — leading zeros; output type tracks the result width.
    Lzcount(TypeKey, [Id; 1]),
    /// `Extend` — zero- or sign-extension; payload includes the fill mode.
    Extend(ExtendOp, TypeKey, [Id; 1]),

    // ── Boolean ops ────────────────────────────────────────────────────────
    /// Boolean binary op.  Output is always `Bool`.
    BoolBin(BoolBinaryOp, [Id; 2]),
    /// Boolean unary op.  Output is always `Bool`.
    BoolUn(BoolUnaryOp, [Id; 1]),
    /// `CastToBool` — converts an integer/value to `Bool`.
    CastToBool([Id; 1]),

    // ── Float ops ──────────────────────────────────────────────────────────
    /// Float binary op tagged by `FloatBinaryOp` and output float type.
    FloatBin(FloatBinaryOp, TypeKey, [Id; 2]),
    /// Float unary op tagged by `FloatUnaryOp` and output float type.
    FloatUn(FloatUnaryOp, TypeKey, [Id; 1]),
    /// Float comparison.  Output is always `Bool`.
    FloatCmp(FloatCmpOp, [Id; 2]),

    // ── Float / integer conversions ────────────────────────────────────────
    /// `IntToFloat` — int → float (`F32` or `F64`).
    IntToFloat(TypeKey, [Id; 1]),
    /// `FloatToInt` — float → int (truncating toward zero).
    FloatToInt(TypeKey, [Id; 1]),
    /// `FloatToFloat` — change float precision.
    FloatToFloat(TypeKey, [Id; 1]),
    /// `IntBitsToFloat` — bit-cast int → float.
    IntBitsToFloat(TypeKey, [Id; 1]),
    /// `FloatBitsToInt` — bit-cast float → int.
    FloatBitsToInt(TypeKey, [Id; 1]),
    /// `CastToFloat` — generic value → float.
    CastToFloat(TypeKey, [Id; 1]),
}

impl egg::Language for StriderLang {
    type Discriminant = std::mem::Discriminant<Self>;

    #[inline]
    fn discriminant(&self) -> Self::Discriminant {
        std::mem::discriminant(self)
    }

    fn matches(&self, other: &Self) -> bool {
        // Egg contract: `matches` compares operator + payload, NOT children.
        // (Children equivalence is decided by union-find on their e-class ids.)
        // We must compare every payload field that influences strider-side
        // identity.  Operator equality is implied by the discriminant check
        // (egg already short-circuits on `discriminant()` mismatch), but the
        // payload comparison below is explicit for documentation.
        use StriderLang::*;
        if std::mem::discriminant(self) != std::mem::discriminant(other) {
            return false;
        }
        match (self, other) {
            (Opaque(a), Opaque(b)) => a == b,
            (IntConst(va, ta), IntConst(vb, tb)) => va == vb && ta == tb,
            (BoolConst(a), BoolConst(b)) => a == b,
            (FloatConst(va, ta), FloatConst(vb, tb)) => va == vb && ta == tb,
            (IntBin(opa, ta, _), IntBin(opb, tb, _)) => opa == opb && ta == tb,
            (IntUn(opa, ta, _), IntUn(opb, tb, _)) => opa == opb && ta == tb,
            (IntCmp(opa, _), IntCmp(opb, _)) => opa == opb,
            (CastToInt(ta, _), CastToInt(tb, _)) => ta == tb,
            (Truncate(ta, _), Truncate(tb, _)) => ta == tb,
            (Popcount(ta, _), Popcount(tb, _)) => ta == tb,
            (Lzcount(ta, _), Lzcount(tb, _)) => ta == tb,
            (Extend(opa, ta, _), Extend(opb, tb, _)) => opa == opb && ta == tb,
            (BoolBin(opa, _), BoolBin(opb, _)) => opa == opb,
            (BoolUn(opa, _), BoolUn(opb, _)) => opa == opb,
            (CastToBool(_), CastToBool(_)) => true,
            (FloatBin(opa, ta, _), FloatBin(opb, tb, _)) => opa == opb && ta == tb,
            (FloatUn(opa, ta, _), FloatUn(opb, tb, _)) => opa == opb && ta == tb,
            (FloatCmp(opa, _), FloatCmp(opb, _)) => opa == opb,
            (IntToFloat(ta, _), IntToFloat(tb, _)) => ta == tb,
            (FloatToInt(ta, _), FloatToInt(tb, _)) => ta == tb,
            (FloatToFloat(ta, _), FloatToFloat(tb, _)) => ta == tb,
            (IntBitsToFloat(ta, _), IntBitsToFloat(tb, _)) => ta == tb,
            (FloatBitsToInt(ta, _), FloatBitsToInt(tb, _)) => ta == tb,
            (CastToFloat(ta, _), CastToFloat(tb, _)) => ta == tb,
            _ => false,
        }
    }

    fn children(&self) -> &[Id] {
        use StriderLang::*;
        match self {
            Opaque(_) | IntConst(..) | BoolConst(_) | FloatConst(..) => &[],
            IntBin(_, _, ids) => ids,
            IntUn(_, _, ids) => ids,
            IntCmp(_, ids) => ids,
            CastToInt(_, ids) => ids,
            Truncate(_, ids) => ids,
            Popcount(_, ids) => ids,
            Lzcount(_, ids) => ids,
            Extend(_, _, ids) => ids,
            BoolBin(_, ids) => ids,
            BoolUn(_, ids) => ids,
            CastToBool(ids) => ids,
            FloatBin(_, _, ids) => ids,
            FloatUn(_, _, ids) => ids,
            FloatCmp(_, ids) => ids,
            IntToFloat(_, ids) => ids,
            FloatToInt(_, ids) => ids,
            FloatToFloat(_, ids) => ids,
            IntBitsToFloat(_, ids) => ids,
            FloatBitsToInt(_, ids) => ids,
            CastToFloat(_, ids) => ids,
        }
    }

    fn children_mut(&mut self) -> &mut [Id] {
        use StriderLang::*;
        match self {
            Opaque(_) | IntConst(..) | BoolConst(_) | FloatConst(..) => &mut [],
            IntBin(_, _, ids) => ids,
            IntUn(_, _, ids) => ids,
            IntCmp(_, ids) => ids,
            CastToInt(_, ids) => ids,
            Truncate(_, ids) => ids,
            Popcount(_, ids) => ids,
            Lzcount(_, ids) => ids,
            Extend(_, _, ids) => ids,
            BoolBin(_, ids) => ids,
            BoolUn(_, ids) => ids,
            CastToBool(ids) => ids,
            FloatBin(_, _, ids) => ids,
            FloatUn(_, _, ids) => ids,
            FloatCmp(_, ids) => ids,
            IntToFloat(_, ids) => ids,
            FloatToInt(_, ids) => ids,
            FloatToFloat(_, ids) => ids,
            IntBitsToFloat(_, ids) => ids,
            FloatBitsToInt(_, ids) => ids,
            CastToFloat(_, ids) => ids,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egg::Language;

    fn id(n: u32) -> Id {
        Id::from(n as usize)
    }

    /// Two opaque leaves with distinct payloads must NOT match — they
    /// represent distinct strider nodes (different phi sites, different
    /// `InitialVar`s, etc.) and must land in different e-classes.
    #[test]
    fn opaque_leaves_with_distinct_ids_do_not_match() {
        let a = StriderLang::Opaque(1);
        let b = StriderLang::Opaque(2);
        assert!(!a.matches(&b));
    }

    /// Two opaque leaves with the same payload MUST match — same strider
    /// node round-tripped through `add()` should hit the same e-class.
    #[test]
    fn opaque_leaves_with_same_id_match() {
        let a = StriderLang::Opaque(1);
        let b = StriderLang::Opaque(1);
        assert!(a.matches(&b));
    }

    /// Constants distinguished by value: `IntConst(5, U64)` ≠
    /// `IntConst(7, U64)`.  The output-type payload is what makes a
    /// `0u32` differ from `0u64` (verified below).
    #[test]
    fn int_const_distinguishes_value() {
        let a = StriderLang::IntConst(5, NodeOutputType::U64);
        let b = StriderLang::IntConst(7, NodeOutputType::U64);
        assert!(!a.matches(&b));
    }

    /// Constants of the same value but different types must NOT match.
    /// Mirrors strider's per-width dedup semantics.
    #[test]
    fn int_const_distinguishes_type() {
        let a = StriderLang::IntConst(0, NodeOutputType::U32);
        let b = StriderLang::IntConst(0, NodeOutputType::U64);
        assert!(!a.matches(&b));
    }

    /// Same value AND same type → match (egg will dedupe into one e-class).
    #[test]
    fn int_const_same_value_and_type_match() {
        let a = StriderLang::IntConst(42, NodeOutputType::U32);
        let b = StriderLang::IntConst(42, NodeOutputType::U32);
        assert!(a.matches(&b));
    }

    /// Binary ops with the same op + type but DIFFERENT children must
    /// still match — children are compared by union-find, not by
    /// `Language::matches`.
    #[test]
    fn int_bin_matches_ignore_children() {
        let a = StriderLang::IntBin(IntBinaryOp::Add, NodeOutputType::U64, [id(1), id(2)]);
        let b = StriderLang::IntBin(IntBinaryOp::Add, NodeOutputType::U64, [id(3), id(4)]);
        assert!(a.matches(&b));
    }

    /// Different op (Add vs Mul) must NOT match.
    #[test]
    fn int_bin_distinguishes_op() {
        let a = StriderLang::IntBin(IntBinaryOp::Add, NodeOutputType::U64, [id(1), id(2)]);
        let b = StriderLang::IntBin(IntBinaryOp::Mul, NodeOutputType::U64, [id(1), id(2)]);
        assert!(!a.matches(&b));
    }

    /// Different output type must NOT match — keeps `Add(_:U32)` distinct
    /// from `Add(_:U64)`.
    #[test]
    fn int_bin_distinguishes_output_type() {
        let a = StriderLang::IntBin(IntBinaryOp::Add, NodeOutputType::U32, [id(1), id(2)]);
        let b = StriderLang::IntBin(IntBinaryOp::Add, NodeOutputType::U64, [id(1), id(2)]);
        assert!(!a.matches(&b));
    }

    /// `children()` for an opaque leaf is empty.
    #[test]
    fn opaque_leaf_has_no_children() {
        let a = StriderLang::Opaque(42);
        assert!(a.children().is_empty());
    }

    /// `children()` for an `IntBin` returns the two child Ids in order.
    #[test]
    fn int_bin_has_two_children() {
        let a = StriderLang::IntBin(IntBinaryOp::Add, NodeOutputType::U32, [id(7), id(8)]);
        assert_eq!(a.children(), &[id(7), id(8)]);
    }

    /// Different leaf kinds (e.g. `IntConst` vs `BoolConst`) never match
    /// — guards against a future refactor that lets them share a
    /// discriminant by accident.
    #[test]
    fn distinct_variant_kinds_never_match() {
        let a = StriderLang::IntConst(1, NodeOutputType::Bool);
        let b = StriderLang::BoolConst(true);
        assert!(!a.matches(&b));
    }
}
