//! Binary / unary / cast / comparison builders; each matches or builds the named node.
//!
//! Lowered forms (`int_sub`, `int_not`, `int_ne` / `int_le`, `float_*`) build the lifter's
//! canonical shapes. Boolean ops pin the output to `I1`.

use std::marker::PhantomData;

use strider_ir::node::{NodeKind, ValueType};
use strider_ir::{FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp};

use crate::expr::{Expr, IntoExpr};
use crate::matcher::KindSpec;
use crate::matcher::match_pat::MatchPat;
use crate::template::template_pat::TemplatePat;

/// A pattern over operands `T`, valid on whichever sides (match / template) they all are.
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

/// A match-only pattern: not usable as a rewrite RHS.
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

/// `$ctor` is [`Expr::exact`] or [`Expr::exact_i1`]; with `op`, the op is a runtime `$variant`
/// argument.
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
    "Unsigned addition `lhs + rhs`. Commutative." }
    bin! { int_mul, exact, NodeKind::IntBinaryOp(IntBinaryOp::Mul),
    "Wrapping multiplication `lhs * rhs`. Commutative." }
    bin! { int_div, exact, NodeKind::IntBinaryOp(IntBinaryOp::Div),
    "Unsigned division `lhs / rhs`." }
    bin! { int_sdiv, exact, NodeKind::IntBinaryOp(IntBinaryOp::Sdiv),
    "Signed division `(signed)lhs / (signed)rhs`." }
    bin! { int_rem, exact, NodeKind::IntBinaryOp(IntBinaryOp::Rem),
    "Unsigned remainder `lhs % rhs`." }
    bin! { int_srem, exact, NodeKind::IntBinaryOp(IntBinaryOp::Srem),
    "Signed remainder `(signed)lhs % (signed)rhs`." }
    bin! { int_and, exact, NodeKind::IntBinaryOp(IntBinaryOp::And),
    "Bitwise AND `lhs & rhs`. Commutative." }
    bin! { int_or, exact, NodeKind::IntBinaryOp(IntBinaryOp::Or),
    "Bitwise OR `lhs | rhs`. Commutative." }
    bin! { int_xor, exact, NodeKind::IntBinaryOp(IntBinaryOp::Xor),
    "Bitwise XOR `lhs ^ rhs`. Commutative." }
    bin! { int_shl, exact, NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft),
    "Logical left-shift `lhs << rhs`." }
    bin! { int_shr, exact, NodeKind::IntBinaryOp(IntBinaryOp::ShiftRight),
    "Logical right-shift `lhs >> rhs`." }
    bin! { int_sshr, exact, NodeKind::IntBinaryOp(IntBinaryOp::SShiftRight),
    "Arithmetic right-shift `(signed)lhs >> rhs`." }

    /// Subtraction `lhs - rhs` (the lifter's `Add(lhs, Neg(rhs))` shape).
    pub fn int_sub<L: IntoExpr + 'static, R: IntoExpr + 'static>(lhs: L, rhs: R) -> Op<(L, R)> {
        Op::new(Expr::int_sub(lhs.into_expr(), rhs.into_expr()))
    }

    un! { int_neg, NodeKind::IntUnaryOp(IntUnaryOp::Neg),
    "Two's-complement negation `-inner`." }
    un! { int_popcount, NodeKind::Popcount,
    "A `Popcount(inner)` (count-set-bits) node." }
    un! { int_lzcount, NodeKind::Lzcount,
    "An `Lzcount(inner)` (leading-zero-count) node." }

    /// Bitwise complement `~inner` (the canonical `int_xor(_, all_ones)`).
    pub fn int_not<I: IntoExpr + 'static>(inner: I) -> Op<(I,)> {
        let ones = Const::AllOnes.into();
        let xor = NodeKind::IntBinaryOp(IntBinaryOp::Xor);
        Op::new(Expr::exact(xor, [inner.into_expr(), ones]))
    }

    un! { int_truncate, NodeKind::Truncate,
    "A `Truncate(inner)` (integer narrowing) node." }
    un! { int_zero_extend, NodeKind::Extend(ExtendOp::ZeroExtend),
    "Zero-extension `Extend(ZeroExtend)(inner)` node." }
    un! { int_sign_extend, NodeKind::Extend(ExtendOp::SignExtend),
    "Sign-extension `Extend(SignExtend)(inner)` node." }
    un! { int_to_float, NodeKind::IntToFloat,
    "An `IntToFloat(inner)` value-conversion." }
    un! { float_to_int, NodeKind::FloatToInt,
    "A `FloatToInt(inner)` value-conversion." }
    un! { int_bits_to_float, NodeKind::IntBitsToFloat,
    "An `IntBitsToFloat(inner)` bitcast." }
    un! { float_bits_to_int, NodeKind::FloatBitsToInt,
    "A `FloatBitsToInt(inner)` bitcast." }
    un! { float_to_float, NodeKind::FloatToFloat,
    "A `FloatToFloat(inner)` precision-conversion." }

    /// An `Extend(op)` node with the given runtime `ExtendOp`.
    pub fn int_extend<I: IntoExpr + 'static>(op: ExtendOp, inner: I) -> Op<(I,)> {
        Op::new(Expr::exact(NodeKind::Extend(op), [inner.into_expr()]))
    }

    bin! { int_eq, exact_i1, NodeKind::IntCmpOp(IntCmpOp::Equal),
    "Unsigned equality `lhs == rhs`. Commutative." }
    bin! { int_lt, exact_i1, NodeKind::IntCmpOp(IntCmpOp::Less),
    "Unsigned less-than `lhs < rhs`." }
    bin! { int_slt, exact_i1, NodeKind::IntCmpOp(IntCmpOp::Sless),
    "Signed less-than `(signed)lhs < (signed)rhs`." }
    bin! { int_carry, exact_i1, NodeKind::IntCmpOp(IntCmpOp::Carry),
    "Unsigned addition carry-out. Commutative." }
    bin! { int_scarry, exact_i1, NodeKind::IntCmpOp(IntCmpOp::Scarry),
    "Signed addition overflow. Commutative." }
    bin! { int_sborrow, exact_i1, NodeKind::IntCmpOp(IntCmpOp::Sborrow),
    "Signed subtraction borrow." }

    /// Unsigned `lhs != rhs`: `int_xor(int_eq(l, r), 1):I1`; `.ordered()` pins the `int_eq`.
    pub fn int_ne<L: IntoExpr + 'static, R: IntoExpr + 'static>(lhs: L, rhs: R) -> Op<(L, R)> {
        let eq = NodeKind::IntCmpOp(IntCmpOp::Equal);
        let (l, r) = (lhs.into_expr(), rhs.into_expr());
        Op::new(Expr::negated(Expr::exact_i1(eq, [l, r])))
    }

    /// Unsigned `lhs <= rhs`: `int_xor(int_lt(r, l), 1):I1`.
    pub fn int_le<L: IntoExpr + 'static, R: IntoExpr + 'static>(lhs: L, rhs: R) -> Op<(L, R)> {
        let lt = NodeKind::IntCmpOp(IntCmpOp::Less);
        let (l, r) = (lhs.into_expr(), rhs.into_expr());
        Op::new(Expr::negated(Expr::exact_i1(lt, [r, l])))
    }

    /// Signed `lhs <= rhs`: `int_xor(int_slt(r, l), 1):I1`.
    pub fn int_sle<L: IntoExpr + 'static, R: IntoExpr + 'static>(lhs: L, rhs: R) -> Op<(L, R)> {
        let slt = NodeKind::IntCmpOp(IntCmpOp::Sless);
        let (l, r) = (lhs.into_expr(), rhs.into_expr());
        Op::new(Expr::negated(Expr::exact_i1(slt, [r, l])))
    }

    bin! { float_add, exact, NodeKind::FloatBinaryOp(FloatBinaryOp::Add),
    "Float addition `lhs + rhs`. Commutative." }
    bin! { float_mul, exact, NodeKind::FloatBinaryOp(FloatBinaryOp::Mul),
    "Float multiplication `lhs * rhs`. Commutative." }
    bin! { float_div, exact, NodeKind::FloatBinaryOp(FloatBinaryOp::Div),
    "Float division `lhs / rhs`." }

    /// Float subtraction `lhs - rhs` (the lifter's `Add(lhs, Neg(rhs))` shape).
    pub fn float_sub<L: IntoExpr + 'static, R: IntoExpr + 'static>(lhs: L, rhs: R) -> Op<(L, R)> {
        let neg = NodeKind::FloatUnaryOp(FloatUnaryOp::Neg);
        let add = NodeKind::FloatBinaryOp(FloatBinaryOp::Add);
        let r = Expr::exact(neg, [rhs.into_expr()]);
        Op::new(Expr::exact(add, [lhs.into_expr(), r]))
    }

    un! { float_neg, NodeKind::FloatUnaryOp(FloatUnaryOp::Neg),
    "Float negation `-x`." }
    un! { float_abs, NodeKind::FloatUnaryOp(FloatUnaryOp::Abs),
    "Float absolute value `|x|`." }
    un! { float_sqrt, NodeKind::FloatUnaryOp(FloatUnaryOp::Sqrt),
    "Float square root `√x`." }
    un! { float_ceil, NodeKind::FloatUnaryOp(FloatUnaryOp::Ceil),
    "Float ceiling `⌈x⌉`." }
    un! { float_floor, NodeKind::FloatUnaryOp(FloatUnaryOp::Floor),
    "Float floor `⌊x⌋`." }
    un! { float_round, NodeKind::FloatUnaryOp(FloatUnaryOp::Round),
    "Float round to nearest, ties away from zero." }

    /// A float unary op of kind `op`.
    pub fn float_unary<I: IntoExpr + 'static>(op: FloatUnaryOp, inner: I) -> Op<(I,)> {
        Op::new(Expr::exact(NodeKind::FloatUnaryOp(op), [inner.into_expr()]))
    }

    bin! { float_eq, exact_i1, NodeKind::FloatCmpOp(FloatCmpOp::Equal),
    "Float equality `lhs == rhs`. Commutative." }
    bin! { float_lt, exact_i1, NodeKind::FloatCmpOp(FloatCmpOp::Less),
    "Float less-than `lhs < rhs`." }

    /// Float `lhs != rhs`: `int_xor(float_eq(l, r), 1):I1`; `.ordered()` pins the `float_eq`.
    pub fn float_ne<L: IntoExpr + 'static, R: IntoExpr + 'static>(lhs: L, rhs: R) -> Op<(L, R)> {
        let eq = NodeKind::FloatCmpOp(FloatCmpOp::Equal);
        let (l, r) = (lhs.into_expr(), rhs.into_expr());
        Op::new(Expr::negated(Expr::exact_i1(eq, [l, r])))
    }

    bin! { bool_and, exact_i1, NodeKind::IntBinaryOp(IntBinaryOp::And),
    "Boolean AND (`IntBinaryOp::And` at `I1`). Commutative." }
    bin! { bool_or, exact_i1, NodeKind::IntBinaryOp(IntBinaryOp::Or),
    "Boolean OR (`IntBinaryOp::Or` at `I1`). Commutative." }
    bin! { bool_xor, exact_i1, NodeKind::IntBinaryOp(IntBinaryOp::Xor),
    "Boolean XOR (`IntBinaryOp::Xor` at `I1`). Commutative." }

    /// Boolean NOT, i.e. `int_xor(operand, IntConst(1)):I1`.
    pub fn bool_not<I: IntoExpr + 'static>(operand: I) -> Op<(I,)> {
        Op::new(Expr::bool_not(operand.into_expr()))
    }

    bin! { int_binary, exact, op IntBinaryOp,
    "Integer binary op of kind `op`." }
    bin! { int_cmp, exact_i1, op IntCmpOp,
    "Integer comparison of kind `op` (output `I1`)." }
    bin! { float_binary, exact, op FloatBinaryOp,
    "Float binary op of kind `op`." }
    bin! { float_cmp, exact_i1, op FloatCmpOp,
    "Float comparison of kind `op` (output `I1`)." }
    bin! { bool_binary, exact_i1, op IntBinaryOp,
    "Boolean binary op of kind `op` (`IntBinaryOp` at `I1`)." }
}

pub use ops::*;
