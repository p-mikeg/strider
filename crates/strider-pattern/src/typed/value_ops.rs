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

use strider_ir::{
    ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp,
    node::{NodeKind, ValueType},
};

use crate::{
    matcher::{
        KindSpec, MatcherBuilder, PatValueRef,
        match_pat::{MatchPat, Pre},
    },
    template::{TemplateBuilder, TmplValueRef, template_pat::TemplatePat},
    typed::{
        builder_like::{
            compile_bool_binary, compile_int_binary, compile_two_input, compile_unary_kind,
        },
        consts::{int_const, int_const_with_fn},
    },
};

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
//   * `runtime_variant_binary!` — a runtime-op-carrying struct (+ free fn
//     + `MatchPat` + `TemplatePat`) that simply delegates to its hand-
//     written `*Fixed` sibling. The delegation is the same on both the
//     match and template side, so both impls are emitted from one call.
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

/// Emit a runtime-op-carrying binary struct (`$struct`), its free fn, and
/// both its `MatchPat` and `TemplatePat` impls. Both impls forward to the
/// hand-written `$fixed` sibling, so the runtime-variant builder is a
/// pure pass-through over the fixed-op one.
macro_rules! runtime_variant_binary {
    ($(#[$smeta:meta])* $struct:ident, $fixed:ident, $op:ty, $fn:ident, $fndoc:literal) => {
        $(#[$smeta])*
        pub struct $struct<L, R> {
            op: $op,
            lhs: L,
            rhs: R,
        }

        impl<L: MatchPat, R: MatchPat> MatchPat for $struct<L, R> {
            fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
                $fixed {
                    op: self.op,
                    lhs: self.lhs,
                    rhs: self.rhs,
                }
                .compile(b)
            }
        }

        impl<L: TemplatePat, R: TemplatePat> TemplatePat for $struct<L, R> {
            fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
                $fixed {
                    op: self.op,
                    lhs: self.lhs,
                    rhs: self.rhs,
                }
                .compile(b)
            }
        }

        #[doc = $fndoc]
        pub fn $fn<L: MatchPat, R: MatchPat>(op: $op, lhs: L, rhs: R) -> $struct<L, R> {
            $struct { op, lhs, rhs }
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

runtime_variant_binary!(
    /// A runtime-variant integer binary op `int_binary(op, l, r)`.
    IntBinary,
    IntBinaryFixed,
    IntBinaryOp,
    int_binary,
    "Variant-agnostic integer binary op `int_binary(op, l, r)`."
);

variant_binary_any!(
    /// Match **any** `IntBinaryOp` variant. Match-only.
    IntBinaryAny,
    int_binary_any,
    NodeKind::IntBinaryOp(IntBinaryOp::Add),
    "Match any `IntBinaryOp` regardless of variant."
);

macro_rules! int_binary_op {
    ($name:ident, $variant:ident, $doc:literal) => {
        #[doc = $doc]
        pub fn $name<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> IntBinaryFixed<L, R> {
            IntBinaryFixed {
                op: IntBinaryOp::$variant,
                lhs,
                rhs,
            }
        }
    };
}

int_binary_op!(
    add,
    Add,
    "Match unsigned addition `lhs + rhs`. Commutative."
);
int_binary_op!(
    mul,
    Mul,
    "Match wrapping multiplication `lhs * rhs`. Commutative."
);
int_binary_op!(div, Div, "Match unsigned division `lhs / rhs`.");
int_binary_op!(
    sdiv,
    Sdiv,
    "Match signed division `(signed)lhs / (signed)rhs`."
);
int_binary_op!(rem, Rem, "Match unsigned remainder `lhs % rhs`.");
int_binary_op!(
    srem,
    Srem,
    "Match signed remainder `(signed)lhs % (signed)rhs`."
);
int_binary_op!(and, And, "Match bitwise AND `lhs & rhs`. Commutative.");
int_binary_op!(or, Or, "Match bitwise OR `lhs | rhs`. Commutative.");
int_binary_op!(xor, Xor, "Match bitwise XOR `lhs ^ rhs`. Commutative.");
int_binary_op!(shl, ShiftLeft, "Match logical left-shift `lhs << rhs`.");
int_binary_op!(shr, ShiftRight, "Match logical right-shift `lhs >> rhs`.");
int_binary_op!(
    sshr,
    SShiftRight,
    "Match arithmetic right-shift `(signed)lhs >> rhs`."
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

/// Match `lhs - rhs` (the lifter's `Add(lhs, Neg(rhs))` shape).
pub fn sub<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> Sub<L, R> {
    Sub { lhs, rhs }
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

macro_rules! int_unary_op {
    ($name:ident, $kind:expr, $doc:literal) => {
        #[doc = $doc]
        pub fn $name<I: MatchPat>(inner: I) -> IntUnaryFixed<I> {
            IntUnaryFixed { kind: $kind, inner }
        }
    };
}

int_unary_op!(
    neg,
    NodeKind::IntUnaryOp(IntUnaryOp::Neg),
    "Match two's-complement negation `-inner`."
);
int_unary_op!(
    popcount,
    NodeKind::Popcount,
    "Match a `Popcount(inner)` (count-set-bits) node."
);
int_unary_op!(
    lzcount,
    NodeKind::Lzcount,
    "Match an `Lzcount(inner)` (leading-zero-count) node."
);

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

/// Match a bitwise complement `~inner` (the canonical `xor(_, all_ones)`).
pub fn bit_not<I: MatchPat>(inner: I) -> BitNot<I> {
    BitNot { inner }
}

/// Alias for [`bit_not`] (matches the Python `not_` keyword-collision name).
pub fn not_<I: MatchPat>(inner: I) -> BitNot<I> {
    bit_not(inner)
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

macro_rules! cast_op {
    ($name:ident, $kind:expr, $doc:literal) => {
        #[doc = $doc]
        pub fn $name<I: MatchPat>(inner: I) -> Cast<I> {
            Cast { kind: $kind, inner }
        }
    };
}

cast_op!(
    truncate,
    NodeKind::Truncate,
    "Match a `Truncate(inner)` (integer narrowing) node."
);
cast_op!(
    zero_extend,
    NodeKind::Extend(ExtendOp::ZeroExtend),
    "Match a zero-extension `Extend(ZeroExtend)(inner)` node."
);
cast_op!(
    sign_extend,
    NodeKind::Extend(ExtendOp::SignExtend),
    "Match a sign-extension `Extend(SignExtend)(inner)` node."
);
cast_op!(
    int_to_float,
    NodeKind::IntToFloat,
    "Match an `IntToFloat(inner)` value-conversion."
);
cast_op!(
    float_to_int,
    NodeKind::FloatToInt,
    "Match a `FloatToInt(inner)` value-conversion."
);
cast_op!(
    int_bits_to_float,
    NodeKind::IntBitsToFloat,
    "Match an `IntBitsToFloat(inner)` bitcast."
);
cast_op!(
    float_bits_to_int,
    NodeKind::FloatBitsToInt,
    "Match a `FloatBitsToInt(inner)` bitcast."
);
cast_op!(
    float_to_float,
    NodeKind::FloatToFloat,
    "Match a `FloatToFloat(inner)` precision-conversion."
);

/// Match an `Extend(op)` node with the given runtime `ExtendOp`.
pub fn extend<I: MatchPat>(op: ExtendOp, inner: I) -> Cast<I> {
    Cast {
        kind: NodeKind::Extend(op),
        inner,
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

runtime_variant_binary!(
    /// Runtime-variant integer comparison `int_cmp(op, l, r)` (output `I1`).
    IntCmp,
    IntCmpFixed,
    IntCmpOp,
    int_cmp,
    "Variant-agnostic integer comparison `int_cmp(op, l, r)`."
);

variant_binary_any!(
    /// Match **any** `IntCmpOp` regardless of variant (output `I1`). Match-only.
    IntCmpAny,
    int_cmp_any,
    NodeKind::IntCmpOp(IntCmpOp::Equal),
    "Match any `IntCmpOp` regardless of variant.",
    pin_i1
);

macro_rules! int_cmp_op {
    ($name:ident, $variant:ident, $doc:literal) => {
        #[doc = $doc]
        pub fn $name<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> IntCmpFixed<L, R> {
            IntCmpFixed {
                op: IntCmpOp::$variant,
                lhs,
                rhs,
            }
        }
    };
}

int_cmp_op!(
    int_eq,
    Equal,
    "Match an unsigned equality `lhs == rhs`. Commutative."
);
int_cmp_op!(int_lt, Less, "Match an unsigned less-than `lhs < rhs`.");
int_cmp_op!(
    int_slt,
    Sless,
    "Match a signed less-than `(signed)lhs < (signed)rhs`."
);
int_cmp_op!(
    int_carry,
    Carry,
    "Match an unsigned addition carry-out. Commutative."
);
int_cmp_op!(
    int_scarry,
    Scarry,
    "Match a signed addition overflow. Commutative."
);
int_cmp_op!(int_sborrow, Sborrow, "Match a signed subtraction borrow.");

/// Match an unsigned not-equal `lhs != rhs` — `xor(int_eq(l, r), 1):I1`.
pub fn int_ne<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> impl MatchPat {
    xor(int_eq(lhs, rhs), bool_one())
}

/// Match an unsigned less-or-equal `lhs <= rhs` — `xor(int_lt(r, l), 1):I1`.
pub fn int_le<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> impl MatchPat {
    xor(int_lt(rhs, lhs), bool_one())
}

/// Match a signed less-or-equal `(signed)lhs <= (signed)rhs` —
/// `xor(int_slt(r, l), 1):I1`.
pub fn int_sle<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> impl MatchPat {
    xor(int_slt(rhs, lhs), bool_one())
}

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

runtime_variant_binary!(
    /// Variant-agnostic float binary op `float_binary(op, l, r)`.
    FloatBinary,
    FloatBinaryFixed,
    FloatBinaryOp,
    float_binary,
    "Variant-agnostic float binary op."
);

variant_binary_any!(
    /// Match **any** `FloatBinaryOp` variant. Match-only.
    FloatBinaryAny,
    float_binary_any,
    NodeKind::FloatBinaryOp(FloatBinaryOp::Add),
    "Match any `FloatBinaryOp` regardless of variant."
);

macro_rules! float_binary_op {
    ($name:ident, $variant:ident, $doc:literal) => {
        #[doc = $doc]
        pub fn $name<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> FloatBinaryFixed<L, R> {
            FloatBinaryFixed {
                op: FloatBinaryOp::$variant,
                lhs,
                rhs,
            }
        }
    };
}

float_binary_op!(
    float_add,
    Add,
    "Match a float addition `lhs + rhs`. Commutative."
);
float_binary_op!(
    float_mul,
    Mul,
    "Match a float multiplication `lhs * rhs`. Commutative."
);
float_binary_op!(float_div, Div, "Match a float division `lhs / rhs`.");

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

/// Match a float subtraction `lhs - rhs`.
pub fn float_sub<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> FloatSub<L, R> {
    FloatSub { lhs, rhs }
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

macro_rules! float_unary_op {
    ($name:ident, $variant:ident, $doc:literal) => {
        #[doc = $doc]
        pub fn $name<I: MatchPat>(inner: I) -> FloatUnaryFixed<I> {
            FloatUnaryFixed {
                op: FloatUnaryOp::$variant,
                inner,
            }
        }
    };
}

float_unary_op!(float_neg, Neg, "Match a float negation `-x`.");
float_unary_op!(float_abs, Abs, "Match a float absolute value `|x|`.");
float_unary_op!(float_sqrt, Sqrt, "Match a float square root `√x`.");
float_unary_op!(float_ceil, Ceil, "Match a float ceiling `⌈x⌉`.");
float_unary_op!(float_floor, Floor, "Match a float floor `⌊x⌋`.");
float_unary_op!(float_round, Round, "Match a float round-to-nearest-even.");

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

runtime_variant_binary!(
    /// Runtime-variant float comparison `float_cmp(op, l, r)` (output `I1`).
    FloatCmp,
    FloatCmpFixed,
    FloatCmpOp,
    float_cmp,
    "Variant-agnostic float comparison."
);

variant_binary_any!(
    /// Match **any** `FloatCmpOp` regardless of variant (output `I1`). Match-only.
    FloatCmpAny,
    float_cmp_any,
    NodeKind::FloatCmpOp(FloatCmpOp::Equal),
    "Match any `FloatCmpOp` regardless of variant.",
    pin_i1
);

macro_rules! float_cmp_op {
    ($name:ident, $variant:ident, $doc:literal) => {
        #[doc = $doc]
        pub fn $name<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> FloatCmpFixed<L, R> {
            FloatCmpFixed {
                op: FloatCmpOp::$variant,
                lhs,
                rhs,
            }
        }
    };
}

float_cmp_op!(
    float_eq,
    Equal,
    "Match a float equality `lhs == rhs`. Commutative."
);
float_cmp_op!(float_lt, Less, "Match a float less-than `lhs < rhs`.");

/// Match a float not-equal `lhs != rhs` — `xor(float_eq(l, r), 1):I1`.
pub fn float_ne<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> impl MatchPat {
    xor(float_eq(lhs, rhs), bool_one())
}

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
        bool_binary_out(b, IntBinaryOp::Or, less, equal)
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
        let one = bool_one().compile(b);
        bool_binary_out(b, IntBinaryOp::Xor, eq, one)
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

runtime_variant_binary!(
    /// Runtime-variant boolean binary op `bool_binary(op, l, r)` (output `I1`).
    BoolBinary,
    BoolBinaryFixed,
    IntBinaryOp,
    bool_binary,
    "Variant-agnostic boolean binary op (`IntBinaryOp` at `I1`)."
);

variant_binary_any!(
    /// Match **any** `IntBinaryOp` at `I1` regardless of variant. Match-only.
    BoolBinaryAny,
    bool_bin_any,
    NodeKind::IntBinaryOp(IntBinaryOp::And),
    "Match any `IntBinaryOp` at `I1` regardless of variant.",
    pin_i1
);

macro_rules! bool_op {
    ($name:ident, $variant:ident, $doc:literal) => {
        #[doc = $doc]
        pub fn $name<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> BoolBinaryFixed<L, R> {
            BoolBinaryFixed {
                op: IntBinaryOp::$variant,
                lhs,
                rhs,
            }
        }
    };
}

bool_op!(
    bool_and,
    And,
    "Match a boolean AND (`IntBinaryOp::And` at `I1`). Commutative."
);
bool_op!(
    bool_or,
    Or,
    "Match a boolean OR (`IntBinaryOp::Or` at `I1`). Commutative."
);
bool_op!(
    bool_xor,
    Xor,
    "Match a boolean XOR (`IntBinaryOp::Xor` at `I1`). Commutative."
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

/// Match a boolean NOT — `xor(operand, IntConst(1)):I1`.
pub fn bool_not<I: MatchPat>(operand: I) -> BoolNot<I> {
    BoolNot { inner: operand }
}

// ── Shared helpers ────────────────────────────────────────────────────

/// Wire a binary `IntBinaryOp` consuming `l` / `r`, pinning the output to
/// `I1` (the boolean shape).
fn bool_binary_out(
    b: &mut MatcherBuilder,
    op: IntBinaryOp,
    l: PatValueRef,
    r: PatValueRef,
) -> PatValueRef {
    let out = b.binary(op, l, r);
    b.set_value_ty(out, ValueType::I1);
    out
}

/// A `bool_one` operand re-presented as a [`MatchPat`].
fn bool_one() -> impl MatchPat {
    crate::typed::consts::bool_const(true)
}

// ── Template-side (build) twins ───────────────────────────────────────
//
// Every composite op above also gets a build-side twin here, gathered in
// the `template` module (re-exported at the crate root as
// `strider_pattern::template`). The twins construct the *same* structs as
// the match-side factories — the structs' `TemplatePat` impls already
// know how to lower into a [`TemplateBuilder`] — but their constructor
// bounds are `TemplatePat` instead of `MatchPat`. That moves the
// match/template boundary to **construction**: a template builder accepts
// template-only operands (`ConstWith`, `var`, nested template ops) and
// refuses match-only operands (`any()` / predicates / `*_any`), while the
// bare match builders refuse template-only operands.
//
// The twins are emitted by a parallel macro pass over the same op lists
// the match side uses — a single `mod` can't be re-opened by separate
// per-op macro invocations, so the match factories emit in this module's
// scope and these twins are generated together inside one `mod template`.
pub mod template {
    use strider_ir::{
        ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, node::NodeKind,
    };

    use crate::{
        template::template_pat::TemplatePat,
        typed::{
            consts::bool_const,
            value_ops::{
                BitNot, BoolBinary, BoolBinaryFixed, BoolNot, Cast, FloatBinary, FloatBinaryFixed,
                FloatCmp, FloatCmpFixed, FloatSub, FloatUnaryFixed, IntBinary, IntBinaryFixed,
                IntCmp, IntCmpFixed, IntUnaryFixed, Sub,
            },
        },
    };

    /// Build-side twin of an integer binary op (`IntBinaryFixed`).
    macro_rules! int_binary_op_tpl {
        ($name:ident, $variant:ident, $doc:literal) => {
            #[doc = $doc]
            pub fn $name<L: TemplatePat, R: TemplatePat>(lhs: L, rhs: R) -> IntBinaryFixed<L, R> {
                IntBinaryFixed {
                    op: IntBinaryOp::$variant,
                    lhs,
                    rhs,
                }
            }
        };
    }

    int_binary_op_tpl!(add, Add, "Build an unsigned addition `lhs + rhs`.");
    int_binary_op_tpl!(mul, Mul, "Build a wrapping multiplication `lhs * rhs`.");
    int_binary_op_tpl!(div, Div, "Build an unsigned division `lhs / rhs`.");
    int_binary_op_tpl!(
        sdiv,
        Sdiv,
        "Build a signed division `(signed)lhs / (signed)rhs`."
    );
    int_binary_op_tpl!(rem, Rem, "Build an unsigned remainder `lhs % rhs`.");
    int_binary_op_tpl!(
        srem,
        Srem,
        "Build a signed remainder `(signed)lhs % (signed)rhs`."
    );
    int_binary_op_tpl!(and, And, "Build a bitwise AND `lhs & rhs`.");
    int_binary_op_tpl!(or, Or, "Build a bitwise OR `lhs | rhs`.");
    int_binary_op_tpl!(xor, Xor, "Build a bitwise XOR `lhs ^ rhs`.");
    int_binary_op_tpl!(shl, ShiftLeft, "Build a logical left-shift `lhs << rhs`.");
    int_binary_op_tpl!(shr, ShiftRight, "Build a logical right-shift `lhs >> rhs`.");
    int_binary_op_tpl!(
        sshr,
        SShiftRight,
        "Build an arithmetic right-shift `(signed)lhs >> rhs`."
    );

    /// Build a subtraction `lhs - rhs` (the lifter's `Add(lhs, Neg(rhs))` shape).
    pub fn sub<L: TemplatePat, R: TemplatePat>(lhs: L, rhs: R) -> Sub<L, R> {
        Sub { lhs, rhs }
    }

    /// Build-side twin of a fixed-kind integer unary op.
    macro_rules! int_unary_op_tpl {
        ($name:ident, $kind:expr, $doc:literal) => {
            #[doc = $doc]
            pub fn $name<I: TemplatePat>(inner: I) -> IntUnaryFixed<I> {
                IntUnaryFixed { kind: $kind, inner }
            }
        };
    }

    int_unary_op_tpl!(
        neg,
        NodeKind::IntUnaryOp(strider_ir::IntUnaryOp::Neg),
        "Build a two's-complement negation `-inner`."
    );
    int_unary_op_tpl!(
        popcount,
        NodeKind::Popcount,
        "Build a `Popcount(inner)` node."
    );
    int_unary_op_tpl!(
        lzcount,
        NodeKind::Lzcount,
        "Build an `Lzcount(inner)` node."
    );

    /// Build a bitwise complement `~inner` (the canonical `xor(_, all_ones)`).
    pub fn bit_not<I: TemplatePat>(inner: I) -> BitNot<I> {
        BitNot { inner }
    }

    /// Alias for [`bit_not`] (the Python `not_` keyword-collision name).
    pub fn not_<I: TemplatePat>(inner: I) -> BitNot<I> {
        bit_not(inner)
    }

    /// Build-side twin of a unary-shape cast.
    macro_rules! cast_op_tpl {
        ($name:ident, $kind:expr, $doc:literal) => {
            #[doc = $doc]
            pub fn $name<I: TemplatePat>(inner: I) -> Cast<I> {
                Cast { kind: $kind, inner }
            }
        };
    }

    cast_op_tpl!(
        truncate,
        NodeKind::Truncate,
        "Build a `Truncate(inner)` node."
    );
    cast_op_tpl!(
        zero_extend,
        NodeKind::Extend(ExtendOp::ZeroExtend),
        "Build a `Extend(ZeroExtend)(inner)` node."
    );
    cast_op_tpl!(
        sign_extend,
        NodeKind::Extend(ExtendOp::SignExtend),
        "Build a `Extend(SignExtend)(inner)` node."
    );
    cast_op_tpl!(
        int_to_float,
        NodeKind::IntToFloat,
        "Build an `IntToFloat(inner)` node."
    );
    cast_op_tpl!(
        float_to_int,
        NodeKind::FloatToInt,
        "Build a `FloatToInt(inner)` node."
    );
    cast_op_tpl!(
        int_bits_to_float,
        NodeKind::IntBitsToFloat,
        "Build an `IntBitsToFloat(inner)` node."
    );
    cast_op_tpl!(
        float_bits_to_int,
        NodeKind::FloatBitsToInt,
        "Build a `FloatBitsToInt(inner)` node."
    );
    cast_op_tpl!(
        float_to_float,
        NodeKind::FloatToFloat,
        "Build a `FloatToFloat(inner)` node."
    );

    /// Build an `Extend(op)` node with the given runtime `ExtendOp`.
    pub fn extend<I: TemplatePat>(op: ExtendOp, inner: I) -> Cast<I> {
        Cast {
            kind: NodeKind::Extend(op),
            inner,
        }
    }

    /// Build-side twin of a fixed-variant integer comparison (output `I1`).
    macro_rules! int_cmp_op_tpl {
        ($name:ident, $variant:ident, $doc:literal) => {
            #[doc = $doc]
            pub fn $name<L: TemplatePat, R: TemplatePat>(lhs: L, rhs: R) -> IntCmpFixed<L, R> {
                IntCmpFixed {
                    op: IntCmpOp::$variant,
                    lhs,
                    rhs,
                }
            }
        };
    }

    int_cmp_op_tpl!(int_eq, Equal, "Build an unsigned equality `lhs == rhs`.");
    int_cmp_op_tpl!(int_lt, Less, "Build an unsigned less-than `lhs < rhs`.");
    int_cmp_op_tpl!(
        int_slt,
        Sless,
        "Build a signed less-than `(signed)lhs < (signed)rhs`."
    );
    int_cmp_op_tpl!(int_carry, Carry, "Build an unsigned addition carry-out.");
    int_cmp_op_tpl!(int_scarry, Scarry, "Build a signed addition overflow.");
    int_cmp_op_tpl!(int_sborrow, Sborrow, "Build a signed subtraction borrow.");

    /// Build an unsigned not-equal `lhs != rhs` — `xor(int_eq(l, r), 1):I1`.
    pub fn int_ne<L: TemplatePat, R: TemplatePat>(lhs: L, rhs: R) -> impl TemplatePat {
        xor(int_eq(lhs, rhs), bool_const(true))
    }

    /// Build an unsigned less-or-equal `lhs <= rhs` — `xor(int_lt(r, l), 1):I1`.
    pub fn int_le<L: TemplatePat, R: TemplatePat>(lhs: L, rhs: R) -> impl TemplatePat {
        xor(int_lt(rhs, lhs), bool_const(true))
    }

    /// Build a signed less-or-equal `(signed)lhs <= (signed)rhs` —
    /// `xor(int_slt(r, l), 1):I1`.
    pub fn int_sle<L: TemplatePat, R: TemplatePat>(lhs: L, rhs: R) -> impl TemplatePat {
        xor(int_slt(rhs, lhs), bool_const(true))
    }

    /// Variant-agnostic integer binary op `int_binary(op, l, r)`.
    pub fn int_binary<L: TemplatePat, R: TemplatePat>(
        op: IntBinaryOp,
        lhs: L,
        rhs: R,
    ) -> IntBinary<L, R> {
        IntBinary { op, lhs, rhs }
    }

    /// Variant-agnostic integer comparison `int_cmp(op, l, r)` (output `I1`).
    pub fn int_cmp<L: TemplatePat, R: TemplatePat>(op: IntCmpOp, lhs: L, rhs: R) -> IntCmp<L, R> {
        IntCmp { op, lhs, rhs }
    }

    /// Build-side twin of a fixed-variant float binary op.
    macro_rules! float_binary_op_tpl {
        ($name:ident, $variant:ident, $doc:literal) => {
            #[doc = $doc]
            pub fn $name<L: TemplatePat, R: TemplatePat>(lhs: L, rhs: R) -> FloatBinaryFixed<L, R> {
                FloatBinaryFixed {
                    op: FloatBinaryOp::$variant,
                    lhs,
                    rhs,
                }
            }
        };
    }

    float_binary_op_tpl!(float_add, Add, "Build a float addition `lhs + rhs`.");
    float_binary_op_tpl!(float_mul, Mul, "Build a float multiplication `lhs * rhs`.");
    float_binary_op_tpl!(float_div, Div, "Build a float division `lhs / rhs`.");

    /// Build a float subtraction `lhs - rhs`.
    pub fn float_sub<L: TemplatePat, R: TemplatePat>(lhs: L, rhs: R) -> FloatSub<L, R> {
        FloatSub { lhs, rhs }
    }

    /// Build-side twin of a fixed-variant float unary op.
    macro_rules! float_unary_op_tpl {
        ($name:ident, $variant:ident, $doc:literal) => {
            #[doc = $doc]
            pub fn $name<I: TemplatePat>(inner: I) -> FloatUnaryFixed<I> {
                FloatUnaryFixed {
                    op: FloatUnaryOp::$variant,
                    inner,
                }
            }
        };
    }

    float_unary_op_tpl!(float_neg, Neg, "Build a float negation `-x`.");
    float_unary_op_tpl!(float_abs, Abs, "Build a float absolute value `|x|`.");
    float_unary_op_tpl!(float_sqrt, Sqrt, "Build a float square root `√x`.");
    float_unary_op_tpl!(float_ceil, Ceil, "Build a float ceiling `⌈x⌉`.");
    float_unary_op_tpl!(float_floor, Floor, "Build a float floor `⌊x⌋`.");
    float_unary_op_tpl!(float_round, Round, "Build a float round-to-nearest-even.");

    /// Build-side twin of a fixed-variant float comparison (output `I1`).
    macro_rules! float_cmp_op_tpl {
        ($name:ident, $variant:ident, $doc:literal) => {
            #[doc = $doc]
            pub fn $name<L: TemplatePat, R: TemplatePat>(lhs: L, rhs: R) -> FloatCmpFixed<L, R> {
                FloatCmpFixed {
                    op: FloatCmpOp::$variant,
                    lhs,
                    rhs,
                }
            }
        };
    }

    float_cmp_op_tpl!(float_eq, Equal, "Build a float equality `lhs == rhs`.");
    float_cmp_op_tpl!(float_lt, Less, "Build a float less-than `lhs < rhs`.");

    /// Build a float not-equal `lhs != rhs` — `xor(float_eq(l, r), 1):I1`.
    pub fn float_ne<L: TemplatePat, R: TemplatePat>(lhs: L, rhs: R) -> impl TemplatePat {
        xor(float_eq(lhs, rhs), bool_const(true))
    }

    /// Variant-agnostic float binary op `float_binary(op, l, r)`.
    pub fn float_binary<L: TemplatePat, R: TemplatePat>(
        op: FloatBinaryOp,
        lhs: L,
        rhs: R,
    ) -> FloatBinary<L, R> {
        FloatBinary { op, lhs, rhs }
    }

    /// Variant-agnostic float comparison `float_cmp(op, l, r)` (output `I1`).
    pub fn float_cmp<L: TemplatePat, R: TemplatePat>(
        op: FloatCmpOp,
        lhs: L,
        rhs: R,
    ) -> FloatCmp<L, R> {
        FloatCmp { op, lhs, rhs }
    }

    /// Build-side twin of a fixed-variant boolean binary op (output `I1`).
    macro_rules! bool_op_tpl {
        ($name:ident, $variant:ident, $doc:literal) => {
            #[doc = $doc]
            pub fn $name<L: TemplatePat, R: TemplatePat>(lhs: L, rhs: R) -> BoolBinaryFixed<L, R> {
                BoolBinaryFixed {
                    op: IntBinaryOp::$variant,
                    lhs,
                    rhs,
                }
            }
        };
    }

    bool_op_tpl!(
        bool_and,
        And,
        "Build a boolean AND (`IntBinaryOp::And` at `I1`)."
    );
    bool_op_tpl!(
        bool_or,
        Or,
        "Build a boolean OR (`IntBinaryOp::Or` at `I1`)."
    );
    bool_op_tpl!(
        bool_xor,
        Xor,
        "Build a boolean XOR (`IntBinaryOp::Xor` at `I1`)."
    );

    /// Build a boolean NOT — `xor(operand, IntConst(1)):I1`.
    pub fn bool_not<I: TemplatePat>(operand: I) -> BoolNot<I> {
        BoolNot { inner: operand }
    }

    /// Variant-agnostic boolean binary op `bool_binary(op, l, r)` (output `I1`).
    pub fn bool_binary<L: TemplatePat, R: TemplatePat>(
        op: IntBinaryOp,
        lhs: L,
        rhs: R,
    ) -> BoolBinary<L, R> {
        BoolBinary { op, lhs, rhs }
    }
}
