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

use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir::{ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp};

use crate::builder::{MatcherBuilder, PatOutRef};
use crate::match_pat::{MatchPat, Pre};
use crate::pattern::KindSpec;
use crate::typed::consts::int_const_all_ones_pre;

// ── Integer binary ops ────────────────────────────────────────────────

/// A fixed-variant integer binary op `lhs ∘ rhs`.
pub struct IntBinaryFixed<L, R> {
    op: IntBinaryOp,
    lhs: L,
    rhs: R,
}

impl<L: MatchPat, R: MatchPat> MatchPat for IntBinaryFixed<L, R> {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let l = self.lhs.compile(b);
        let r = self.rhs.compile(b);
        b.binary(self.op, l, r)
    }
}

/// A runtime-variant integer binary op `int_binary(op, l, r)`.
pub struct IntBinary<L, R> {
    op: IntBinaryOp,
    lhs: L,
    rhs: R,
}

impl<L: MatchPat, R: MatchPat> MatchPat for IntBinary<L, R> {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let l = self.lhs.compile(b);
        let r = self.rhs.compile(b);
        b.binary(self.op, l, r)
    }
}

/// Variant-agnostic integer binary op `int_binary(op, l, r)`.
#[must_use]
pub fn int_binary<L: MatchPat, R: MatchPat>(op: IntBinaryOp, lhs: L, rhs: R) -> IntBinary<L, R> {
    IntBinary { op, lhs, rhs }
}

/// Match **any** `IntBinaryOp` variant. Match-only.
pub struct IntBinaryAny<L, R> {
    lhs: L,
    rhs: R,
}

impl<L: MatchPat, R: MatchPat> MatchPat for IntBinaryAny<L, R> {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let exemplar = NodeKind::IntBinaryOp(IntBinaryOp::Add);
        let n = b.node(KindSpec::Variant(std::mem::discriminant(&exemplar)));
        let l = self.lhs.compile(b);
        let r = self.rhs.compile(b);
        b.input(n, 0, l);
        b.input(n, 1, r);
        b.value_output(n, 0)
    }
}

/// Match any `IntBinaryOp` regardless of variant.
#[must_use]
pub fn int_binary_any<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> IntBinaryAny<L, R> {
    IntBinaryAny { lhs, rhs }
}

macro_rules! int_binary_op {
    ($name:ident, $variant:ident, $doc:literal) => {
        #[doc = $doc]
        #[must_use]
        pub fn $name<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> IntBinaryFixed<L, R> {
            IntBinaryFixed {
                op: IntBinaryOp::$variant,
                lhs,
                rhs,
            }
        }
    };
}

int_binary_op!(add, Add, "Match unsigned addition `lhs + rhs`. Commutative.");
int_binary_op!(mul, Mul, "Match wrapping multiplication `lhs * rhs`. Commutative.");
int_binary_op!(div, Div, "Match unsigned division `lhs / rhs`.");
int_binary_op!(sdiv, Sdiv, "Match signed division `(signed)lhs / (signed)rhs`.");
int_binary_op!(rem, Rem, "Match unsigned remainder `lhs % rhs`.");
int_binary_op!(srem, Srem, "Match signed remainder `(signed)lhs % (signed)rhs`.");
int_binary_op!(and, And, "Match bitwise AND `lhs & rhs`. Commutative.");
int_binary_op!(or, Or, "Match bitwise OR `lhs | rhs`. Commutative.");
int_binary_op!(xor, Xor, "Match bitwise XOR `lhs ^ rhs`. Commutative.");
int_binary_op!(shl, ShiftLeft, "Match logical left-shift `lhs << rhs`.");
int_binary_op!(shr, ShiftRight, "Match logical right-shift `lhs >> rhs`.");
int_binary_op!(sshr, SShiftRight, "Match arithmetic right-shift `(signed)lhs >> rhs`.");

/// Match a subtraction `lhs - rhs`, lowered to `add(lhs, neg(rhs))`.
pub struct Sub<L, R> {
    lhs: L,
    rhs: R,
}

impl<L: MatchPat, R: MatchPat> MatchPat for Sub<L, R> {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let l = self.lhs.compile(b);
        let neg_rhs = {
            let r = self.rhs.compile(b);
            b.unary(KindSpec::Exact(NodeKind::IntUnaryOp(IntUnaryOp::Neg)), r)
        };
        b.binary(IntBinaryOp::Add, l, neg_rhs)
    }
}

/// Match `lhs - rhs` (the lifter's `Add(lhs, Neg(rhs))` shape).
#[must_use]
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
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let i = self.inner.compile(b);
        b.unary(KindSpec::Exact(self.kind), i)
    }
}

macro_rules! int_unary_op {
    ($name:ident, $kind:expr, $doc:literal) => {
        #[doc = $doc]
        #[must_use]
        pub fn $name<I: MatchPat>(inner: I) -> IntUnaryFixed<I> {
            IntUnaryFixed { kind: $kind, inner }
        }
    };
}

int_unary_op!(neg, NodeKind::IntUnaryOp(IntUnaryOp::Neg), "Match two's-complement negation `-inner`.");
int_unary_op!(popcount, NodeKind::Popcount, "Match a `Popcount(inner)` (count-set-bits) node.");
int_unary_op!(lzcount, NodeKind::Lzcount, "Match an `Lzcount(inner)` (leading-zero-count) node.");

/// Match **any** `IntUnaryOp` variant. Match-only.
pub struct IntUnaryAny<I> {
    inner: I,
}

impl<I: MatchPat> MatchPat for IntUnaryAny<I> {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let exemplar = NodeKind::IntUnaryOp(IntUnaryOp::Neg);
        let i = self.inner.compile(b);
        b.unary(KindSpec::Variant(std::mem::discriminant(&exemplar)), i)
    }
}

/// Match any `IntUnaryOp` regardless of variant.
#[must_use]
pub fn int_unary_any<I: MatchPat>(inner: I) -> IntUnaryAny<I> {
    IntUnaryAny { inner }
}

/// Match a bitwise complement `~inner` — `xor(inner, all_ones)`.
pub struct BitNot<I> {
    inner: I,
}

impl<I: MatchPat> MatchPat for BitNot<I> {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let i = self.inner.compile(b);
        let ones = int_const_all_ones_pre(b);
        let ones_out = ones.compile(b);
        b.binary(IntBinaryOp::Xor, i, ones_out)
    }
}

/// Match a bitwise complement `~inner` (the canonical `xor(_, all_ones)`).
#[must_use]
pub fn bit_not<I: MatchPat>(inner: I) -> BitNot<I> {
    BitNot { inner }
}

/// Alias for [`bit_not`] (matches the Python `not_` keyword-collision name).
#[must_use]
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
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let i = self.inner.compile(b);
        b.unary(KindSpec::Exact(self.kind), i)
    }
}

macro_rules! cast_op {
    ($name:ident, $kind:expr, $doc:literal) => {
        #[doc = $doc]
        #[must_use]
        pub fn $name<I: MatchPat>(inner: I) -> Cast<I> {
            Cast { kind: $kind, inner }
        }
    };
}

cast_op!(truncate, NodeKind::Truncate, "Match a `Truncate(inner)` (integer narrowing) node.");
cast_op!(zero_extend, NodeKind::Extend(ExtendOp::ZeroExtend), "Match a zero-extension `Extend(ZeroExtend)(inner)` node.");
cast_op!(sign_extend, NodeKind::Extend(ExtendOp::SignExtend), "Match a sign-extension `Extend(SignExtend)(inner)` node.");
cast_op!(int_to_float, NodeKind::IntToFloat, "Match an `IntToFloat(inner)` value-conversion.");
cast_op!(float_to_int, NodeKind::FloatToInt, "Match a `FloatToInt(inner)` value-conversion.");
cast_op!(int_bits_to_float, NodeKind::IntBitsToFloat, "Match an `IntBitsToFloat(inner)` bitcast.");
cast_op!(float_bits_to_int, NodeKind::FloatBitsToInt, "Match a `FloatBitsToInt(inner)` bitcast.");
cast_op!(float_to_float, NodeKind::FloatToFloat, "Match a `FloatToFloat(inner)` precision-conversion.");

/// Match an `Extend(op)` node with the given runtime `ExtendOp`.
#[must_use]
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
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let l = self.lhs.compile(b);
        let r = self.rhs.compile(b);
        let o = b.node(KindSpec::Exact(NodeKind::IntCmpOp(self.op)));
        b.input(o, 0, l);
        b.input(o, 1, r);
        let out = b.value_output(o, 0);
        b.set_output_ty(out, NodeOutputType::I1);
        out
    }
}

/// Runtime-variant integer comparison `int_cmp(op, l, r)` (output `I1`).
pub struct IntCmp<L, R> {
    op: IntCmpOp,
    lhs: L,
    rhs: R,
}

impl<L: MatchPat, R: MatchPat> MatchPat for IntCmp<L, R> {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        IntCmpFixed {
            op: self.op,
            lhs: self.lhs,
            rhs: self.rhs,
        }
        .compile(b)
    }
}

/// Variant-agnostic integer comparison `int_cmp(op, l, r)`.
#[must_use]
pub fn int_cmp<L: MatchPat, R: MatchPat>(op: IntCmpOp, lhs: L, rhs: R) -> IntCmp<L, R> {
    IntCmp { op, lhs, rhs }
}

/// Match **any** `IntCmpOp` regardless of variant (output `I1`). Match-only.
pub struct IntCmpAny<L, R> {
    lhs: L,
    rhs: R,
}

impl<L: MatchPat, R: MatchPat> MatchPat for IntCmpAny<L, R> {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let exemplar = NodeKind::IntCmpOp(IntCmpOp::Equal);
        let n = b.node(KindSpec::Variant(std::mem::discriminant(&exemplar)));
        let l = self.lhs.compile(b);
        let r = self.rhs.compile(b);
        b.input(n, 0, l);
        b.input(n, 1, r);
        let out = b.value_output(n, 0);
        b.set_output_ty(out, NodeOutputType::I1);
        out
    }
}

/// Match any `IntCmpOp` regardless of variant.
#[must_use]
pub fn int_cmp_any<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> IntCmpAny<L, R> {
    IntCmpAny { lhs, rhs }
}

macro_rules! int_cmp_op {
    ($name:ident, $variant:ident, $doc:literal) => {
        #[doc = $doc]
        #[must_use]
        pub fn $name<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> IntCmpFixed<L, R> {
            IntCmpFixed {
                op: IntCmpOp::$variant,
                lhs,
                rhs,
            }
        }
    };
}

int_cmp_op!(int_eq, Equal, "Match an unsigned equality `lhs == rhs`. Commutative.");
int_cmp_op!(int_lt, Less, "Match an unsigned less-than `lhs < rhs`.");
int_cmp_op!(int_slt, Sless, "Match a signed less-than `(signed)lhs < (signed)rhs`.");
int_cmp_op!(int_carry, Carry, "Match an unsigned addition carry-out. Commutative.");
int_cmp_op!(int_scarry, Scarry, "Match a signed addition overflow. Commutative.");
int_cmp_op!(int_sborrow, Sborrow, "Match a signed subtraction borrow.");

/// Match an unsigned not-equal `lhs != rhs` — `xor(int_eq(l, r), 1):I1`.
#[must_use]
pub fn int_ne<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> impl MatchPat {
    xor(int_eq(lhs, rhs), bool_one())
}

/// Match an unsigned less-or-equal `lhs <= rhs` — `xor(int_lt(r, l), 1):I1`.
#[must_use]
pub fn int_le<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> impl MatchPat {
    xor(int_lt(rhs, lhs), bool_one())
}

/// Match a signed less-or-equal `(signed)lhs <= (signed)rhs` —
/// `xor(int_slt(r, l), 1):I1`.
#[must_use]
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
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let n = b.node(KindSpec::Exact(NodeKind::FloatBinaryOp(self.op)));
        let l = self.lhs.compile(b);
        let r = self.rhs.compile(b);
        b.input(n, 0, l);
        b.input(n, 1, r);
        b.value_output(n, 0)
    }
}

/// Variant-agnostic float binary op `float_binary(op, l, r)`.
pub struct FloatBinary<L, R> {
    op: FloatBinaryOp,
    lhs: L,
    rhs: R,
}

impl<L: MatchPat, R: MatchPat> MatchPat for FloatBinary<L, R> {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        FloatBinaryFixed {
            op: self.op,
            lhs: self.lhs,
            rhs: self.rhs,
        }
        .compile(b)
    }
}

/// Variant-agnostic float binary op.
#[must_use]
pub fn float_binary<L: MatchPat, R: MatchPat>(
    op: FloatBinaryOp,
    lhs: L,
    rhs: R,
) -> FloatBinary<L, R> {
    FloatBinary { op, lhs, rhs }
}

/// Match **any** `FloatBinaryOp` variant. Match-only.
pub struct FloatBinaryAny<L, R> {
    lhs: L,
    rhs: R,
}

impl<L: MatchPat, R: MatchPat> MatchPat for FloatBinaryAny<L, R> {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let exemplar = NodeKind::FloatBinaryOp(FloatBinaryOp::Add);
        let n = b.node(KindSpec::Variant(std::mem::discriminant(&exemplar)));
        let l = self.lhs.compile(b);
        let r = self.rhs.compile(b);
        b.input(n, 0, l);
        b.input(n, 1, r);
        b.value_output(n, 0)
    }
}

/// Match any `FloatBinaryOp` regardless of variant.
#[must_use]
pub fn float_binary_any<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> FloatBinaryAny<L, R> {
    FloatBinaryAny { lhs, rhs }
}

macro_rules! float_binary_op {
    ($name:ident, $variant:ident, $doc:literal) => {
        #[doc = $doc]
        #[must_use]
        pub fn $name<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> FloatBinaryFixed<L, R> {
            FloatBinaryFixed {
                op: FloatBinaryOp::$variant,
                lhs,
                rhs,
            }
        }
    };
}

float_binary_op!(float_add, Add, "Match a float addition `lhs + rhs`. Commutative.");
float_binary_op!(float_mul, Mul, "Match a float multiplication `lhs * rhs`. Commutative.");
float_binary_op!(float_div, Div, "Match a float division `lhs / rhs`.");

/// Match a float subtraction `lhs - rhs`, lowered to
/// `float_add(lhs, float_neg(rhs))`.
pub struct FloatSub<L, R> {
    lhs: L,
    rhs: R,
}

impl<L: MatchPat, R: MatchPat> MatchPat for FloatSub<L, R> {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let neg_rhs = FloatUnaryFixed {
            op: FloatUnaryOp::Neg,
            inner: self.rhs,
        };
        FloatBinaryFixed {
            op: FloatBinaryOp::Add,
            lhs: self.lhs,
            rhs: neg_rhs,
        }
        .compile(b)
    }
}

/// Match a float subtraction `lhs - rhs`.
#[must_use]
pub fn float_sub<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> FloatSub<L, R> {
    FloatSub { lhs, rhs }
}

/// A fixed-variant float unary op.
pub struct FloatUnaryFixed<I> {
    op: FloatUnaryOp,
    inner: I,
}

impl<I: MatchPat> MatchPat for FloatUnaryFixed<I> {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let i = self.inner.compile(b);
        b.unary(KindSpec::Exact(NodeKind::FloatUnaryOp(self.op)), i)
    }
}

macro_rules! float_unary_op {
    ($name:ident, $variant:ident, $doc:literal) => {
        #[doc = $doc]
        #[must_use]
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

/// Match **any** `FloatUnaryOp` variant. Match-only.
pub struct FloatUnaryAny<I> {
    inner: I,
}

impl<I: MatchPat> MatchPat for FloatUnaryAny<I> {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let exemplar = NodeKind::FloatUnaryOp(FloatUnaryOp::Neg);
        let i = self.inner.compile(b);
        b.unary(KindSpec::Variant(std::mem::discriminant(&exemplar)), i)
    }
}

/// Match any `FloatUnaryOp` regardless of variant.
#[must_use]
pub fn float_unary_any<I: MatchPat>(inner: I) -> FloatUnaryAny<I> {
    FloatUnaryAny { inner }
}

/// A fixed-variant float comparison `lhs ∘ rhs` (output `I1`).
pub struct FloatCmpFixed<L, R> {
    op: FloatCmpOp,
    lhs: L,
    rhs: R,
}

impl<L: MatchPat, R: MatchPat> MatchPat for FloatCmpFixed<L, R> {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let n = b.node(KindSpec::Exact(NodeKind::FloatCmpOp(self.op)));
        let l = self.lhs.compile(b);
        let r = self.rhs.compile(b);
        b.input(n, 0, l);
        b.input(n, 1, r);
        let out = b.value_output(n, 0);
        b.set_output_ty(out, NodeOutputType::I1);
        out
    }
}

/// Runtime-variant float comparison `float_cmp(op, l, r)` (output `I1`).
pub struct FloatCmp<L, R> {
    op: FloatCmpOp,
    lhs: L,
    rhs: R,
}

impl<L: MatchPat, R: MatchPat> MatchPat for FloatCmp<L, R> {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        FloatCmpFixed {
            op: self.op,
            lhs: self.lhs,
            rhs: self.rhs,
        }
        .compile(b)
    }
}

/// Variant-agnostic float comparison.
#[must_use]
pub fn float_cmp<L: MatchPat, R: MatchPat>(op: FloatCmpOp, lhs: L, rhs: R) -> FloatCmp<L, R> {
    FloatCmp { op, lhs, rhs }
}

/// Match **any** `FloatCmpOp` regardless of variant (output `I1`). Match-only.
pub struct FloatCmpAny<L, R> {
    lhs: L,
    rhs: R,
}

impl<L: MatchPat, R: MatchPat> MatchPat for FloatCmpAny<L, R> {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let exemplar = NodeKind::FloatCmpOp(FloatCmpOp::Equal);
        let n = b.node(KindSpec::Variant(std::mem::discriminant(&exemplar)));
        let l = self.lhs.compile(b);
        let r = self.rhs.compile(b);
        b.input(n, 0, l);
        b.input(n, 1, r);
        let out = b.value_output(n, 0);
        b.set_output_ty(out, NodeOutputType::I1);
        out
    }
}

/// Match any `FloatCmpOp` regardless of variant.
#[must_use]
pub fn float_cmp_any<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> FloatCmpAny<L, R> {
    FloatCmpAny { lhs, rhs }
}

macro_rules! float_cmp_op {
    ($name:ident, $variant:ident, $doc:literal) => {
        #[doc = $doc]
        #[must_use]
        pub fn $name<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> FloatCmpFixed<L, R> {
            FloatCmpFixed {
                op: FloatCmpOp::$variant,
                lhs,
                rhs,
            }
        }
    };
}

float_cmp_op!(float_eq, Equal, "Match a float equality `lhs == rhs`. Commutative.");
float_cmp_op!(float_lt, Less, "Match a float less-than `lhs < rhs`.");

/// Match a float not-equal `lhs != rhs` — `xor(float_eq(l, r), 1):I1`.
#[must_use]
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
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
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
#[must_use]
pub fn float_le<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> FloatLe<L, R> {
    FloatLe { lhs, rhs }
}

/// Match `float_is_nan(x)` — `xor(float_eq(x, x), 1):I1`. The operand is
/// compiled once and shared across both equality inputs.
pub struct FloatIsNan<I> {
    inner: I,
}

impl<I: MatchPat> MatchPat for FloatIsNan<I> {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let x = self.inner.compile(b);
        let eq = FloatCmpFixed {
            op: FloatCmpOp::Equal,
            lhs: Pre(x),
            rhs: Pre(x),
        }
        .compile(b);
        let one = bool_one_out(b);
        bool_binary_out(b, IntBinaryOp::Xor, eq, one)
    }
}

/// Match `float_is_nan(x)`.
#[must_use]
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
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let l = self.lhs.compile(b);
        let r = self.rhs.compile(b);
        bool_binary_out(b, self.op, l, r)
    }
}

/// Runtime-variant boolean binary op `bool_binary(op, l, r)` (output `I1`).
pub struct BoolBinary<L, R> {
    op: IntBinaryOp,
    lhs: L,
    rhs: R,
}

impl<L: MatchPat, R: MatchPat> MatchPat for BoolBinary<L, R> {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let l = self.lhs.compile(b);
        let r = self.rhs.compile(b);
        bool_binary_out(b, self.op, l, r)
    }
}

/// Variant-agnostic boolean binary op (`IntBinaryOp` at `I1`).
#[must_use]
pub fn bool_binary<L: MatchPat, R: MatchPat>(op: IntBinaryOp, lhs: L, rhs: R) -> BoolBinary<L, R> {
    BoolBinary { op, lhs, rhs }
}

/// Match **any** `IntBinaryOp` at `I1` regardless of variant. Match-only.
pub struct BoolBinaryAny<L, R> {
    lhs: L,
    rhs: R,
}

impl<L: MatchPat, R: MatchPat> MatchPat for BoolBinaryAny<L, R> {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let exemplar = NodeKind::IntBinaryOp(IntBinaryOp::And);
        let n = b.node(KindSpec::Variant(std::mem::discriminant(&exemplar)));
        let l = self.lhs.compile(b);
        let r = self.rhs.compile(b);
        b.input(n, 0, l);
        b.input(n, 1, r);
        let out = b.value_output(n, 0);
        b.set_output_ty(out, NodeOutputType::I1);
        out
    }
}

/// Match any `IntBinaryOp` at `I1` regardless of variant.
#[must_use]
pub fn bool_bin_any<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> BoolBinaryAny<L, R> {
    BoolBinaryAny { lhs, rhs }
}

macro_rules! bool_op {
    ($name:ident, $variant:ident, $doc:literal) => {
        #[doc = $doc]
        #[must_use]
        pub fn $name<L: MatchPat, R: MatchPat>(lhs: L, rhs: R) -> BoolBinaryFixed<L, R> {
            BoolBinaryFixed {
                op: IntBinaryOp::$variant,
                lhs,
                rhs,
            }
        }
    };
}

bool_op!(bool_and, And, "Match a boolean AND (`IntBinaryOp::And` at `I1`). Commutative.");
bool_op!(bool_or, Or, "Match a boolean OR (`IntBinaryOp::Or` at `I1`). Commutative.");
bool_op!(bool_xor, Xor, "Match a boolean XOR (`IntBinaryOp::Xor` at `I1`). Commutative.");

/// Match a boolean NOT `~operand` — `xor(operand, 1):I1`.
pub struct BoolNot<I> {
    inner: I,
}

impl<I: MatchPat> MatchPat for BoolNot<I> {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let i = self.inner.compile(b);
        let one = bool_one_out(b);
        bool_binary_out(b, IntBinaryOp::Xor, i, one)
    }
}

/// Match a boolean NOT — `xor(operand, IntConst(1)):I1`.
#[must_use]
pub fn bool_not<I: MatchPat>(operand: I) -> BoolNot<I> {
    BoolNot { inner: operand }
}

// ── Shared helpers ────────────────────────────────────────────────────

/// Wire a binary `IntBinaryOp` consuming `l` / `r`, pinning the output to
/// `I1` (the boolean shape).
fn bool_binary_out(
    b: &mut MatcherBuilder,
    op: IntBinaryOp,
    l: PatOutRef,
    r: PatOutRef,
) -> PatOutRef {
    let out = b.binary(op, l, r);
    b.set_output_ty(out, NodeOutputType::I1);
    out
}

/// A `bool_const(true)` operand handle (the `IntConst(1):I1` all-ones at
/// `I1`), compiled into `b`.
fn bool_one_out(b: &mut MatcherBuilder) -> PatOutRef {
    let out = b.leaf(KindSpec::Exact(NodeKind::IntConst(1)));
    b.set_output_ty(out, NodeOutputType::I1);
    out
}

/// A `bool_one` operand re-presented as a [`MatchPat`].
fn bool_one() -> impl MatchPat {
    crate::typed::consts::bool_const(true)
}
