//! Binary / unary / cast / comparison typed builders.
//!
//! Lift-time canonicalisations (`int_sub` to `int_add(_, int_neg(_))`,
//! `int_not` to `int_xor(_, all_ones)`, the lowered `int_ne` / `int_le` /
//! `float_*` shapes) are
//! reproduced here so a typed pattern matches the canonical IR the lifter
//! emits. Boolean ops pin the output to `I1`. Commutativity is data-driven via
//! `NodeKind::is_commutative`.

use strider_ir::node::{NodeKind, ValueType};
use strider_ir::{
    ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp,
};

use crate::matcher::match_pat::{MatchPat, Pre};
use crate::matcher::{KindSpec, MatcherBuilder, PatValueRef};
use crate::template::template_pat::TemplatePat;
use crate::template::{TemplateBuilder, TmplValueRef};
use crate::typed::builder_like::{
    compile_bool_binary, compile_int_binary, compile_two_input, compile_unary_kind,
};
use crate::typed::consts::{int_const, int_const_with_fn};

// The `*Fixed` structs and their lowered-shape siblings (`Sub`, `BitNot`,
// `FloatSub`, `FloatLe`, ...) stay hand-written: their lowerings differ.

/// Emit a match-only `*Any` binary struct matching any variant of
/// `$exemplar`'s kind. Pass a trailing `pin_i1` to pin the output to `I1`
/// (booleans / comparisons); omit it for value outputs.
macro_rules! variant_binary_any {
    ($(#[$smeta:meta])* $struct:ident, $fn:ident, $exemplar:expr, $fndoc:literal $(, $pin_i1:ident)?) => {
        $(#[$smeta])*
        pub struct $struct<L, R> {
            lhs: L,
            rhs: R,
        }

        impl<L: MatchPat, R: MatchPat> MatchPat for $struct<L, R> {
            fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
                let exemplar = $exemplar;
                #[allow(unused_mut, unused_assignments)]
                let mut pin: Option<ValueType> = None;
                $(
                    let _ = stringify!($pin_i1);
                    pin = Some(ValueType::I1);
                )?
                compile_two_input(b, KindSpec::variant_of(&exemplar), self.lhs, self.rhs, pin)
            }
        }

        #[doc = $fndoc]
        pub fn $fn<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> $struct<L, R> {
            $struct { lhs, rhs }
        }
    };
}

/// Unary counterpart of [`variant_binary_any`]; never needs an output pin.
macro_rules! variant_unary_any {
    ($(#[$smeta:meta])* $struct:ident, $fn:ident, $exemplar:expr, $fndoc:literal) => {
        $(#[$smeta])*
        pub struct $struct<I> {
            inner: I,
        }

        impl<I: MatchPat> MatchPat for $struct<I> {
            fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
                let exemplar = $exemplar;
                compile_unary_kind(b, KindSpec::variant_of(&exemplar), self.inner)
            }
        }

        #[doc = $fndoc]
        pub fn $fn<I: MatchPat>(inner: I) -> $struct<I> {
            $struct { inner }
        }
    };
}

/// The `MatchPat` and `TemplatePat` impls for a builder whose lowering is the
/// same expression on both sides, which is what the generic `compile_*`
/// helpers exist to make true.
///
/// `BitNot` is deliberately NOT expressed here: its match side pins
/// `int_const(u128::MAX)` while its build side derives all-ones from the
/// root's width, so the two are genuinely different lowerings.
macro_rules! dual_pat {
    ($ty:ident<$($g:ident),+ $(,)?>, |$me:ident, $b:ident| $body:block) => {
        impl<$($g: MatchPat),+> MatchPat for $ty<$($g),+> {
            fn compile($me, $b: &mut MatcherBuilder) -> PatValueRef $body
        }

        impl<$($g: TemplatePat),+> TemplatePat for $ty<$($g),+> {
            fn compile($me, $b: &mut TemplateBuilder) -> TmplValueRef $body
        }
    };
}

pub struct IntBinaryFixed<L, R> {
    op: IntBinaryOp,
    lhs: L,
    rhs: R,
}

dual_pat!(IntBinaryFixed<L, R>, |self, b| {
    compile_int_binary(b, self.op, self.lhs, self.rhs)
});

variant_binary_any!(
    /// Match-only.
    AnyIntBinary,
    any_int_binary,
    NodeKind::IntBinaryOp(IntBinaryOp::Add),
    "Match any `IntBinaryOp` regardless of variant."
);

/// Lowered to `int_add(lhs, int_neg(rhs))`.
pub struct Sub<L, R> {
    lhs: L,
    rhs: R,
}

dual_pat!(Sub<L, R>, |self, b| {
    compile_sub(b, self.lhs, self.rhs)
});

fn compile_sub<B, L, R>(b: &mut B, lhs: L, rhs: R) -> B::OutRef
where
    B: crate::typed::builder_like::BuilderLike,
    L: crate::typed::builder_like::CompileInto<B>,
    R: crate::typed::builder_like::CompileInto<B>,
    IntUnaryFixed<R>: crate::typed::builder_like::CompileInto<B>,
{
    let neg_rhs = IntUnaryFixed {
        kind: NodeKind::IntUnaryOp(IntUnaryOp::Neg),
        inner: rhs,
    };
    compile_int_binary(b, IntBinaryOp::Add, lhs, neg_rhs)
}

/// `Neg`, `Popcount`, or `Lzcount`.
pub struct IntUnaryFixed<I> {
    kind: NodeKind,
    inner: I,
}

dual_pat!(IntUnaryFixed<I>, |self, b| {
    compile_unary_kind(b, KindSpec::Exact(self.kind), self.inner)
});

variant_unary_any!(
    /// Match-only.
    AnyIntUnary,
    any_int_unary,
    NodeKind::IntUnaryOp(IntUnaryOp::Neg),
    "Match any `IntUnaryOp` regardless of variant."
);

/// Bitwise complement, i.e. `int_xor(inner, all_ones)`.
pub struct BitNot<I> {
    inner: I,
}

impl<I: MatchPat> MatchPat for BitNot<I> {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        // `int_const`'s match is width-masked, so `u128::MAX` matches
        // all-ones at any output width.
        int_xor(self.inner, int_const(u128::MAX)).compile(b)
    }
}

impl<I: TemplatePat> TemplatePat for BitNot<I> {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        // Unlike the match side, `create_node` stores an `IntConst` verbatim
        // without masking to the output width, so a plain `int_const(u128::MAX)`
        // would materialise a raw `u128::MAX`. Derive all-ones from the
        // rewrite root's resolved width instead.
        let i = self.inner.compile(b);
        let ones_out = int_const_with_fn(|ctx| Ok(ctx.root_ty.bit_mask_u128())).compile(b);
        b.binary(IntBinaryOp::Xor, i, ones_out)
    }
}

/// A unary-shape cast wrapping `inner` with an exact `NodeKind`.
pub struct Cast<I> {
    kind: NodeKind,
    inner: I,
}

dual_pat!(Cast<I>, |self, b| {
    compile_unary_kind(b, KindSpec::Exact(self.kind), self.inner)
});

/// Output `I1`.
pub struct IntCmpFixed<L, R> {
    op: IntCmpOp,
    lhs: L,
    rhs: R,
}

dual_pat!(IntCmpFixed<L, R>, |self, b| {
    let kind = KindSpec::Exact(NodeKind::IntCmpOp(self.op));
    compile_two_input(b, kind, self.lhs, self.rhs, Some(ValueType::I1))
});

variant_binary_any!(
    /// Output `I1`. Match-only.
    AnyIntCmp,
    any_int_cmp,
    NodeKind::IntCmpOp(IntCmpOp::Equal),
    "Match any `IntCmpOp` regardless of variant.",
    pin_i1
);

pub struct FloatBinaryFixed<L, R> {
    op: FloatBinaryOp,
    lhs: L,
    rhs: R,
}

dual_pat!(FloatBinaryFixed<L, R>, |self, b| {
    let kind = KindSpec::Exact(NodeKind::FloatBinaryOp(self.op));
    compile_two_input(b, kind, self.lhs, self.rhs, None)
});

variant_binary_any!(
    /// Match-only.
    AnyFloatBinary,
    any_float_binary,
    NodeKind::FloatBinaryOp(FloatBinaryOp::Add),
    "Match any `FloatBinaryOp` regardless of variant."
);

/// Lowered to `float_add(lhs, float_neg(rhs))`.
pub struct FloatSub<L, R> {
    lhs: L,
    rhs: R,
}

dual_pat!(FloatSub<L, R>, |self, b| {
    compile_float_sub(b, self.lhs, self.rhs)
});

fn compile_float_sub<B, L, R>(b: &mut B, lhs: L, rhs: R) -> B::OutRef
where
    B: crate::typed::builder_like::BuilderLike,
    L: crate::typed::builder_like::CompileInto<B>,
    R: crate::typed::builder_like::CompileInto<B>,
    FloatUnaryFixed<R>: crate::typed::builder_like::CompileInto<B>,
{
    let neg_rhs = FloatUnaryFixed {
        op: FloatUnaryOp::Neg,
        inner: rhs,
    };
    let kind = KindSpec::Exact(NodeKind::FloatBinaryOp(FloatBinaryOp::Add));
    compile_two_input(b, kind, lhs, neg_rhs, None)
}

pub struct FloatUnaryFixed<I> {
    op: FloatUnaryOp,
    inner: I,
}

dual_pat!(FloatUnaryFixed<I>, |self, b| {
    let kind = KindSpec::Exact(NodeKind::FloatUnaryOp(self.op));
    compile_unary_kind(b, kind, self.inner)
});

variant_unary_any!(
    /// Match-only.
    AnyFloatUnary,
    any_float_unary,
    NodeKind::FloatUnaryOp(FloatUnaryOp::Neg),
    "Match any `FloatUnaryOp` regardless of variant."
);

/// Output `I1`.
pub struct FloatCmpFixed<L, R> {
    op: FloatCmpOp,
    lhs: L,
    rhs: R,
}

dual_pat!(FloatCmpFixed<L, R>, |self, b| {
    let kind = KindSpec::Exact(NodeKind::FloatCmpOp(self.op));
    compile_two_input(b, kind, self.lhs, self.rhs, Some(ValueType::I1))
});

variant_binary_any!(
    /// Output `I1`. Match-only.
    AnyFloatCmp,
    any_float_cmp,
    NodeKind::FloatCmpOp(FloatCmpOp::Equal),
    "Match any `FloatCmpOp` regardless of variant.",
    pin_i1
);

/// NaN-aware `bool_or(float_lt(l, r), float_eq(l, r))` at `I1`.
pub struct FloatLe<L, R> {
    lhs: L,
    rhs: R,
}

impl<L: MatchPat, R: MatchPat> MatchPat for FloatLe<L, R> {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        // Each operand feeds both cmp branches, and a move-by-value operand
        // can't be consumed twice; compile once and fan out via `Pre`. The
        // pin is what makes the two branches agree on the operand.
        let l = self.lhs.compile(b);
        let r = self.rhs.compile(b);
        b.pin_shared_identity(l);
        b.pin_shared_identity(r);
        let less = FloatCmpFixed {
            op: FloatCmpOp::Less,
            lhs: Pre(l),
            rhs: Pre(r),
        }
        .compile(b);
        let equal = FloatCmpFixed {
            op: FloatCmpOp::Equal,
            lhs: Pre(l),
            rhs: Pre(r),
        }
        .compile(b);
        compile_bool_binary(b, IntBinaryOp::Or, Pre(less), Pre(equal))
    }
}

pub fn float_le<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> FloatLe<L, R> {
    FloatLe { lhs, rhs }
}

/// `int_xor(float_eq(x, x), 1):I1`.
pub struct FloatIsNan<I> {
    inner: I,
}

impl<I: MatchPat> MatchPat for FloatIsNan<I> {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        // `x` feeds both equality inputs; compile once, fan out via `Pre`, and
        // pin the identity the fan-out alone does not enforce.
        let x = self.inner.compile(b);
        b.pin_shared_identity(x);
        let eq = FloatCmpFixed {
            op: FloatCmpOp::Equal,
            lhs: Pre(x),
            rhs: Pre(x),
        }
        .compile(b);
        compile_bool_binary(
            b,
            IntBinaryOp::Xor,
            Pre(eq),
            crate::typed::consts::bool_const(true),
        )
    }
}

pub fn float_is_nan<I: MatchPat>(inner: I) -> FloatIsNan<I> {
    FloatIsNan { inner }
}

/// Output `I1`.
pub struct BoolBinaryFixed<L, R> {
    op: IntBinaryOp,
    lhs: L,
    rhs: R,
}

dual_pat!(BoolBinaryFixed<L, R>, |self, b| {
    compile_bool_binary(b, self.op, self.lhs, self.rhs)
});

variant_binary_any!(
    /// Any `IntBinaryOp` at `I1`. Match-only.
    AnyBoolBinary,
    any_bool_binary,
    NodeKind::IntBinaryOp(IntBinaryOp::And),
    "Match any `IntBinaryOp` at `I1` regardless of variant.",
    pin_i1
);

/// `int_xor(operand, 1):I1`.
pub struct BoolNot<I> {
    inner: I,
}

dual_pat!(BoolNot<I>, |self, b| { compile_bool_not(b, self.inner) });

fn compile_bool_not<B, I>(b: &mut B, inner: I) -> B::OutRef
where
    B: crate::typed::builder_like::BuilderLike,
    I: crate::typed::builder_like::CompileInto<B>,
    crate::typed::consts::BoolConst: crate::typed::builder_like::CompileInto<B>,
{
    compile_bool_binary(
        b,
        IntBinaryOp::Xor,
        inner,
        crate::typed::consts::bool_const(true),
    )
}

// Every factory free function is emitted from this one op list, once at crate
// scope under `MatchPat` and once inside `mod template` under `TemplatePat`.
// Both sides build the identical structs; only the constructor's trait bound
// differs, which is what puts the match/template boundary at construction.
// A `mod` can't be re-opened by per-op macro invocations, so the whole list
// lives here and the macro is invoked once per scope.

macro_rules! value_op_factories {
    (
        bound = $bound:path,
        verb = $verb:literal,
        // The xor's `1` for the lowered `int_ne` / `int_le` / ... shapes,
        // passed in so it carries this scope's trait bound.
        ne_one = $ne_one:expr,
    ) => {
        binary_factory!($bound, IntBinaryFixed, IntBinaryOp, Add, int_add,
            concat!($verb, " unsigned addition `lhs + rhs`. Commutative."));
        binary_factory!($bound, IntBinaryFixed, IntBinaryOp, Mul, int_mul,
            concat!($verb, " wrapping multiplication `lhs * rhs`. Commutative."));
        binary_factory!($bound, IntBinaryFixed, IntBinaryOp, Div, int_div,
            concat!($verb, " unsigned division `lhs / rhs`."));
        binary_factory!($bound, IntBinaryFixed, IntBinaryOp, Sdiv, int_sdiv,
            concat!($verb, " signed division `(signed)lhs / (signed)rhs`."));
        binary_factory!($bound, IntBinaryFixed, IntBinaryOp, Rem, int_rem,
            concat!($verb, " unsigned remainder `lhs % rhs`."));
        binary_factory!($bound, IntBinaryFixed, IntBinaryOp, Srem, int_srem,
            concat!($verb, " signed remainder `(signed)lhs % (signed)rhs`."));
        binary_factory!($bound, IntBinaryFixed, IntBinaryOp, And, int_and,
            concat!($verb, " bitwise AND `lhs & rhs`. Commutative."));
        binary_factory!($bound, IntBinaryFixed, IntBinaryOp, Or, int_or,
            concat!($verb, " bitwise OR `lhs | rhs`. Commutative."));
        binary_factory!($bound, IntBinaryFixed, IntBinaryOp, Xor, int_xor,
            concat!($verb, " bitwise XOR `lhs ^ rhs`. Commutative."));
        binary_factory!($bound, IntBinaryFixed, IntBinaryOp, ShiftLeft, int_shl,
            concat!($verb, " logical left-shift `lhs << rhs`."));
        binary_factory!($bound, IntBinaryFixed, IntBinaryOp, ShiftRight, int_shr,
            concat!($verb, " logical right-shift `lhs >> rhs`."));
        binary_factory!($bound, IntBinaryFixed, IntBinaryOp, SShiftRight, int_sshr,
            concat!($verb, " arithmetic right-shift `(signed)lhs >> rhs`."));

        /// Subtraction `lhs - rhs` (the lifter's `Add(lhs, Neg(rhs))` shape).
        pub fn int_sub<L: $bound, R: $bound>(lhs: L, rhs: R) -> Sub<L, R> {
            Sub { lhs, rhs }
        }

        unary_factory!($bound, IntUnaryFixed, kind, NodeKind::IntUnaryOp(IntUnaryOp::Neg), int_neg,
            concat!($verb, " two's-complement negation `-inner`."));
        unary_factory!($bound, IntUnaryFixed, kind, NodeKind::Popcount, int_popcount,
            concat!($verb, " a `Popcount(inner)` (count-set-bits) node."));
        unary_factory!($bound, IntUnaryFixed, kind, NodeKind::Lzcount, int_lzcount,
            concat!($verb, " an `Lzcount(inner)` (leading-zero-count) node."));

        /// Bitwise complement `~inner` (the canonical `int_xor(_, all_ones)`).
        pub fn int_not<I: $bound>(inner: I) -> BitNot<I> {
            BitNot { inner }
        }

        unary_factory!($bound, Cast, kind, NodeKind::Truncate, int_truncate,
            concat!($verb, " a `Truncate(inner)` (integer narrowing) node."));
        unary_factory!($bound, Cast, kind, NodeKind::Extend(ExtendOp::ZeroExtend), int_zero_extend,
            concat!($verb, " a zero-extension `Extend(ZeroExtend)(inner)` node."));
        unary_factory!($bound, Cast, kind, NodeKind::Extend(ExtendOp::SignExtend), int_sign_extend,
            concat!($verb, " a sign-extension `Extend(SignExtend)(inner)` node."));
        unary_factory!($bound, Cast, kind, NodeKind::IntToFloat, int_to_float,
            concat!($verb, " an `IntToFloat(inner)` value-conversion."));
        unary_factory!($bound, Cast, kind, NodeKind::FloatToInt, float_to_int,
            concat!($verb, " a `FloatToInt(inner)` value-conversion."));
        unary_factory!($bound, Cast, kind, NodeKind::IntBitsToFloat, int_bits_to_float,
            concat!($verb, " an `IntBitsToFloat(inner)` bitcast."));
        unary_factory!($bound, Cast, kind, NodeKind::FloatBitsToInt, float_bits_to_int,
            concat!($verb, " a `FloatBitsToInt(inner)` bitcast."));
        unary_factory!($bound, Cast, kind, NodeKind::FloatToFloat, float_to_float,
            concat!($verb, " a `FloatToFloat(inner)` precision-conversion."));

        /// An `Extend(op)` node with the given runtime `ExtendOp`.
        pub fn int_extend<I: $bound>(op: ExtendOp, inner: I) -> Cast<I> {
            Cast { kind: NodeKind::Extend(op), inner }
        }

        binary_factory!($bound, IntCmpFixed, IntCmpOp, Equal, int_eq,
            concat!($verb, " an unsigned equality `lhs == rhs`. Commutative."));
        binary_factory!($bound, IntCmpFixed, IntCmpOp, Less, int_lt,
            concat!($verb, " an unsigned less-than `lhs < rhs`."));
        binary_factory!($bound, IntCmpFixed, IntCmpOp, Sless, int_slt,
            concat!($verb, " a signed less-than `(signed)lhs < (signed)rhs`."));
        binary_factory!($bound, IntCmpFixed, IntCmpOp, Carry, int_carry,
            concat!($verb, " an unsigned addition carry-out. Commutative."));
        binary_factory!($bound, IntCmpFixed, IntCmpOp, Scarry, int_scarry,
            concat!($verb, " a signed addition overflow. Commutative."));
        binary_factory!($bound, IntCmpFixed, IntCmpOp, Sborrow, int_sborrow,
            concat!($verb, " a signed subtraction borrow."));

        #[doc = concat!($verb, " an unsigned not-equal `lhs != rhs`, i.e. `int_xor(int_eq(l, r), 1):I1`.")]
        pub fn int_ne<L: $bound, R: $bound>(lhs: L, rhs: R) -> impl $bound {
            int_xor(int_eq(lhs, rhs), $ne_one)
        }

        #[doc = concat!($verb, " an unsigned less-or-equal `lhs <= rhs`, i.e. `int_xor(int_lt(r, l), 1):I1`.")]
        pub fn int_le<L: $bound, R: $bound>(lhs: L, rhs: R) -> impl $bound {
            int_xor(int_lt(rhs, lhs), $ne_one)
        }

        #[doc = concat!($verb, " a signed less-or-equal `(signed)lhs <= (signed)rhs`, i.e. `int_xor(int_slt(r, l), 1):I1`.")]
        pub fn int_sle<L: $bound, R: $bound>(lhs: L, rhs: R) -> impl $bound {
            int_xor(int_slt(rhs, lhs), $ne_one)
        }

        binary_factory!($bound, FloatBinaryFixed, FloatBinaryOp, Add, float_add,
            concat!($verb, " a float addition `lhs + rhs`. Commutative."));
        binary_factory!($bound, FloatBinaryFixed, FloatBinaryOp, Mul, float_mul,
            concat!($verb, " a float multiplication `lhs * rhs`. Commutative."));
        binary_factory!($bound, FloatBinaryFixed, FloatBinaryOp, Div, float_div,
            concat!($verb, " a float division `lhs / rhs`."));

        /// Float subtraction `lhs - rhs` (the lifter's `Add(lhs, Neg(rhs))` shape).
        pub fn float_sub<L: $bound, R: $bound>(lhs: L, rhs: R) -> FloatSub<L, R> {
            FloatSub { lhs, rhs }
        }

        float_unary_factory!($bound, Neg, float_neg, concat!($verb, " a float negation `-x`."));
        float_unary_factory!($bound, Abs, float_abs, concat!($verb, " a float absolute value `|x|`."));
        float_unary_factory!($bound, Sqrt, float_sqrt, concat!($verb, " a float square root `√x`."));
        float_unary_factory!($bound, Ceil, float_ceil, concat!($verb, " a float ceiling `⌈x⌉`."));
        float_unary_factory!($bound, Floor, float_floor, concat!($verb, " a float floor `⌊x⌋`."));
        float_unary_factory!($bound, Round, float_round, concat!($verb, " a float round-to-nearest-even."));

        binary_factory!($bound, FloatCmpFixed, FloatCmpOp, Equal, float_eq,
            concat!($verb, " a float equality `lhs == rhs`. Commutative."));
        binary_factory!($bound, FloatCmpFixed, FloatCmpOp, Less, float_lt,
            concat!($verb, " a float less-than `lhs < rhs`."));

        #[doc = concat!($verb, " a float not-equal `lhs != rhs`, i.e. `int_xor(float_eq(l, r), 1):I1`.")]
        pub fn float_ne<L: $bound, R: $bound>(lhs: L, rhs: R) -> impl $bound {
            int_xor(float_eq(lhs, rhs), $ne_one)
        }

        binary_factory!($bound, BoolBinaryFixed, IntBinaryOp, And, bool_and,
            concat!($verb, " a boolean AND (`IntBinaryOp::And` at `I1`). Commutative."));
        binary_factory!($bound, BoolBinaryFixed, IntBinaryOp, Or, bool_or,
            concat!($verb, " a boolean OR (`IntBinaryOp::Or` at `I1`). Commutative."));
        binary_factory!($bound, BoolBinaryFixed, IntBinaryOp, Xor, bool_xor,
            concat!($verb, " a boolean XOR (`IntBinaryOp::Xor` at `I1`). Commutative."));

        /// Boolean NOT, i.e. `int_xor(operand, IntConst(1)):I1`.
        pub fn bool_not<I: $bound>(operand: I) -> BoolNot<I> {
            BoolNot { inner: operand }
        }

        #[doc = concat!($verb, " a variant-agnostic integer binary op `int_binary(op, l, r)`.")]
        pub fn int_binary<L: $bound, R: $bound>(
            op: IntBinaryOp,
            lhs: L,
            rhs: R,
        ) -> IntBinaryFixed<L, R> {
            IntBinaryFixed { op, lhs, rhs }
        }

        #[doc = concat!($verb, " a variant-agnostic integer comparison `int_cmp(op, l, r)` (output `I1`).")]
        pub fn int_cmp<L: $bound, R: $bound>(
            op: IntCmpOp,
            lhs: L,
            rhs: R,
        ) -> IntCmpFixed<L, R> {
            IntCmpFixed { op, lhs, rhs }
        }

        #[doc = concat!($verb, " a variant-agnostic float binary op `float_binary(op, l, r)`.")]
        pub fn float_binary<L: $bound, R: $bound>(
            op: FloatBinaryOp,
            lhs: L,
            rhs: R,
        ) -> FloatBinaryFixed<L, R> {
            FloatBinaryFixed { op, lhs, rhs }
        }

        #[doc = concat!($verb, " a variant-agnostic float comparison `float_cmp(op, l, r)` (output `I1`).")]
        pub fn float_cmp<L: $bound, R: $bound>(
            op: FloatCmpOp,
            lhs: L,
            rhs: R,
        ) -> FloatCmpFixed<L, R> {
            FloatCmpFixed { op, lhs, rhs }
        }

        #[doc = concat!($verb, " a variant-agnostic boolean binary op `bool_binary(op, l, r)` (`IntBinaryOp` at `I1`).")]
        pub fn bool_binary<L: $bound, R: $bound>(
            op: IntBinaryOp,
            lhs: L,
            rhs: R,
        ) -> BoolBinaryFixed<L, R> {
            BoolBinaryFixed { op, lhs, rhs }
        }
    };
}

macro_rules! binary_factory {
    ($bound:path, $struct:ident, $op:ty, $variant:ident, $name:ident, $doc:expr) => {
        #[doc = $doc]
        pub fn $name<L: $bound, R: $bound>(lhs: L, rhs: R) -> $struct<L, R> {
            $struct {
                op: <$op>::$variant,
                lhs,
                rhs,
            }
        }
    };
}

macro_rules! unary_factory {
    ($bound:path, $struct:ident, $field:ident, $kind:expr, $name:ident, $doc:expr) => {
        #[doc = $doc]
        pub fn $name<I: $bound>(inner: I) -> $struct<I> {
            $struct {
                $field: $kind,
                inner,
            }
        }
    };
}

macro_rules! float_unary_factory {
    ($bound:path, $variant:ident, $name:ident, $doc:expr) => {
        #[doc = $doc]
        pub fn $name<I: $bound>(inner: I) -> FloatUnaryFixed<I> {
            FloatUnaryFixed {
                op: FloatUnaryOp::$variant,
                inner,
            }
        }
    };
}

value_op_factories! {
    bound = MatchPat,
    verb = "Match",
    ne_one = crate::typed::consts::bool_const(true),
}

// Build-side twins, re-exported at the crate root as
// `strider_pattern::template`. Same structs, `TemplatePat` bound instead of
// `MatchPat`: a template builder accepts template-only operands (`ConstWith`,
// `var`, nested template ops) and refuses match-only ones (`anything()` /
// predicates / `*_any`), and vice versa.
pub mod template {
    use strider_ir::node::NodeKind;
    use strider_ir::{
        ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp,
    };

    use crate::template::template_pat::TemplatePat;
    use crate::typed::consts::bool_const;
    use crate::typed::value_ops::{
        BitNot, BoolBinaryFixed, BoolNot, Cast, FloatBinaryFixed, FloatCmpFixed, FloatSub,
        FloatUnaryFixed, IntBinaryFixed, IntCmpFixed, IntUnaryFixed, Sub,
    };

    value_op_factories! {
        bound = TemplatePat,
        verb = "Build",
        ne_one = bool_const(true),
    }
}
