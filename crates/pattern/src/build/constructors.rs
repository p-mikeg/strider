//! Ergonomic constructors for the [`super::Build`] enum.
//!
//! Each `pub fn` produces a [`super::Build`] value for the corresponding IR
//! node kind.  The tiny `int_binary` / `int_unary` / `int_cmp` /
//! `bool_binary` / `float_binary` / `float_unary` / `float_cmp` helpers at
//! the head of each section are private and exist only to avoid repeating
//! the `Build::...(op, Arc::new(l), Arc::new(r))` boilerplate in every
//! public shorthand.

use std::sync::Arc;

use ir::{
    BoolBinaryOp, BoolUnaryOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp,
    IntUnaryOp,
};

use crate::error::Result;
use crate::var::{
    BoolBinaryOpVar, BoolUnaryOpVar, FloatBinaryOpVar, FloatCmpOpVar, FloatUnaryOpVar,
    IntBinaryOpVar, IntCmpOpVar, IntUnaryOpVar, Var,
};

use super::{Build, BuildCtx, BuildValue};

/// Reuse a captured [`Var`] from the LHS match.
pub fn cap(v: Var) -> Build {
    Build::Capture(v)
}

/// Build a fresh `IntConst` node with a compile-time-known value.
pub fn int_const_lit(n: u64) -> Build {
    Build::IntConst(BuildValue::Lit(n))
}

/// Build a fresh `IntConst` node whose value is computed from the match
/// context at rewrite-firing time.
pub fn int_const_fn<F>(f: F) -> Build
where
    F: Fn(&BuildCtx<'_>) -> Result<u64> + Send + Sync + 'static,
{
    Build::IntConst(BuildValue::Computed(Arc::new(f)))
}

/// Build a fresh `BoolConst` node with a literal value.
pub fn bool_const_lit(b: bool) -> Build {
    Build::BoolConst(BuildValue::Lit(b))
}

/// Build a fresh `BoolConst` node whose value is computed at firing time.
pub fn bool_const_fn<F>(f: F) -> Build
where
    F: Fn(&BuildCtx<'_>) -> Result<bool> + Send + Sync + 'static,
{
    Build::BoolConst(BuildValue::Computed(Arc::new(f)))
}

/// Build a fresh `FloatConst` node with a literal bit pattern.
pub fn float_const_lit(bits: u64) -> Build {
    Build::FloatConst(BuildValue::Lit(bits))
}

/// Build a fresh `FloatConst` node whose bit pattern is computed at firing
/// time.
pub fn float_const_fn<F>(f: F) -> Build
where
    F: Fn(&BuildCtx<'_>) -> Result<u64> + Send + Sync + 'static,
{
    Build::FloatConst(BuildValue::Computed(Arc::new(f)))
}

/// Abort the rewrite from inside the RHS tree.
pub fn skip() -> Build {
    Build::Skip
}

// Integer binary ops
fn int_binary(op: IntBinaryOp, l: Build, r: Build) -> Build {
    Build::IntBinary(op, Arc::new(l), Arc::new(r))
}

/// Build `l + r`.
pub fn add(l: Build, r: Build) -> Build {
    int_binary(IntBinaryOp::Add, l, r)
}
/// Build `l - r`.
pub fn sub(l: Build, r: Build) -> Build {
    int_binary(IntBinaryOp::Sub, l, r)
}
/// Build `l * r`.
pub fn mul(l: Build, r: Build) -> Build {
    int_binary(IntBinaryOp::Mul, l, r)
}
/// Build `l & r`.
pub fn and(l: Build, r: Build) -> Build {
    int_binary(IntBinaryOp::And, l, r)
}
/// Build `l | r`.
pub fn or(l: Build, r: Build) -> Build {
    int_binary(IntBinaryOp::Or, l, r)
}
/// Build `l ^ r`.
pub fn xor(l: Build, r: Build) -> Build {
    int_binary(IntBinaryOp::Xor, l, r)
}
/// Build `l << r`.
pub fn shl(l: Build, r: Build) -> Build {
    int_binary(IntBinaryOp::ShiftLeft, l, r)
}
/// Build `l >> r` (logical / unsigned).
pub fn shr(l: Build, r: Build) -> Build {
    int_binary(IntBinaryOp::ShiftRight, l, r)
}
/// Build `l >>> r` (arithmetic / signed).
pub fn sshr(l: Build, r: Build) -> Build {
    int_binary(IntBinaryOp::SShiftRight, l, r)
}
/// Build `l / r` (unsigned).
pub fn div(l: Build, r: Build) -> Build {
    int_binary(IntBinaryOp::Div, l, r)
}
/// Build `l / r` (signed).
pub fn sdiv(l: Build, r: Build) -> Build {
    int_binary(IntBinaryOp::Sdiv, l, r)
}
/// Build `l % r` (unsigned).
pub fn rem(l: Build, r: Build) -> Build {
    int_binary(IntBinaryOp::Rem, l, r)
}
/// Build `l % r` (signed).
pub fn srem(l: Build, r: Build) -> Build {
    int_binary(IntBinaryOp::Srem, l, r)
}

// Integer unary ops
fn int_unary(op: IntUnaryOp, x: Build) -> Build {
    Build::IntUnary(op, Arc::new(x))
}

/// Build `-x`.
pub fn neg(x: Build) -> Build {
    int_unary(IntUnaryOp::Neg, x)
}
/// Build `!x` (bitwise complement).
pub fn not(x: Build) -> Build {
    int_unary(IntUnaryOp::Not, x)
}

// Integer cmp ops (→ Bool)
fn int_cmp(op: IntCmpOp, l: Build, r: Build) -> Build {
    Build::IntCmp(op, Arc::new(l), Arc::new(r))
}

/// Build `l == r` (integer equality).
pub fn int_eq(l: Build, r: Build) -> Build {
    int_cmp(IntCmpOp::Equal, l, r)
}
/// Build `l < r` (unsigned less-than).
pub fn int_lt(l: Build, r: Build) -> Build {
    int_cmp(IntCmpOp::Less, l, r)
}
/// Build `l < r` (signed less-than).
pub fn int_slt(l: Build, r: Build) -> Build {
    int_cmp(IntCmpOp::Sless, l, r)
}
/// Build `l <= r` (unsigned less-or-equal).
pub fn int_le(l: Build, r: Build) -> Build {
    int_cmp(IntCmpOp::LessEqual, l, r)
}
/// Build `l <= r` (signed less-or-equal).
pub fn int_sle(l: Build, r: Build) -> Build {
    int_cmp(IntCmpOp::SlessEqual, l, r)
}

// Bool binary ops
fn bool_binary(op: BoolBinaryOp, l: Build, r: Build) -> Build {
    Build::BoolBinary(op, Arc::new(l), Arc::new(r))
}

/// Build `l && r` (boolean and).
pub fn bool_and(l: Build, r: Build) -> Build {
    bool_binary(BoolBinaryOp::And, l, r)
}
/// Build `l || r` (boolean or).
pub fn bool_or(l: Build, r: Build) -> Build {
    bool_binary(BoolBinaryOp::Or, l, r)
}
/// Build `l ^ r` (boolean xor).
pub fn bool_xor(l: Build, r: Build) -> Build {
    bool_binary(BoolBinaryOp::Xor, l, r)
}

// Bool unary ops
/// Build `!x` (boolean negation).
pub fn bool_neg(x: Build) -> Build {
    Build::BoolUnary(BoolUnaryOp::Neg, Arc::new(x))
}
/// Alias for [`bool_neg`].  Provided because the original task prompt lists
/// both `bool_neg` and `bool_not`; the IR only has a single `Neg` variant.
pub fn bool_not(x: Build) -> Build {
    bool_neg(x)
}

// Float binary ops
fn float_binary(op: FloatBinaryOp, l: Build, r: Build) -> Build {
    Build::FloatBinary(op, Arc::new(l), Arc::new(r))
}

/// Build float `l + r`.
pub fn float_add(l: Build, r: Build) -> Build {
    float_binary(FloatBinaryOp::Add, l, r)
}
/// Build float `l - r`.
pub fn float_sub(l: Build, r: Build) -> Build {
    float_binary(FloatBinaryOp::Sub, l, r)
}
/// Build float `l * r`.
pub fn float_mul(l: Build, r: Build) -> Build {
    float_binary(FloatBinaryOp::Mul, l, r)
}
/// Build float `l / r`.
pub fn float_div(l: Build, r: Build) -> Build {
    float_binary(FloatBinaryOp::Div, l, r)
}

// Float unary ops
fn float_unary(op: FloatUnaryOp, x: Build) -> Build {
    Build::FloatUnary(op, Arc::new(x))
}

/// Build `-x` (float).
pub fn float_neg(x: Build) -> Build {
    float_unary(FloatUnaryOp::Neg, x)
}
/// Build `|x|` (float absolute value).
pub fn float_abs(x: Build) -> Build {
    float_unary(FloatUnaryOp::Abs, x)
}
/// Build `sqrt(x)`.
pub fn float_sqrt(x: Build) -> Build {
    float_unary(FloatUnaryOp::Sqrt, x)
}
/// Build `round(x)`.
pub fn float_round(x: Build) -> Build {
    float_unary(FloatUnaryOp::Round, x)
}
/// Build `floor(x)`.
pub fn float_floor(x: Build) -> Build {
    float_unary(FloatUnaryOp::Floor, x)
}
/// Build `ceil(x)`.
pub fn float_ceil(x: Build) -> Build {
    float_unary(FloatUnaryOp::Ceil, x)
}

// Float cmp ops
fn float_cmp(op: FloatCmpOp, l: Build, r: Build) -> Build {
    Build::FloatCmp(op, Arc::new(l), Arc::new(r))
}

/// Build float `l == r`.
pub fn float_eq(l: Build, r: Build) -> Build {
    float_cmp(FloatCmpOp::Equal, l, r)
}
/// Build float `l < r`.
pub fn float_lt(l: Build, r: Build) -> Build {
    float_cmp(FloatCmpOp::Less, l, r)
}
/// Build float `l <= r`.
pub fn float_le(l: Build, r: Build) -> Build {
    float_cmp(FloatCmpOp::LessEqual, l, r)
}
/// Build float `l != r`.
pub fn float_ne(l: Build, r: Build) -> Build {
    float_cmp(FloatCmpOp::NotEqual, l, r)
}

// Variant-from-var helpers

/// Build an integer binary op whose variant is resolved from a captured
/// [`IntBinaryOpVar`] at firing time.
pub fn int_binary_from_var(v: IntBinaryOpVar, l: Build, r: Build) -> Build {
    Build::IntBinaryFromVar(v, Arc::new(l), Arc::new(r))
}

/// Build an integer unary op whose variant is resolved from a captured
/// [`IntUnaryOpVar`] at firing time.
pub fn int_unary_from_var(v: IntUnaryOpVar, x: Build) -> Build {
    Build::IntUnaryFromVar(v, Arc::new(x))
}

/// Build an integer comparison op whose variant is resolved from a captured
/// [`IntCmpOpVar`] at firing time.  Produces `Bool`.
pub fn int_cmp_from_var(v: IntCmpOpVar, l: Build, r: Build) -> Build {
    Build::IntCmpFromVar(v, Arc::new(l), Arc::new(r))
}

/// Build a boolean binary op whose variant is resolved from a captured
/// [`BoolBinaryOpVar`] at firing time.
pub fn bool_binary_from_var(v: BoolBinaryOpVar, l: Build, r: Build) -> Build {
    Build::BoolBinaryFromVar(v, Arc::new(l), Arc::new(r))
}

/// Build a boolean unary op whose variant is resolved from a captured
/// [`BoolUnaryOpVar`] at firing time.
pub fn bool_unary_from_var(v: BoolUnaryOpVar, x: Build) -> Build {
    Build::BoolUnaryFromVar(v, Arc::new(x))
}

/// Build a float binary op whose variant is resolved from a captured
/// [`FloatBinaryOpVar`] at firing time.
pub fn float_binary_from_var(v: FloatBinaryOpVar, l: Build, r: Build) -> Build {
    Build::FloatBinaryFromVar(v, Arc::new(l), Arc::new(r))
}

/// Build a float unary op whose variant is resolved from a captured
/// [`FloatUnaryOpVar`] at firing time.
pub fn float_unary_from_var(v: FloatUnaryOpVar, x: Build) -> Build {
    Build::FloatUnaryFromVar(v, Arc::new(x))
}

/// Build a float comparison op whose variant is resolved from a captured
/// [`FloatCmpOpVar`] at firing time.  Produces `Bool`.
pub fn float_cmp_from_var(v: FloatCmpOpVar, l: Build, r: Build) -> Build {
    Build::FloatCmpFromVar(v, Arc::new(l), Arc::new(r))
}
