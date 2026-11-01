//! Binary / unary / cast / comparison typed builders.
//!
//! Lift-time canonicalisations (`int_sub` to `int_add(_, int_neg(_))`,
//! `int_not` to `int_xor(_, all_ones)`, the lowered `int_ne` / `int_le` /
//! `float_*` shapes) are reproduced here so a typed pattern matches the
//! canonical IR the lifter emits. Boolean ops pin the output to `I1`.
//! Commutativity is data-driven via `NodeKind::is_commutative`.

use std::marker::PhantomData;

use strider_ir::node::{NodeKind, ValueType};
use strider_ir::{FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp};

use crate::expr::{Expr, IntoExpr};
use crate::matcher::KindSpec;
use crate::matcher::match_pat::MatchPat;
use crate::template::template_pat::TemplatePat;

/// A pattern built from operands of types `T`, valid on whichever sides
/// they all are.
pub struct Op<T>(Expr, PhantomData<fn() -> T>);

impl<T> Op<T> {
    pub(crate) fn new(e: Expr) -> Self {
        Self(e, PhantomData)
    }
}

impl<T> IntoExpr for Op<T> {
    fn into_expr(self) -> Expr {
        self.0
    }
}

impl MatchPat for Op<()> {}
impl<I: MatchPat> MatchPat for Op<(I,)> {}
impl<L: MatchPat, R: MatchPat> MatchPat for Op<(L, R)> {}
impl TemplatePat for Op<()> {}
impl<I: TemplatePat> TemplatePat for Op<(I,)> {}
impl<L: TemplatePat, R: TemplatePat> TemplatePat for Op<(L, R)> {}
impl<T> crate::template::template_pat::sealed::Sealed for Op<T> {}

/// A pattern only the match side accepts: a rewrite RHS must build one
/// concrete shape.
pub struct MatchOnly(pub(crate) Expr);

impl IntoExpr for MatchOnly {
    fn into_expr(self) -> Expr {
        self.0
    }
}

impl MatchPat for MatchOnly {}

/// Any node of `exemplar`'s kind, whatever its variant, over the operands.
fn variant<const N: usize>(exemplar: NodeKind, operands: [Expr; N], i1: bool) -> MatchOnly {
    let ty = i1.then_some(ValueType::I1);
    MatchOnly(Expr::op(KindSpec::variant_of(&exemplar), operands, ty))
}

/// Match any `IntBinaryOp` regardless of variant.
pub fn any_int_binary<L: MatchPat + 'static, R: MatchPat + 'static>(l: L, r: R) -> MatchOnly {
    let kind = NodeKind::IntBinaryOp(IntBinaryOp::Add);
    variant(kind, [l.into_expr(), r.into_expr()], false)
}

/// Match any `IntUnaryOp` regardless of variant.
pub fn any_int_unary<I: MatchPat + 'static>(inner: I) -> MatchOnly {
    variant(
        NodeKind::IntUnaryOp(IntUnaryOp::Neg),
        [inner.into_expr()],
        false,
    )
}

/// Match any `IntCmpOp` regardless of variant. Output `I1`.
pub fn any_int_cmp<L: MatchPat + 'static, R: MatchPat + 'static>(l: L, r: R) -> MatchOnly {
    let kind = NodeKind::IntCmpOp(IntCmpOp::Equal);
    variant(kind, [l.into_expr(), r.into_expr()], true)
}

/// Match any `FloatBinaryOp` regardless of variant.
pub fn any_float_binary<L: MatchPat + 'static, R: MatchPat + 'static>(l: L, r: R) -> MatchOnly {
    let kind = NodeKind::FloatBinaryOp(FloatBinaryOp::Add);
    variant(kind, [l.into_expr(), r.into_expr()], false)
}

/// Match any `FloatUnaryOp` regardless of variant.
pub fn any_float_unary<I: MatchPat + 'static>(inner: I) -> MatchOnly {
    variant(
        NodeKind::FloatUnaryOp(FloatUnaryOp::Neg),
        [inner.into_expr()],
        false,
    )
}

/// Match any `FloatCmpOp` regardless of variant. Output `I1`.
pub fn any_float_cmp<L: MatchPat + 'static, R: MatchPat + 'static>(l: L, r: R) -> MatchOnly {
    let kind = NodeKind::FloatCmpOp(FloatCmpOp::Equal);
    variant(kind, [l.into_expr(), r.into_expr()], true)
}

/// Match any `IntBinaryOp` at `I1` regardless of variant.
pub fn any_bool_binary<L: MatchPat + 'static, R: MatchPat + 'static>(l: L, r: R) -> MatchOnly {
    let kind = NodeKind::IntBinaryOp(IntBinaryOp::And);
    variant(kind, [l.into_expr(), r.into_expr()], true)
}

/// NaN-aware `bool_or(float_lt(l, r), float_eq(l, r))` at `I1`.
pub fn float_le<L: MatchPat + 'static, R: MatchPat + 'static>(lhs: L, rhs: R) -> MatchOnly {
    MatchOnly(Expr::shared(vec![lhs.into_expr(), rhs.into_expr()], |v| {
        let cmp = |op| NodeKind::FloatCmpOp(op);
        let less = Expr::exact_i1(cmp(FloatCmpOp::Less), [Expr::at(v[0]), Expr::at(v[1])]);
        let equal = Expr::exact_i1(cmp(FloatCmpOp::Equal), [Expr::at(v[0]), Expr::at(v[1])]);
        Expr::exact_i1(NodeKind::IntBinaryOp(IntBinaryOp::Or), [less, equal])
    }))
}

/// `int_xor(float_eq(x, x), 1):I1`.
pub fn float_is_nan<I: MatchPat + 'static>(inner: I) -> MatchOnly {
    MatchOnly(Expr::shared(vec![inner.into_expr()], |v| {
        let eq = NodeKind::FloatCmpOp(FloatCmpOp::Equal);
        Expr::bool_not(Expr::exact_i1(eq, [Expr::at(v[0]), Expr::at(v[0])]))
    }))
}

/// `$ctor` is [`Expr::exact`] or [`Expr::exact_i1`]; with `op`, the op is a
/// runtime argument of the `$variant` sub-enum.
macro_rules! bin {
    ($name:ident, $ctor:ident, $kind:expr, $doc:expr) => {
        #[doc = $doc]
        pub fn $name<L: IntoExpr + 'static, R: IntoExpr + 'static>(lhs: L, rhs: R) -> Op<(L, R)> {
            let (l, r) = (lhs.into_expr(), rhs.into_expr());
            Op::new(Expr::$ctor($kind, [l, r]))
        }
    };
    ($name:ident, $ctor:ident, op $variant:ident, $doc:expr) => {
        #[doc = $doc]
        pub fn $name<L: IntoExpr + 'static, R: IntoExpr + 'static>(
            op: $variant,
            lhs: L,
            rhs: R,
        ) -> Op<(L, R)> {
            let (l, r) = (lhs.into_expr(), rhs.into_expr());
            Op::new(Expr::$ctor(NodeKind::$variant(op), [l, r]))
        }
    };
}

macro_rules! un {
    ($name:ident, $kind:expr, $doc:expr) => {
        #[doc = $doc]
        pub fn $name<I: IntoExpr + 'static>(inner: I) -> Op<(I,)> {
            Op::new(Expr::exact($kind, [inner.into_expr()]))
        }
    };
}

pub(crate) mod ops {
    use strider_ir::node::NodeKind;
    use strider_ir::{
        ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp,
    };

    use crate::expr::{Expr, IntoExpr};
    use crate::typed::consts::Const;
    use crate::typed::value_ops::Op;

    bin! { int_add, exact, NodeKind::IntBinaryOp(IntBinaryOp::Add),
    "Match or build unsigned addition `lhs + rhs`. Commutative." }
    bin! { int_mul, exact, NodeKind::IntBinaryOp(IntBinaryOp::Mul),
    "Match or build wrapping multiplication `lhs * rhs`. Commutative." }
    bin! { int_div, exact, NodeKind::IntBinaryOp(IntBinaryOp::Div),
    "Match or build unsigned division `lhs / rhs`." }
    bin! { int_sdiv, exact, NodeKind::IntBinaryOp(IntBinaryOp::Sdiv),
    "Match or build signed division `(signed)lhs / (signed)rhs`." }
    bin! { int_rem, exact, NodeKind::IntBinaryOp(IntBinaryOp::Rem),
    "Match or build unsigned remainder `lhs % rhs`." }
    bin! { int_srem, exact, NodeKind::IntBinaryOp(IntBinaryOp::Srem),
    "Match or build signed remainder `(signed)lhs % (signed)rhs`." }
    bin! { int_and, exact, NodeKind::IntBinaryOp(IntBinaryOp::And),
    "Match or build bitwise AND `lhs & rhs`. Commutative." }
    bin! { int_or, exact, NodeKind::IntBinaryOp(IntBinaryOp::Or),
    "Match or build bitwise OR `lhs | rhs`. Commutative." }
    bin! { int_xor, exact, NodeKind::IntBinaryOp(IntBinaryOp::Xor),
    "Match or build bitwise XOR `lhs ^ rhs`. Commutative." }
    bin! { int_shl, exact, NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft),
    "Match or build logical left-shift `lhs << rhs`." }
    bin! { int_shr, exact, NodeKind::IntBinaryOp(IntBinaryOp::ShiftRight),
    "Match or build logical right-shift `lhs >> rhs`." }
    bin! { int_sshr, exact, NodeKind::IntBinaryOp(IntBinaryOp::SShiftRight),
    "Match or build arithmetic right-shift `(signed)lhs >> rhs`." }

    /// Subtraction `lhs - rhs` (the lifter's `Add(lhs, Neg(rhs))` shape).
    pub fn int_sub<L: IntoExpr + 'static, R: IntoExpr + 'static>(lhs: L, rhs: R) -> Op<(L, R)> {
        Op::new(Expr::int_sub(lhs.into_expr(), rhs.into_expr()))
    }

    un! { int_neg, NodeKind::IntUnaryOp(IntUnaryOp::Neg),
    "Match or build two's-complement negation `-inner`." }
    un! { int_popcount, NodeKind::Popcount,
    "Match or build a `Popcount(inner)` (count-set-bits) node." }
    un! { int_lzcount, NodeKind::Lzcount,
    "Match or build an `Lzcount(inner)` (leading-zero-count) node." }

    /// Bitwise complement `~inner` (the canonical `int_xor(_, all_ones)`).
    pub fn int_not<I: IntoExpr + 'static>(inner: I) -> Op<(I,)> {
        let ones = Const::AllOnes.into();
        let xor = NodeKind::IntBinaryOp(IntBinaryOp::Xor);
        Op::new(Expr::exact(xor, [inner.into_expr(), ones]))
    }

    un! { int_truncate, NodeKind::Truncate,
    "Match or build a `Truncate(inner)` (integer narrowing) node." }
    un! { int_zero_extend, NodeKind::Extend(ExtendOp::ZeroExtend),
    "Match or build a zero-extension `Extend(ZeroExtend)(inner)` node." }
    un! { int_sign_extend, NodeKind::Extend(ExtendOp::SignExtend),
    "Match or build a sign-extension `Extend(SignExtend)(inner)` node." }
    un! { int_to_float, NodeKind::IntToFloat,
    "Match or build an `IntToFloat(inner)` value-conversion." }
    un! { float_to_int, NodeKind::FloatToInt,
    "Match or build a `FloatToInt(inner)` value-conversion." }
    un! { int_bits_to_float, NodeKind::IntBitsToFloat,
    "Match or build an `IntBitsToFloat(inner)` bitcast." }
    un! { float_bits_to_int, NodeKind::FloatBitsToInt,
    "Match or build a `FloatBitsToInt(inner)` bitcast." }
    un! { float_to_float, NodeKind::FloatToFloat,
    "Match or build a `FloatToFloat(inner)` precision-conversion." }

    /// An `Extend(op)` node with the given runtime `ExtendOp`.
    pub fn int_extend<I: IntoExpr + 'static>(op: ExtendOp, inner: I) -> Op<(I,)> {
        Op::new(Expr::exact(NodeKind::Extend(op), [inner.into_expr()]))
    }

    bin! { int_eq, exact_i1, NodeKind::IntCmpOp(IntCmpOp::Equal),
    "Match or build an unsigned equality `lhs == rhs`. Commutative." }
    bin! { int_lt, exact_i1, NodeKind::IntCmpOp(IntCmpOp::Less),
    "Match or build an unsigned less-than `lhs < rhs`." }
    bin! { int_slt, exact_i1, NodeKind::IntCmpOp(IntCmpOp::Sless),
    "Match or build a signed less-than `(signed)lhs < (signed)rhs`." }
    bin! { int_carry, exact_i1, NodeKind::IntCmpOp(IntCmpOp::Carry),
    "Match or build an unsigned addition carry-out. Commutative." }
    bin! { int_scarry, exact_i1, NodeKind::IntCmpOp(IntCmpOp::Scarry),
    "Match or build a signed addition overflow. Commutative." }
    bin! { int_sborrow, exact_i1, NodeKind::IntCmpOp(IntCmpOp::Sborrow),
    "Match or build a signed subtraction borrow." }

    /// Match or build an unsigned not-equal `lhs != rhs`, i.e.
    /// `int_xor(int_eq(l, r), 1):I1`; `.ordered()` pins the `int_eq`.
    pub fn int_ne<L: IntoExpr + 'static, R: IntoExpr + 'static>(lhs: L, rhs: R) -> Op<(L, R)> {
        let eq = NodeKind::IntCmpOp(IntCmpOp::Equal);
        let (l, r) = (lhs.into_expr(), rhs.into_expr());
        Op::new(Expr::negated(Expr::exact_i1(eq, [l, r])))
    }

    /// Match or build an unsigned less-or-equal `lhs <= rhs`, i.e.
    /// `int_xor(int_lt(r, l), 1):I1`; `.ordered()` is a no-op, the `int_lt`
    /// orders its pair.
    pub fn int_le<L: IntoExpr + 'static, R: IntoExpr + 'static>(lhs: L, rhs: R) -> Op<(L, R)> {
        let lt = NodeKind::IntCmpOp(IntCmpOp::Less);
        let (l, r) = (lhs.into_expr(), rhs.into_expr());
        Op::new(Expr::negated(Expr::exact_i1(lt, [r, l])))
    }

    /// Match or build a signed less-or-equal `(signed)lhs <= (signed)rhs`, i.e.
    /// `int_xor(int_slt(r, l), 1):I1`; `.ordered()` is a no-op, the `int_slt`
    /// orders its pair.
    pub fn int_sle<L: IntoExpr + 'static, R: IntoExpr + 'static>(lhs: L, rhs: R) -> Op<(L, R)> {
        let slt = NodeKind::IntCmpOp(IntCmpOp::Sless);
        let (l, r) = (lhs.into_expr(), rhs.into_expr());
        Op::new(Expr::negated(Expr::exact_i1(slt, [r, l])))
    }

    bin! { float_add, exact, NodeKind::FloatBinaryOp(FloatBinaryOp::Add),
    "Match or build a float addition `lhs + rhs`. Commutative." }
    bin! { float_mul, exact, NodeKind::FloatBinaryOp(FloatBinaryOp::Mul),
    "Match or build a float multiplication `lhs * rhs`. Commutative." }
    bin! { float_div, exact, NodeKind::FloatBinaryOp(FloatBinaryOp::Div),
    "Match or build a float division `lhs / rhs`." }

    /// Float subtraction `lhs - rhs` (the lifter's `Add(lhs, Neg(rhs))` shape).
    pub fn float_sub<L: IntoExpr + 'static, R: IntoExpr + 'static>(lhs: L, rhs: R) -> Op<(L, R)> {
        let neg = NodeKind::FloatUnaryOp(FloatUnaryOp::Neg);
        let add = NodeKind::FloatBinaryOp(FloatBinaryOp::Add);
        let r = Expr::exact(neg, [rhs.into_expr()]);
        Op::new(Expr::exact(add, [lhs.into_expr(), r]))
    }

    un! { float_neg, NodeKind::FloatUnaryOp(FloatUnaryOp::Neg),
    "Match or build a float negation `-x`." }
    un! { float_abs, NodeKind::FloatUnaryOp(FloatUnaryOp::Abs),
    "Match or build a float absolute value `|x|`." }
    un! { float_sqrt, NodeKind::FloatUnaryOp(FloatUnaryOp::Sqrt),
    "Match or build a float square root `√x`." }
    un! { float_ceil, NodeKind::FloatUnaryOp(FloatUnaryOp::Ceil),
    "Match or build a float ceiling `⌈x⌉`." }
    un! { float_floor, NodeKind::FloatUnaryOp(FloatUnaryOp::Floor),
    "Match or build a float floor `⌊x⌋`." }
    un! { float_round, NodeKind::FloatUnaryOp(FloatUnaryOp::Round),
    "Match or build a float round to nearest, ties away from zero." }

    /// Match or build a float unary op `float_unary(op, x)` of kind `op` only.
    pub fn float_unary<I: IntoExpr + 'static>(op: FloatUnaryOp, inner: I) -> Op<(I,)> {
        Op::new(Expr::exact(NodeKind::FloatUnaryOp(op), [inner.into_expr()]))
    }

    bin! { float_eq, exact_i1, NodeKind::FloatCmpOp(FloatCmpOp::Equal),
    "Match or build a float equality `lhs == rhs`. Commutative." }
    bin! { float_lt, exact_i1, NodeKind::FloatCmpOp(FloatCmpOp::Less),
    "Match or build a float less-than `lhs < rhs`." }

    /// Match or build a float not-equal `lhs != rhs`, i.e.
    /// `int_xor(float_eq(l, r), 1):I1`; `.ordered()` pins the `float_eq`.
    pub fn float_ne<L: IntoExpr + 'static, R: IntoExpr + 'static>(lhs: L, rhs: R) -> Op<(L, R)> {
        let eq = NodeKind::FloatCmpOp(FloatCmpOp::Equal);
        let (l, r) = (lhs.into_expr(), rhs.into_expr());
        Op::new(Expr::negated(Expr::exact_i1(eq, [l, r])))
    }

    bin! { bool_and, exact_i1, NodeKind::IntBinaryOp(IntBinaryOp::And),
    "Match or build a boolean AND (`IntBinaryOp::And` at `I1`). Commutative." }
    bin! { bool_or, exact_i1, NodeKind::IntBinaryOp(IntBinaryOp::Or),
    "Match or build a boolean OR (`IntBinaryOp::Or` at `I1`). Commutative." }
    bin! { bool_xor, exact_i1, NodeKind::IntBinaryOp(IntBinaryOp::Xor),
    "Match or build a boolean XOR (`IntBinaryOp::Xor` at `I1`). Commutative." }

    /// Boolean NOT, i.e. `int_xor(operand, IntConst(1)):I1`.
    pub fn bool_not<I: IntoExpr + 'static>(operand: I) -> Op<(I,)> {
        Op::new(Expr::bool_not(operand.into_expr()))
    }

    bin! { int_binary, exact, op IntBinaryOp,
    "Match or build an integer binary op of kind `op`." }
    bin! { int_cmp, exact_i1, op IntCmpOp,
    "Match or build an integer comparison of kind `op` (output `I1`)." }
    bin! { float_binary, exact, op FloatBinaryOp,
    "Match or build a float binary op of kind `op`." }
    bin! { float_cmp, exact_i1, op FloatCmpOp,
    "Match or build a float comparison of kind `op` (output `I1`)." }
    bin! { bool_binary, exact_i1, op IntBinaryOp,
    "Match or build a boolean binary op of kind `op` (`IntBinaryOp` at `I1`)." }
}

pub use ops::*;
