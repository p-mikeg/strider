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

/// Declare public two-operand constructors that delegate to a private
/// `$builder(OpEnum::$variant, l, r)` helper.
///
/// Each entry is `(pub_fn_name, OpEnumVariant)` and may carry arbitrary
/// outer attributes (doc comments, `#[inline]`, etc.).
macro_rules! decl_binary_ops {
    ($builder:ident, $op_enum:ident, [ $( $(#[$attr:meta])* ($fn_name:ident, $variant:ident) ),* $(,)? ]) => {
        $(
            $(#[$attr])*
            pub fn $fn_name(l: Build, r: Build) -> Build {
                $builder($op_enum::$variant, l, r)
            }
        )*
    };
}

/// Declare public one-operand constructors that delegate to a private
/// `$builder(OpEnum::$variant, x)` helper.
macro_rules! decl_unary_ops {
    ($builder:ident, $op_enum:ident, [ $( $(#[$attr:meta])* ($fn_name:ident, $variant:ident) ),* $(,)? ]) => {
        $(
            $(#[$attr])*
            pub fn $fn_name(x: Build) -> Build {
                $builder($op_enum::$variant, x)
            }
        )*
    };
}

/// Declare public comparison constructors.  Identical in shape to
/// [`decl_binary_ops`] but kept as a distinct macro so the call-site reads
/// as a cmp-op group rather than a binary-op group.
macro_rules! decl_cmp_ops {
    ($builder:ident, $op_enum:ident, [ $( $(#[$attr:meta])* ($fn_name:ident, $variant:ident) ),* $(,)? ]) => {
        $(
            $(#[$attr])*
            pub fn $fn_name(l: Build, r: Build) -> Build {
                $builder($op_enum::$variant, l, r)
            }
        )*
    };
}

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

decl_binary_ops!(int_binary, IntBinaryOp, [
    /// Build `l + r`.
    (add, Add),
    /// Build `l - r`.
    (sub, Sub),
    /// Build `l * r`.
    (mul, Mul),
    /// Build `l & r`.
    (and, And),
    /// Build `l | r`.
    (or, Or),
    /// Build `l ^ r`.
    (xor, Xor),
    /// Build `l << r`.
    (shl, ShiftLeft),
    /// Build `l >> r` (logical / unsigned).
    (shr, ShiftRight),
    /// Build `l >>> r` (arithmetic / signed).
    (sshr, SShiftRight),
    /// Build `l / r` (unsigned).
    (div, Div),
    /// Build `l / r` (signed).
    (sdiv, Sdiv),
    /// Build `l % r` (unsigned).
    (rem, Rem),
    /// Build `l % r` (signed).
    (srem, Srem),
]);

// Integer unary ops
fn int_unary(op: IntUnaryOp, x: Build) -> Build {
    Build::IntUnary(op, Arc::new(x))
}

decl_unary_ops!(int_unary, IntUnaryOp, [
    /// Build `-x`.
    (neg, Neg),
    /// Build `!x` (bitwise complement).
    (not, Not),
]);

// Integer cmp ops (→ Bool)
fn int_cmp(op: IntCmpOp, l: Build, r: Build) -> Build {
    Build::IntCmp(op, Arc::new(l), Arc::new(r))
}

decl_cmp_ops!(int_cmp, IntCmpOp, [
    /// Build `l == r` (integer equality).
    (int_eq, Equal),
    /// Build `l < r` (unsigned less-than).
    (int_lt, Less),
    /// Build `l < r` (signed less-than).
    (int_slt, Sless),
    /// Build `l <= r` (unsigned less-or-equal).
    (int_le, LessEqual),
    /// Build `l <= r` (signed less-or-equal).
    (int_sle, SlessEqual),
]);

// Bool binary ops
fn bool_binary(op: BoolBinaryOp, l: Build, r: Build) -> Build {
    Build::BoolBinary(op, Arc::new(l), Arc::new(r))
}

decl_binary_ops!(bool_binary, BoolBinaryOp, [
    /// Build `l && r` (boolean and).
    (bool_and, And),
    /// Build `l || r` (boolean or).
    (bool_or, Or),
    /// Build `l ^ r` (boolean xor).
    (bool_xor, Xor),
]);

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

decl_binary_ops!(float_binary, FloatBinaryOp, [
    /// Build float `l + r`.
    (float_add, Add),
    /// Build float `l - r`.
    (float_sub, Sub),
    /// Build float `l * r`.
    (float_mul, Mul),
    /// Build float `l / r`.
    (float_div, Div),
]);

// Float unary ops
fn float_unary(op: FloatUnaryOp, x: Build) -> Build {
    Build::FloatUnary(op, Arc::new(x))
}

decl_unary_ops!(float_unary, FloatUnaryOp, [
    /// Build `-x` (float).
    (float_neg, Neg),
    /// Build `|x|` (float absolute value).
    (float_abs, Abs),
    /// Build `sqrt(x)`.
    (float_sqrt, Sqrt),
    /// Build `round(x)`.
    (float_round, Round),
    /// Build `floor(x)`.
    (float_floor, Floor),
    /// Build `ceil(x)`.
    (float_ceil, Ceil),
]);

// Float cmp ops
fn float_cmp(op: FloatCmpOp, l: Build, r: Build) -> Build {
    Build::FloatCmp(op, Arc::new(l), Arc::new(r))
}

decl_cmp_ops!(float_cmp, FloatCmpOp, [
    /// Build float `l == r`.
    (float_eq, Equal),
    /// Build float `l < r`.
    (float_lt, Less),
    /// Build float `l <= r`.
    (float_le, LessEqual),
    /// Build float `l != r`.
    (float_ne, NotEqual),
]);

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
