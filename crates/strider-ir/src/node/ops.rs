/// How to fill the bits produced by a widening integer extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExtendOp {
    /// Fill new high bits with zero.
    ZeroExtend,
    /// Replicate the sign bit into all new high bits.
    SignExtend,
}

/// Comparison operations that produce an `I1` from two integer operands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IntCmpOp {
    /// Unsigned equality: `l == r`.
    Equal,
    /// Signed less-than: `(signed)l < (signed)r`.
    Sless,
    /// Unsigned less-than: `l < r`.  Also represents the unsigned borrow
    /// predicate `l - r < 0`: rsleigh's `IntLess` opcode is documented as
    /// "also indicates a borrow on unsigned subtraction" (see
    /// `rsleigh/src/ffi.rs` `IntLess = 15`), and an unsigned subtraction
    /// borrows iff the minuend is less than the subtrahend.  There is no
    /// separate `Borrow` variant.
    ///
    /// `LessEqual` and `SlessEqual` are not separate variants: the
    /// pcode-lift dispatch lowers them at lift time to
    /// `Xor(Less(b, a), IntConst(1)):I1` and `Xor(Sless(b, a), IntConst(1)):I1`
    /// respectively.  Patterns and passes see the lowered shape directly.
    Less,
    /// Unsigned carry: the addition `l + r` overflows the type's width.
    Carry,
    /// Signed carry (overflow): the addition `l + r` overflows the signed range.
    Scarry,
    /// Signed borrow (overflow): the subtraction `l - r` overflows the signed range.
    Sborrow,
}

/// Binary arithmetic and bitwise operations on integer values.
///
/// `Sub` is intentionally absent: the pcode-lift dispatch lowers
/// `IntSub(a, b)` at lift time to `Add(a, IntUnaryOp::Neg(b))`.  Patterns
/// and passes see one canonical form for subtraction; the
/// `pattern::sub(a, b)` ergonomic alias produces the lowered shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IntBinaryOp {
    /// Wrapping addition: `l + r`.
    Add,
    /// Bitwise and: `l & r`.
    And,
    /// Bitwise or: `l | r`.
    Or,
    /// Bitwise exclusive-or: `l ^ r`.
    Xor,
    /// Unsigned division: `l / r`.
    Div,
    /// Signed division: `(signed)l / (signed)r`.
    Sdiv,
    /// Unsigned remainder: `l % r`.
    Rem,
    /// Signed remainder: `(signed)l % (signed)r`.
    Srem,
    /// Logical (unsigned) right shift: `l >> r`.
    ShiftRight,
    /// Arithmetic (signed) right shift: `(signed)l >> r`.
    SShiftRight,
    /// Left shift: `l << r`.
    ShiftLeft,
    /// Wrapping multiplication: `l * r`.
    Mul,
}

/// Unary arithmetic operations on integer values.
///
/// **Naming note:** rsleigh's Sleigh-derived opcode names use the opposite
/// convention from conventional Rust nomenclature.  Sleigh's `IntNeg`
/// opcode is *bitwise* complement (`~x`) and Sleigh's `Int2Comp` is
/// two's-complement negation (`-x`).  The IR's `Neg` follows the
/// conventional meaning (`-x`, from rsleigh's `Int2Comp`); bitwise
/// complement is no longer a dedicated unary op — it is lowered at lift
/// time to `Xor(x, all_ones)` (since `x ^ all_ones ≡ ~x`).  See the
/// lifter's (`strider_lift::lift`) dispatch site for the rsleigh → IR mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IntUnaryOp {
    /// Two's-complement negation: `-x` (`!x + 1`).  Lifted from rsleigh's
    /// `Int2Comp` opcode.
    Neg,
}

/// Binary arithmetic operations on floating-point values.
///
/// `Sub` is not a primitive: pcode-lift lowers `FloatSub(a, b)` at lift
/// time to `FloatAdd(a, FloatUnaryOp::Neg(b))`.  IEEE 754 guarantees the
/// bit-pattern result matches `FloatSub` exactly (negation flips the sign
/// bit on all values, including NaN/inf).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FloatBinaryOp {
    /// Floating-point addition: `l + r`.
    Add,
    /// Floating-point multiplication: `l * r`.
    Mul,
    /// Floating-point division: `l / r`.
    Div,
}

/// Unary operations on floating-point values that produce a float result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FloatUnaryOp {
    /// Floating-point negation: `-x`.
    Neg,
    /// Absolute value: `|x|`.
    Abs,
    /// Square root: `√x`.
    Sqrt,
    /// Round toward positive infinity: `⌈x⌉`.
    Ceil,
    /// Round toward negative infinity: `⌊x⌋`.
    Floor,
    /// Round to nearest integer (ties to even): `round(x)`.
    Round,
}

/// Comparison operations that produce an `I1` from two floating-point operands.
///
/// `NotEqual` and `LessEqual` are not primitives: pcode-lift lowers them
/// at lift time:
///
/// - `FloatNotEqual(a, b)` → `Xor(FloatEqual(a, b), IntConst(1)):I1` (sound
///   under IEEE 754: `Equal` is false on NaN, so the I1 xor with 1 is true).
/// - `FloatLessEqual(a, b)` → `Or(FloatLess(a, b), FloatEqual(a, b))`
///   (NaN-aware: cannot use `Xor(Less(b, a), 1)` because that would
///   return true for NaN, while IEEE `<=` returns false).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FloatCmpOp {
    /// IEEE 754 equality: `l == r` (false if either is NaN).
    Equal,
    /// IEEE 754 less-than: `l < r` (false if either is NaN).
    Less,
}
