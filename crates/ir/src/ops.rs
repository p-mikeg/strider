/// Binary operations on boolean (`Bool`) values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BoolBinaryOp {
    /// Logical exclusive-or: `a ^ b`.
    Xor,
    /// Logical and: `a & b`.
    And,
    /// Logical or: `a | b`.
    Or
}

/// Unary operations on boolean (`Bool`) values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BoolUnaryOp {
    /// Logical negation: `!a`.
    Neg
}

/// How to fill the bits produced by a widening integer extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExtendOp {
    /// Fill new high bits with zero.
   ZeroExtend,
   /// Replicate the sign bit into all new high bits.
   SignExtend
}

/// Comparison operations that produce a `Bool` from two integer operands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntCmpOp {
    /// Unsigned equality: `l == r`.
    Equal,
    /// Signed less-than: `(signed)l < (signed)r`.
    Sless,
    /// Signed less-than-or-equal: `(signed)l <= (signed)r`.
    SlessEqual,
    /// Unsigned less-than: `l < r`.
    Less,
    /// Unsigned less-than-or-equal: `l <= r`.
    LessEqual,
    /// Unsigned carry: the addition `l + r` overflows the type's width.
    Carry,
    /// Signed carry (overflow): the addition `l + r` overflows the signed range.
    Scarry,
    /// Unsigned borrow: `l < r` (subtraction `l - r` would borrow).
    Borrow,
    /// Signed borrow (overflow): the subtraction `l - r` overflows the signed range.
    Sborrow,
}

/// Binary arithmetic and bitwise operations on integer values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntBinaryOp {
    /// Wrapping addition: `l + r`.
    Add,
    /// Wrapping subtraction: `l - r`.
    Sub,
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
    Mul
}

/// Unary arithmetic and bitwise operations on integer values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntUnaryOp {
    /// Two's-complement negation: `-x`.
    Neg,
    /// Bitwise complement: `~x`.
    Not,
}

/// Binary arithmetic operations on floating-point values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FloatBinaryOp {
    /// Floating-point addition: `l + r`.
    Add,
    /// Floating-point subtraction: `l - r`.
    Sub,
    /// Floating-point multiplication: `l * r`.
    Mul,
    /// Floating-point division: `l / r`.
    Div,
}

/// Unary operations on floating-point values that produce a float result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

/// Comparison operations that produce a `Bool` from two floating-point operands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FloatCmpOp {
    /// IEEE 754 equality: `l == r` (false if either is NaN).
    Equal,
    /// IEEE 754 inequality: `l != r` (true if either is NaN).
    NotEqual,
    /// IEEE 754 less-than: `l < r` (false if either is NaN).
    Less,
    /// IEEE 754 less-than-or-equal: `l <= r` (false if either is NaN).
    LessEqual,
}
