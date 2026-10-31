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

// Every factory is emitted once from this op list: `Op<T>` is a match
// pattern when its operands are and a template when they are, so one
// constructor serves both sides and `template` re-exports it.

macro_rules! value_op_factories {
    ($bound:path, $verb:literal) => {
        bin!($bound, int_add, exact, NodeKind::IntBinaryOp(IntBinaryOp::Add),
            concat!($verb, " unsigned addition `lhs + rhs`. Commutative."));
        bin!($bound, int_mul, exact, NodeKind::IntBinaryOp(IntBinaryOp::Mul),
            concat!($verb, " wrapping multiplication `lhs * rhs`. Commutative."));
        bin!($bound, int_div, exact, NodeKind::IntBinaryOp(IntBinaryOp::Div),
            concat!($verb, " unsigned division `lhs / rhs`."));
        bin!($bound, int_sdiv, exact, NodeKind::IntBinaryOp(IntBinaryOp::Sdiv),
            concat!($verb, " signed division `(signed)lhs / (signed)rhs`."));
        bin!($bound, int_rem, exact, NodeKind::IntBinaryOp(IntBinaryOp::Rem),
            concat!($verb, " unsigned remainder `lhs % rhs`."));
        bin!($bound, int_srem, exact, NodeKind::IntBinaryOp(IntBinaryOp::Srem),
            concat!($verb, " signed remainder `(signed)lhs % (signed)rhs`."));
        bin!($bound, int_and, exact, NodeKind::IntBinaryOp(IntBinaryOp::And),
            concat!($verb, " bitwise AND `lhs & rhs`. Commutative."));
        bin!($bound, int_or, exact, NodeKind::IntBinaryOp(IntBinaryOp::Or),
            concat!($verb, " bitwise OR `lhs | rhs`. Commutative."));
        bin!($bound, int_xor, exact, NodeKind::IntBinaryOp(IntBinaryOp::Xor),
            concat!($verb, " bitwise XOR `lhs ^ rhs`. Commutative."));
        bin!($bound, int_shl, exact, NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft),
            concat!($verb, " logical left-shift `lhs << rhs`."));
        bin!($bound, int_shr, exact, NodeKind::IntBinaryOp(IntBinaryOp::ShiftRight),
            concat!($verb, " logical right-shift `lhs >> rhs`."));
        bin!($bound, int_sshr, exact, NodeKind::IntBinaryOp(IntBinaryOp::SShiftRight),
            concat!($verb, " arithmetic right-shift `(signed)lhs >> rhs`."));

        /// Subtraction `lhs - rhs` (the lifter's `Add(lhs, Neg(rhs))` shape).
        pub fn int_sub<L: $bound + 'static, R: $bound + 'static>(lhs: L, rhs: R) -> Op<(L, R)> {
            Op::new(Expr::int_sub(lhs.into_expr(), rhs.into_expr()))
        }

        un!($bound, int_neg, NodeKind::IntUnaryOp(IntUnaryOp::Neg),
            concat!($verb, " two's-complement negation `-inner`."));
        un!($bound, int_popcount, NodeKind::Popcount,
            concat!($verb, " a `Popcount(inner)` (count-set-bits) node."));
        un!($bound, int_lzcount, NodeKind::Lzcount,
            concat!($verb, " an `Lzcount(inner)` (leading-zero-count) node."));

        /// Bitwise complement `~inner` (the canonical `int_xor(_, all_ones)`).
        pub fn int_not<I: $bound + 'static>(inner: I) -> Op<(I,)> {
            let ones = Const::AllOnes.into();
            let xor = NodeKind::IntBinaryOp(IntBinaryOp::Xor);
            Op::new(Expr::exact(xor, [inner.into_expr(), ones]))
        }

        un!($bound, int_truncate, NodeKind::Truncate,
            concat!($verb, " a `Truncate(inner)` (integer narrowing) node."));
        un!($bound, int_zero_extend, NodeKind::Extend(ExtendOp::ZeroExtend),
            concat!($verb, " a zero-extension `Extend(ZeroExtend)(inner)` node."));
        un!($bound, int_sign_extend, NodeKind::Extend(ExtendOp::SignExtend),
            concat!($verb, " a sign-extension `Extend(SignExtend)(inner)` node."));
        un!($bound, int_to_float, NodeKind::IntToFloat,
            concat!($verb, " an `IntToFloat(inner)` value-conversion."));
        un!($bound, float_to_int, NodeKind::FloatToInt,
            concat!($verb, " a `FloatToInt(inner)` value-conversion."));
        un!($bound, int_bits_to_float, NodeKind::IntBitsToFloat,
            concat!($verb, " an `IntBitsToFloat(inner)` bitcast."));
        un!($bound, float_bits_to_int, NodeKind::FloatBitsToInt,
            concat!($verb, " a `FloatBitsToInt(inner)` bitcast."));
        un!($bound, float_to_float, NodeKind::FloatToFloat,
            concat!($verb, " a `FloatToFloat(inner)` precision-conversion."));

        /// An `Extend(op)` node with the given runtime `ExtendOp`.
        pub fn int_extend<I: $bound + 'static>(op: ExtendOp, inner: I) -> Op<(I,)> {
            Op::new(Expr::exact(NodeKind::Extend(op), [inner.into_expr()]))
        }

        bin!($bound, int_eq, exact_i1, NodeKind::IntCmpOp(IntCmpOp::Equal),
            concat!($verb, " an unsigned equality `lhs == rhs`. Commutative."));
        bin!($bound, int_lt, exact_i1, NodeKind::IntCmpOp(IntCmpOp::Less),
            concat!($verb, " an unsigned less-than `lhs < rhs`."));
        bin!($bound, int_slt, exact_i1, NodeKind::IntCmpOp(IntCmpOp::Sless),
            concat!($verb, " a signed less-than `(signed)lhs < (signed)rhs`."));
        bin!($bound, int_carry, exact_i1, NodeKind::IntCmpOp(IntCmpOp::Carry),
            concat!($verb, " an unsigned addition carry-out. Commutative."));
        bin!($bound, int_scarry, exact_i1, NodeKind::IntCmpOp(IntCmpOp::Scarry),
            concat!($verb, " a signed addition overflow. Commutative."));
        bin!($bound, int_sborrow, exact_i1, NodeKind::IntCmpOp(IntCmpOp::Sborrow),
            concat!($verb, " a signed subtraction borrow."));

        #[doc = concat!($verb, " an unsigned not-equal `lhs != rhs`, i.e. `int_xor(int_eq(l, r), 1):I1`; `.ordered()` pins the `int_eq`.")]
        pub fn int_ne<L: $bound + 'static, R: $bound + 'static>(lhs: L, rhs: R) -> Op<(L, R)> {
            let eq = NodeKind::IntCmpOp(IntCmpOp::Equal);
            let (l, r) = (lhs.into_expr(), rhs.into_expr());
            Op::new(Expr::negated(Expr::exact_i1(eq, [l, r])))
        }

        #[doc = concat!($verb, " an unsigned less-or-equal `lhs <= rhs`, i.e. `int_xor(int_lt(r, l), 1):I1`; `.ordered()` is a no-op, the `int_lt` orders its pair.")]
        pub fn int_le<L: $bound + 'static, R: $bound + 'static>(lhs: L, rhs: R) -> Op<(L, R)> {
            let lt = NodeKind::IntCmpOp(IntCmpOp::Less);
            let (l, r) = (lhs.into_expr(), rhs.into_expr());
            Op::new(Expr::negated(Expr::exact_i1(lt, [r, l])))
        }

        #[doc = concat!($verb, " a signed less-or-equal `(signed)lhs <= (signed)rhs`, i.e. `int_xor(int_slt(r, l), 1):I1`; `.ordered()` is a no-op, the `int_slt` orders its pair.")]
        pub fn int_sle<L: $bound + 'static, R: $bound + 'static>(lhs: L, rhs: R) -> Op<(L, R)> {
            let slt = NodeKind::IntCmpOp(IntCmpOp::Sless);
            let (l, r) = (lhs.into_expr(), rhs.into_expr());
            Op::new(Expr::negated(Expr::exact_i1(slt, [r, l])))
        }

        bin!($bound, float_add, exact, NodeKind::FloatBinaryOp(FloatBinaryOp::Add),
            concat!($verb, " a float addition `lhs + rhs`. Commutative."));
        bin!($bound, float_mul, exact, NodeKind::FloatBinaryOp(FloatBinaryOp::Mul),
            concat!($verb, " a float multiplication `lhs * rhs`. Commutative."));
        bin!($bound, float_div, exact, NodeKind::FloatBinaryOp(FloatBinaryOp::Div),
            concat!($verb, " a float division `lhs / rhs`."));

        /// Float subtraction `lhs - rhs` (the lifter's `Add(lhs, Neg(rhs))` shape).
        pub fn float_sub<L: $bound + 'static, R: $bound + 'static>(lhs: L, rhs: R) -> Op<(L, R)> {
            let neg = NodeKind::FloatUnaryOp(FloatUnaryOp::Neg);
            let add = NodeKind::FloatBinaryOp(FloatBinaryOp::Add);
            let r = Expr::exact(neg, [rhs.into_expr()]);
            Op::new(Expr::exact(add, [lhs.into_expr(), r]))
        }

        un!($bound, float_neg, NodeKind::FloatUnaryOp(FloatUnaryOp::Neg),
            concat!($verb, " a float negation `-x`."));
        un!($bound, float_abs, NodeKind::FloatUnaryOp(FloatUnaryOp::Abs),
            concat!($verb, " a float absolute value `|x|`."));
        un!($bound, float_sqrt, NodeKind::FloatUnaryOp(FloatUnaryOp::Sqrt),
            concat!($verb, " a float square root `√x`."));
        un!($bound, float_ceil, NodeKind::FloatUnaryOp(FloatUnaryOp::Ceil),
            concat!($verb, " a float ceiling `⌈x⌉`."));
        un!($bound, float_floor, NodeKind::FloatUnaryOp(FloatUnaryOp::Floor),
            concat!($verb, " a float floor `⌊x⌋`."));
        un!($bound, float_round, NodeKind::FloatUnaryOp(FloatUnaryOp::Round),
            concat!($verb, " a float round to nearest, ties away from zero."));

        #[doc = concat!($verb, " a float unary op `float_unary(op, x)` of kind `op` only.")]
        pub fn float_unary<I: $bound + 'static>(op: FloatUnaryOp, inner: I) -> Op<(I,)> {
            Op::new(Expr::exact(NodeKind::FloatUnaryOp(op), [inner.into_expr()]))
        }

        bin!($bound, float_eq, exact_i1, NodeKind::FloatCmpOp(FloatCmpOp::Equal),
            concat!($verb, " a float equality `lhs == rhs`. Commutative."));
        bin!($bound, float_lt, exact_i1, NodeKind::FloatCmpOp(FloatCmpOp::Less),
            concat!($verb, " a float less-than `lhs < rhs`."));

        #[doc = concat!($verb, " a float not-equal `lhs != rhs`, i.e. `int_xor(float_eq(l, r), 1):I1`; `.ordered()` pins the `float_eq`.")]
        pub fn float_ne<L: $bound + 'static, R: $bound + 'static>(lhs: L, rhs: R) -> Op<(L, R)> {
            let eq = NodeKind::FloatCmpOp(FloatCmpOp::Equal);
            let (l, r) = (lhs.into_expr(), rhs.into_expr());
            Op::new(Expr::negated(Expr::exact_i1(eq, [l, r])))
        }

        bin!($bound, bool_and, exact_i1, NodeKind::IntBinaryOp(IntBinaryOp::And),
            concat!($verb, " a boolean AND (`IntBinaryOp::And` at `I1`). Commutative."));
        bin!($bound, bool_or, exact_i1, NodeKind::IntBinaryOp(IntBinaryOp::Or),
            concat!($verb, " a boolean OR (`IntBinaryOp::Or` at `I1`). Commutative."));
        bin!($bound, bool_xor, exact_i1, NodeKind::IntBinaryOp(IntBinaryOp::Xor),
            concat!($verb, " a boolean XOR (`IntBinaryOp::Xor` at `I1`). Commutative."));

        /// Boolean NOT, i.e. `int_xor(operand, IntConst(1)):I1`.
        pub fn bool_not<I: $bound + 'static>(operand: I) -> Op<(I,)> {
            Op::new(Expr::bool_not(operand.into_expr()))
        }

        #[doc = concat!($verb, " an integer binary op `int_binary(op, l, r)` of kind `op` only.")]
        pub fn int_binary<L: $bound + 'static, R: $bound + 'static>(
            op: IntBinaryOp,
            lhs: L,
            rhs: R,
        ) -> Op<(L, R)> {
            let (l, r) = (lhs.into_expr(), rhs.into_expr());
            Op::new(Expr::exact(NodeKind::IntBinaryOp(op), [l, r]))
        }

        #[doc = concat!($verb, " an integer comparison `int_cmp(op, l, r)` of kind `op` only (output `I1`).")]
        pub fn int_cmp<L: $bound + 'static, R: $bound + 'static>(
            op: IntCmpOp,
            lhs: L,
            rhs: R,
        ) -> Op<(L, R)> {
            let (l, r) = (lhs.into_expr(), rhs.into_expr());
            Op::new(Expr::exact_i1(NodeKind::IntCmpOp(op), [l, r]))
        }

        #[doc = concat!($verb, " a float binary op `float_binary(op, l, r)` of kind `op` only.")]
        pub fn float_binary<L: $bound + 'static, R: $bound + 'static>(
            op: FloatBinaryOp,
            lhs: L,
            rhs: R,
        ) -> Op<(L, R)> {
            let (l, r) = (lhs.into_expr(), rhs.into_expr());
            Op::new(Expr::exact(NodeKind::FloatBinaryOp(op), [l, r]))
        }

        #[doc = concat!($verb, " a float comparison `float_cmp(op, l, r)` of kind `op` only (output `I1`).")]
        pub fn float_cmp<L: $bound + 'static, R: $bound + 'static>(
            op: FloatCmpOp,
            lhs: L,
            rhs: R,
        ) -> Op<(L, R)> {
            let (l, r) = (lhs.into_expr(), rhs.into_expr());
            Op::new(Expr::exact_i1(NodeKind::FloatCmpOp(op), [l, r]))
        }

        #[doc = concat!($verb, " a boolean binary op `bool_binary(op, l, r)` of kind `op` only (`IntBinaryOp` at `I1`).")]
        pub fn bool_binary<L: $bound + 'static, R: $bound + 'static>(
            op: IntBinaryOp,
            lhs: L,
            rhs: R,
        ) -> Op<(L, R)> {
            let (l, r) = (lhs.into_expr(), rhs.into_expr());
            Op::new(Expr::exact_i1(NodeKind::IntBinaryOp(op), [l, r]))
        }
    };
}

/// `$ctor` is [`Expr::exact`] or [`Expr::exact_i1`].
macro_rules! bin {
    ($bound:path, $name:ident, $ctor:ident, $kind:expr, $doc:expr) => {
        #[doc = $doc]
        pub fn $name<L: $bound + 'static, R: $bound + 'static>(lhs: L, rhs: R) -> Op<(L, R)> {
            let (l, r) = (lhs.into_expr(), rhs.into_expr());
            Op::new(Expr::$ctor($kind, [l, r]))
        }
    };
}

macro_rules! un {
    ($bound:path, $name:ident, $kind:expr, $doc:expr) => {
        #[doc = $doc]
        pub fn $name<I: $bound + 'static>(inner: I) -> Op<(I,)> {
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

    value_op_factories!(IntoExpr, "Match or build");
}

pub use ops::*;
