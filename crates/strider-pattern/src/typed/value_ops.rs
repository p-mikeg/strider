//! Binary / unary / cast / comparison typed builders for integer,
//! float, and boolean operations.
//!
//! Each fixed-variant op is a typed struct (generic over its operand
//! pattern types) whose `compile` wires the exact `NodeKind`. Lift-time
//! canonicalisations (`sub`→`add(_, neg(_))`, `bit_not`→`xor(_,
//! all_ones)`, the lowered `int_ne`/`int_le`/`float_*` shapes) are
//! reproduced so a typed pattern matches the canonical IR the lifter
//! produces. Boolean ops pin the output to `I1`. Commutativity is
//! data-driven (`NodeKind::is_commutative`), so no per-struct work.

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

// ── DRY macros for the repetitive op families ─────────────────────────
//
// The op families below come in three copy-paste shapes that these
// macros collapse so adding an op is a single macro line:
//
//   * `variant_binary_any!` — a match-only `*Any` struct (+ free fn +
//     `MatchPat`) that matches **any** variant of a binary node kind via
//     `KindSpec::Variant(discriminant)`. The `$pin_i1` arm parameterises
//     the boolean/comparison output-type pin (`I1`).
//   * `variant_unary_any!` — the unary counterpart (`b.unary` over a
//     `KindSpec::Variant`); no output-type pin is ever needed.
//
// The *fixed-op* factory free functions (`add`, `int_eq`, `float_add`,
// …) and the runtime-op-carrying ones (`int_binary`, `int_cmp`, …) are
// emitted on BOTH the match side (crate scope, `MatchPat` bound) and the
// build side (`mod template`, `TemplatePat` bound) from a SINGLE op list
// in `value_op_factories!` (invoked once per scope at the end of the
// file). Both sides build the identical `*Fixed` structs — the only
// difference is the constructor's trait bound, which moves the
// match/template typed boundary to construction.
//
// Each family's *fixed-op* structs (`IntBinaryFixed`, …) and their unique
// lowered-shape siblings (`Sub`, `BitNot`, `FloatSub`, `FloatLe`, …) stay
// hand-written: their bodies differ per op, so a macro would obscure them.

/// Emit a match-only `*Any` binary struct, its free fn, and its
/// `MatchPat` impl. The node matches **any** variant of `$exemplar`'s
/// kind via `KindSpec::Variant`. Pass a trailing `pin_i1` to pin the
/// output to `I1` (booleans / comparisons); omit it for value outputs.
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
                let n = b.node(KindSpec::Variant(std::mem::discriminant(&exemplar)));
                let l = self.lhs.compile(b);
                let r = self.rhs.compile(b);
                b.input(n, 0, l);
                b.input(n, 1, r);
                let out = b.value_output(n, 0);
                // The optional `$pin_i1` token gates the `I1` output-type pin
                // for the boolean / comparison families; the value families
                // (`IntBinaryAny`, `FloatBinaryAny`) omit it. `stringify!`
                // consumes the token without emitting any code of its own.
                $(
                    let _ = stringify!($pin_i1);
                    b.set_value_ty(out, ValueType::I1);
                )?
                out
            }
        }

        #[doc = $fndoc]
        pub fn $fn<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> $struct<L, R> {
            $struct { lhs, rhs }
        }
    };
}

/// Emit a match-only `*Any` unary struct, its free fn, and its
/// `MatchPat` impl. The node matches **any** variant of `$exemplar`'s
/// kind via `b.unary(KindSpec::Variant(...))`.
macro_rules! variant_unary_any {
    ($(#[$smeta:meta])* $struct:ident, $fn:ident, $exemplar:expr, $fndoc:literal) => {
        $(#[$smeta])*
        pub struct $struct<I> {
            inner: I,
        }

        impl<I: MatchPat> MatchPat for $struct<I> {
            fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
                let exemplar = $exemplar;
                let i = self.inner.compile(b);
                b.unary(KindSpec::Variant(std::mem::discriminant(&exemplar)), i)
            }
        }

        #[doc = $fndoc]
        pub fn $fn<I: MatchPat>(inner: I) -> $struct<I> {
            $struct { inner }
        }
    };
}

// ── Integer binary ops ────────────────────────────────────────────────

/// A fixed-variant integer binary op `lhs ∘ rhs`.
pub struct IntBinaryFixed<L, R> {
    op: IntBinaryOp,
    lhs: L,
    rhs: R,
}

impl<L: MatchPat, R: MatchPat> MatchPat for IntBinaryFixed<L, R> {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        compile_int_binary(b, self.op, self.lhs, self.rhs)
    }
}

impl<L: TemplatePat, R: TemplatePat> TemplatePat for IntBinaryFixed<L, R> {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        compile_int_binary(b, self.op, self.lhs, self.rhs)
    }
}

variant_binary_any!(
    /// Match **any** `IntBinaryOp` variant. Match-only.
    IntBinaryAny,
    int_binary_any,
    NodeKind::IntBinaryOp(IntBinaryOp::Add),
    "Match any `IntBinaryOp` regardless of variant."
);

/// Match a subtraction `lhs - rhs`, lowered to `add(lhs, neg(rhs))`.
pub struct Sub<L, R> {
    lhs: L,
    rhs: R,
}

impl<L: MatchPat, R: MatchPat> MatchPat for Sub<L, R> {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        compile_sub(b, self.lhs, self.rhs)
    }
}

impl<L: TemplatePat, R: TemplatePat> TemplatePat for Sub<L, R> {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        compile_sub(b, self.lhs, self.rhs)
    }
}

/// `add(lhs, neg(rhs))` — the lifter's lowered subtraction shape, shared
/// across the match and build sides.
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

// ── Integer unary ops ─────────────────────────────────────────────────

/// A fixed-kind integer unary op (`Neg`, `Popcount`, `Lzcount`).
pub struct IntUnaryFixed<I> {
    kind: NodeKind,
    inner: I,
}

impl<I: MatchPat> MatchPat for IntUnaryFixed<I> {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        compile_unary_kind(b, KindSpec::Exact(self.kind), self.inner)
    }
}

impl<I: TemplatePat> TemplatePat for IntUnaryFixed<I> {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        compile_unary_kind(b, KindSpec::Exact(self.kind), self.inner)
    }
}

variant_unary_any!(
    /// Match **any** `IntUnaryOp` variant. Match-only.
    IntUnaryAny,
    int_unary_any,
    NodeKind::IntUnaryOp(IntUnaryOp::Neg),
    "Match any `IntUnaryOp` regardless of variant."
);

/// Match a bitwise complement `~inner` — `xor(inner, all_ones)`.
pub struct BitNot<I> {
    inner: I,
}

impl<I: MatchPat> MatchPat for BitNot<I> {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        // `int_const`'s match is width-masked, so `int_const(u128::MAX)`
        // matches the all-ones constant at any output width.
        xor(self.inner, int_const(u128::MAX)).compile(b)
    }
}

impl<I: TemplatePat> TemplatePat for BitNot<I> {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        // `create_node` stores an `IntConst` value verbatim (it does not
        // mask to the output width), so a plain `int_const(u128::MAX)`
        // template would materialise a raw `u128::MAX` rather than the
        // width-relative all-ones bit pattern. Compute the masked
        // all-ones from the rewrite root's resolved width instead — the
        // output type inherits the root, so `ctx.root_ty` is that width.
        let i = self.inner.compile(b);
        let ones_out = int_const_with_fn(|ctx| Ok(ctx.root_ty.bit_mask_u128())).compile(b);
        b.binary(IntBinaryOp::Xor, i, ones_out)
    }
}

// ── Casts / coercions ─────────────────────────────────────────────────

/// A unary-shape cast wrapping `inner` with an exact `NodeKind`.
pub struct Cast<I> {
    kind: NodeKind,
    inner: I,
}

impl<I: MatchPat> MatchPat for Cast<I> {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        compile_unary_kind(b, KindSpec::Exact(self.kind), self.inner)
    }
}

impl<I: TemplatePat> TemplatePat for Cast<I> {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        compile_unary_kind(b, KindSpec::Exact(self.kind), self.inner)
    }
}

// ── Integer comparisons (output I1) ───────────────────────────────────

/// A fixed-variant integer comparison `lhs ∘ rhs` (output `I1`).
pub struct IntCmpFixed<L, R> {
    op: IntCmpOp,
    lhs: L,
    rhs: R,
}

impl<L: MatchPat, R: MatchPat> MatchPat for IntCmpFixed<L, R> {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        let kind = KindSpec::Exact(NodeKind::IntCmpOp(self.op));
        compile_two_input(b, kind, self.lhs, self.rhs, Some(ValueType::I1))
    }
}

impl<L: TemplatePat, R: TemplatePat> TemplatePat for IntCmpFixed<L, R> {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        let kind = KindSpec::Exact(NodeKind::IntCmpOp(self.op));
        compile_two_input(b, kind, self.lhs, self.rhs, Some(ValueType::I1))
    }
}

variant_binary_any!(
    /// Match **any** `IntCmpOp` regardless of variant (output `I1`). Match-only.
    IntCmpAny,
    int_cmp_any,
    NodeKind::IntCmpOp(IntCmpOp::Equal),
    "Match any `IntCmpOp` regardless of variant.",
    pin_i1
);

// ── Float binary / unary / comparison ops ─────────────────────────────

/// A fixed-variant float binary op `lhs ∘ rhs`.
pub struct FloatBinaryFixed<L, R> {
    op: FloatBinaryOp,
    lhs: L,
    rhs: R,
}

impl<L: MatchPat, R: MatchPat> MatchPat for FloatBinaryFixed<L, R> {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        let kind = KindSpec::Exact(NodeKind::FloatBinaryOp(self.op));
        compile_two_input(b, kind, self.lhs, self.rhs, None)
    }
}

impl<L: TemplatePat, R: TemplatePat> TemplatePat for FloatBinaryFixed<L, R> {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        let kind = KindSpec::Exact(NodeKind::FloatBinaryOp(self.op));
        compile_two_input(b, kind, self.lhs, self.rhs, None)
    }
}

variant_binary_any!(
    /// Match **any** `FloatBinaryOp` variant. Match-only.
    FloatBinaryAny,
    float_binary_any,
    NodeKind::FloatBinaryOp(FloatBinaryOp::Add),
    "Match any `FloatBinaryOp` regardless of variant."
);

/// Match a float subtraction `lhs - rhs`, lowered to
/// `float_add(lhs, float_neg(rhs))`.
pub struct FloatSub<L, R> {
    lhs: L,
    rhs: R,
}

impl<L: MatchPat, R: MatchPat> MatchPat for FloatSub<L, R> {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        compile_float_sub(b, self.lhs, self.rhs)
    }
}

impl<L: TemplatePat, R: TemplatePat> TemplatePat for FloatSub<L, R> {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        compile_float_sub(b, self.lhs, self.rhs)
    }
}

/// `float_add(lhs, float_neg(rhs))` — the lifter's lowered float
/// subtraction shape, shared across the match and build sides.
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

/// A fixed-variant float unary op.
pub struct FloatUnaryFixed<I> {
    op: FloatUnaryOp,
    inner: I,
}

impl<I: MatchPat> MatchPat for FloatUnaryFixed<I> {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        let kind = KindSpec::Exact(NodeKind::FloatUnaryOp(self.op));
        compile_unary_kind(b, kind, self.inner)
    }
}

impl<I: TemplatePat> TemplatePat for FloatUnaryFixed<I> {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        let kind = KindSpec::Exact(NodeKind::FloatUnaryOp(self.op));
        compile_unary_kind(b, kind, self.inner)
    }
}

variant_unary_any!(
    /// Match **any** `FloatUnaryOp` variant. Match-only.
    FloatUnaryAny,
    float_unary_any,
    NodeKind::FloatUnaryOp(FloatUnaryOp::Neg),
    "Match any `FloatUnaryOp` regardless of variant."
);

/// A fixed-variant float comparison `lhs ∘ rhs` (output `I1`).
pub struct FloatCmpFixed<L, R> {
    op: FloatCmpOp,
    lhs: L,
    rhs: R,
}

impl<L: MatchPat, R: MatchPat> MatchPat for FloatCmpFixed<L, R> {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        let kind = KindSpec::Exact(NodeKind::FloatCmpOp(self.op));
        compile_two_input(b, kind, self.lhs, self.rhs, Some(ValueType::I1))
    }
}

impl<L: TemplatePat, R: TemplatePat> TemplatePat for FloatCmpFixed<L, R> {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        let kind = KindSpec::Exact(NodeKind::FloatCmpOp(self.op));
        compile_two_input(b, kind, self.lhs, self.rhs, Some(ValueType::I1))
    }
}

variant_binary_any!(
    /// Match **any** `FloatCmpOp` regardless of variant (output `I1`). Match-only.
    FloatCmpAny,
    float_cmp_any,
    NodeKind::FloatCmpOp(FloatCmpOp::Equal),
    "Match any `FloatCmpOp` regardless of variant.",
    pin_i1
);

/// Match a float less-or-equal `lhs <= rhs` — NaN-aware
/// `or(float_lt(l, r), float_eq(l, r))` at `I1`. The operands are
/// compiled once and shared across both comparison branches.
pub struct FloatLe<L, R> {
    lhs: L,
    rhs: R,
}

impl<L: MatchPat, R: MatchPat> MatchPat for FloatLe<L, R> {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        // Each operand is referenced TWICE (once per cmp branch); a
        // move-by-value operand can't be consumed twice, so this can't
        // delegate to a free-fn one-liner — compile each operand once and
        // fan it out to both consumers via `Pre`.
        let l = self.lhs.compile(b);
        let r = self.rhs.compile(b);
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

/// Match a float less-or-equal `lhs <= rhs`.
pub fn float_le<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> FloatLe<L, R> {
    FloatLe { lhs, rhs }
}

/// Match `float_is_nan(x)` — `xor(float_eq(x, x), 1):I1`. The operand is
/// compiled once and shared across both equality inputs.
pub struct FloatIsNan<I> {
    inner: I,
}

impl<I: MatchPat> MatchPat for FloatIsNan<I> {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        // `x` is referenced TWICE (both inputs of the equality); a
        // move-by-value operand can't be consumed twice, so this can't
        // delegate to a free-fn one-liner — compile the operand once and
        // fan it out to both equality inputs via `Pre`.
        let x = self.inner.compile(b);
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

/// Match `float_is_nan(x)`.
pub fn float_is_nan<I: MatchPat>(inner: I) -> FloatIsNan<I> {
    FloatIsNan { inner }
}

// ── Boolean ops (output I1) ───────────────────────────────────────────

/// A fixed-variant boolean binary op `lhs ∘ rhs` (output `I1`).
pub struct BoolBinaryFixed<L, R> {
    op: IntBinaryOp,
    lhs: L,
    rhs: R,
}

impl<L: MatchPat, R: MatchPat> MatchPat for BoolBinaryFixed<L, R> {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        compile_bool_binary(b, self.op, self.lhs, self.rhs)
    }
}

impl<L: TemplatePat, R: TemplatePat> TemplatePat for BoolBinaryFixed<L, R> {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        compile_bool_binary(b, self.op, self.lhs, self.rhs)
    }
}

variant_binary_any!(
    /// Match **any** `IntBinaryOp` at `I1` regardless of variant. Match-only.
    BoolBinaryAny,
    bool_bin_any,
    NodeKind::IntBinaryOp(IntBinaryOp::And),
    "Match any `IntBinaryOp` at `I1` regardless of variant.",
    pin_i1
);

/// Match a boolean NOT `~operand` — `xor(operand, 1):I1`.
pub struct BoolNot<I> {
    inner: I,
}

impl<I: MatchPat> MatchPat for BoolNot<I> {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        compile_bool_not(b, self.inner)
    }
}

impl<I: TemplatePat> TemplatePat for BoolNot<I> {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        compile_bool_not(b, self.inner)
    }
}

/// `xor(inner, bool_const(true)):I1` — the lowered boolean-NOT shape,
/// shared across the match and build sides.
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

// ── Two-scopes-one-list factory functions ─────────────────────────────
//
// Every fixed-op / runtime-op / composite-lowered-shape factory free
// function is emitted from a SINGLE op list by `value_op_factories!`,
// once at crate scope (`MatchPat` bound, the match side) and once inside
// `mod template` (`TemplatePat` bound, the build side). Both sides build
// the identical structs (`IntBinaryFixed`, `Sub`, …) whose
// `MatchPat`/`TemplatePat` impls live above — the only difference is the
// constructor's trait bound, which moves the match/template typed
// boundary to construction. A `mod` can't be re-opened by per-op macro
// invocations, so the entire list is written once and the whole macro is
// invoked once per scope.
//
// Doc strings are parameterised per side ("Match X" vs "Build X") via the
// `$verb` token so neither side loses its prose.

macro_rules! value_op_factories {
    (
        // The operand-pattern trait bound for this scope's factories.
        bound = $bound:path,
        // The doc-string verb ("Match" or "Build").
        verb = $verb:literal,
        // The `1` operand for the lowered `int_ne`/`int_le`/… `xor` shapes:
        // `bool_const(true)`, supplied per scope so it carries the right
        // (`MatchPat` / `TemplatePat`) trait bound.
        ne_one = $ne_one:expr,
    ) => {
        // ── Integer binary fixed-op factories ─────────────────────────
        binary_factory!($bound, IntBinaryFixed, IntBinaryOp, Add, add,
            concat!($verb, " unsigned addition `lhs + rhs`. Commutative."));
        binary_factory!($bound, IntBinaryFixed, IntBinaryOp, Mul, mul,
            concat!($verb, " wrapping multiplication `lhs * rhs`. Commutative."));
        binary_factory!($bound, IntBinaryFixed, IntBinaryOp, Div, div,
            concat!($verb, " unsigned division `lhs / rhs`."));
        binary_factory!($bound, IntBinaryFixed, IntBinaryOp, Sdiv, sdiv,
            concat!($verb, " signed division `(signed)lhs / (signed)rhs`."));
        binary_factory!($bound, IntBinaryFixed, IntBinaryOp, Rem, rem,
            concat!($verb, " unsigned remainder `lhs % rhs`."));
        binary_factory!($bound, IntBinaryFixed, IntBinaryOp, Srem, srem,
            concat!($verb, " signed remainder `(signed)lhs % (signed)rhs`."));
        binary_factory!($bound, IntBinaryFixed, IntBinaryOp, And, and,
            concat!($verb, " bitwise AND `lhs & rhs`. Commutative."));
        binary_factory!($bound, IntBinaryFixed, IntBinaryOp, Or, or,
            concat!($verb, " bitwise OR `lhs | rhs`. Commutative."));
        binary_factory!($bound, IntBinaryFixed, IntBinaryOp, Xor, xor,
            concat!($verb, " bitwise XOR `lhs ^ rhs`. Commutative."));
        binary_factory!($bound, IntBinaryFixed, IntBinaryOp, ShiftLeft, shl,
            concat!($verb, " logical left-shift `lhs << rhs`."));
        binary_factory!($bound, IntBinaryFixed, IntBinaryOp, ShiftRight, shr,
            concat!($verb, " logical right-shift `lhs >> rhs`."));
        binary_factory!($bound, IntBinaryFixed, IntBinaryOp, SShiftRight, sshr,
            concat!($verb, " arithmetic right-shift `(signed)lhs >> rhs`."));

        /// Subtraction `lhs - rhs` (the lifter's `Add(lhs, Neg(rhs))` shape).
        pub fn sub<L: $bound, R: $bound>(lhs: L, rhs: R) -> Sub<L, R> {
            Sub { lhs, rhs }
        }

        // ── Integer unary fixed-kind factories ────────────────────────
        unary_factory!($bound, IntUnaryFixed, kind, NodeKind::IntUnaryOp(IntUnaryOp::Neg), neg,
            concat!($verb, " two's-complement negation `-inner`."));
        unary_factory!($bound, IntUnaryFixed, kind, NodeKind::Popcount, popcount,
            concat!($verb, " a `Popcount(inner)` (count-set-bits) node."));
        unary_factory!($bound, IntUnaryFixed, kind, NodeKind::Lzcount, lzcount,
            concat!($verb, " an `Lzcount(inner)` (leading-zero-count) node."));

        /// Bitwise complement `~inner` (the canonical `xor(_, all_ones)`).
        pub fn bit_not<I: $bound>(inner: I) -> BitNot<I> {
            BitNot { inner }
        }

        /// Alias for [`bit_not`] (matches the Python `not_` keyword-collision name).
        pub fn not_<I: $bound>(inner: I) -> BitNot<I> {
            bit_not(inner)
        }

        // ── Cast / coercion factories ─────────────────────────────────
        unary_factory!($bound, Cast, kind, NodeKind::Truncate, truncate,
            concat!($verb, " a `Truncate(inner)` (integer narrowing) node."));
        unary_factory!($bound, Cast, kind, NodeKind::Extend(ExtendOp::ZeroExtend), zero_extend,
            concat!($verb, " a zero-extension `Extend(ZeroExtend)(inner)` node."));
        unary_factory!($bound, Cast, kind, NodeKind::Extend(ExtendOp::SignExtend), sign_extend,
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
        pub fn extend<I: $bound>(op: ExtendOp, inner: I) -> Cast<I> {
            Cast { kind: NodeKind::Extend(op), inner }
        }

        // ── Integer comparison factories (output I1) ──────────────────
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

        #[doc = concat!($verb, " an unsigned not-equal `lhs != rhs` — `xor(int_eq(l, r), 1):I1`.")]
        pub fn int_ne<L: $bound, R: $bound>(lhs: L, rhs: R) -> impl $bound {
            xor(int_eq(lhs, rhs), $ne_one)
        }

        #[doc = concat!($verb, " an unsigned less-or-equal `lhs <= rhs` — `xor(int_lt(r, l), 1):I1`.")]
        pub fn int_le<L: $bound, R: $bound>(lhs: L, rhs: R) -> impl $bound {
            xor(int_lt(rhs, lhs), $ne_one)
        }

        #[doc = concat!($verb, " a signed less-or-equal `(signed)lhs <= (signed)rhs` — `xor(int_slt(r, l), 1):I1`.")]
        pub fn int_sle<L: $bound, R: $bound>(lhs: L, rhs: R) -> impl $bound {
            xor(int_slt(rhs, lhs), $ne_one)
        }

        // ── Float binary fixed-op factories ───────────────────────────
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

        // ── Float unary fixed-op factories ────────────────────────────
        float_unary_factory!($bound, Neg, float_neg, concat!($verb, " a float negation `-x`."));
        float_unary_factory!($bound, Abs, float_abs, concat!($verb, " a float absolute value `|x|`."));
        float_unary_factory!($bound, Sqrt, float_sqrt, concat!($verb, " a float square root `√x`."));
        float_unary_factory!($bound, Ceil, float_ceil, concat!($verb, " a float ceiling `⌈x⌉`."));
        float_unary_factory!($bound, Floor, float_floor, concat!($verb, " a float floor `⌊x⌋`."));
        float_unary_factory!($bound, Round, float_round, concat!($verb, " a float round-to-nearest-even."));

        // ── Float comparison factories (output I1) ────────────────────
        binary_factory!($bound, FloatCmpFixed, FloatCmpOp, Equal, float_eq,
            concat!($verb, " a float equality `lhs == rhs`. Commutative."));
        binary_factory!($bound, FloatCmpFixed, FloatCmpOp, Less, float_lt,
            concat!($verb, " a float less-than `lhs < rhs`."));

        #[doc = concat!($verb, " a float not-equal `lhs != rhs` — `xor(float_eq(l, r), 1):I1`.")]
        pub fn float_ne<L: $bound, R: $bound>(lhs: L, rhs: R) -> impl $bound {
            xor(float_eq(lhs, rhs), $ne_one)
        }

        // ── Boolean binary factories (output I1) ──────────────────────
        binary_factory!($bound, BoolBinaryFixed, IntBinaryOp, And, bool_and,
            concat!($verb, " a boolean AND (`IntBinaryOp::And` at `I1`). Commutative."));
        binary_factory!($bound, BoolBinaryFixed, IntBinaryOp, Or, bool_or,
            concat!($verb, " a boolean OR (`IntBinaryOp::Or` at `I1`). Commutative."));
        binary_factory!($bound, BoolBinaryFixed, IntBinaryOp, Xor, bool_xor,
            concat!($verb, " a boolean XOR (`IntBinaryOp::Xor` at `I1`). Commutative."));

        /// Boolean NOT — `xor(operand, IntConst(1)):I1`.
        pub fn bool_not<I: $bound>(operand: I) -> BoolNot<I> {
            BoolNot { inner: operand }
        }

        // ── Runtime-op-carrying factories (build the `*Fixed` directly) ──
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

/// Emit a fixed-variant binary-op factory `pub fn` building `$struct {
/// op: $op::$variant, lhs, rhs }` under the `$bound` operand bound.
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

/// Emit a fixed-kind unary-op factory `pub fn` building `$struct {
/// $field: $kind, inner }` under the `$bound` operand bound.
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

/// Emit a `FloatUnaryFixed` factory `pub fn` building it from a
/// `FloatUnaryOp::$variant` under the `$bound` operand bound.
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

// Match-side factories (crate scope, `MatchPat` bound). The lowered-shape
// `int_ne`/`int_le`/`float_ne` use `bool_const(true)` for the xor's `1`.
value_op_factories! {
    bound = MatchPat,
    verb = "Match",
    ne_one = crate::typed::consts::bool_const(true),
}

// ── Template-side (build) factory twins ───────────────────────────────
//
// The build-side twins (re-exported at the crate root as
// `strider_pattern::template`) construct the *same* structs the match
// side does, under a `TemplatePat` bound instead of `MatchPat`. That
// moves the match/template boundary to **construction**: a template
// builder accepts template-only operands (`ConstWith`, `var`, nested
// template ops) and refuses match-only operands (`any()` / predicates /
// `*_any`), while the bare match builders refuse template-only operands.
//
// The list is identical to the match side — both invoke the single
// `value_op_factories!` body — so adding an op stays one line on each
// side.
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
