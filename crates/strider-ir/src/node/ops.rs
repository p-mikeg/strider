#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExtendOp {
    ZeroExtend,
    SignExtend,
}

/// Every variant outputs `I1`. The lifter lowers `IntLessEqual(a, b)` to
/// `Xor(Less(b, a), IntConst(1)):I1` and `IntSlessEqual` likewise through
/// `Sless`, so both arrive as this set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IntCmpOp {
    Equal,
    /// Signed less-than.
    Sless,
    /// Unsigned less-than, and equally the unsigned-subtraction borrow
    /// predicate (`l - r` borrows iff `l < r`), matching rsleigh's `IntLess`.
    Less,
    /// `l + r` overflows the unsigned range.
    Carry,
    /// `l + r` overflows the signed range.
    Scarry,
    /// `l - r` overflows the signed range.
    Sborrow,
}

/// Arithmetic wraps. `IntSub(a, b)` arrives lowered to `Add(a, Neg(b))`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IntBinaryOp {
    Add,
    And,
    Or,
    Xor,
    /// Unsigned division.
    Div,
    /// Signed division.
    Sdiv,
    /// Unsigned remainder.
    Rem,
    /// Signed remainder.
    Srem,
    /// Logical right shift.
    ShiftRight,
    /// Arithmetic right shift.
    SShiftRight,
    ShiftLeft,
    Mul,
}

/// `Neg` is two's-complement negation `-x`, not bitwise complement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IntUnaryOp {
    Neg,
}

/// `FloatSub(a, b)` arrives lowered to `Add(a, Neg(b))`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FloatBinaryOp {
    Add,
    Mul,
    Div,
}

/// Float in, float out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FloatUnaryOp {
    Neg,
    Abs,
    Sqrt,
    /// Round toward positive infinity.
    Ceil,
    /// Round toward negative infinity.
    Floor,
    /// Round to nearest, ties to even.
    Round,
}

/// Every variant outputs `I1`. The other two comparisons arrive lowered:
///
/// - `FloatNotEqual(a, b)` -> `Xor(FloatEqual(a, b), IntConst(1)):I1`.
/// - `FloatLessEqual(a, b)` -> `Or(FloatLess(a, b), FloatEqual(a, b))`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FloatCmpOp {
    /// False if either operand is NaN.
    Equal,
    /// False if either operand is NaN.
    Less,
}
