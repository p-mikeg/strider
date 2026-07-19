#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExtendOp {
    ZeroExtend,
    SignExtend,
}

/// Every variant outputs `I1`.
///
/// `LessEqual` / `SlessEqual` are absent: the lifter lowers them to
/// `Xor(Less(b, a), IntConst(1)):I1` and `Xor(Sless(b, a), IntConst(1)):I1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IntCmpOp {
    Equal,
    /// Signed less-than.
    Sless,
    /// Unsigned less-than, and equally the unsigned-subtraction borrow
    /// predicate (`l - r` borrows iff `l < r`), matching rsleigh's `IntLess`.
    /// There is no separate `Borrow` variant.
    Less,
    /// `l + r` overflows the unsigned range.
    Carry,
    /// `l + r` overflows the signed range.
    Scarry,
    /// `l - r` overflows the signed range.
    Sborrow,
}

/// Arithmetic wraps. `Sub` is absent: the lifter lowers `IntSub(a, b)` to
/// `Add(a, Neg(b))`, so subtraction has one canonical shape.
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

/// Naming trap: Sleigh inverts the usual convention. Its `IntNeg` is bitwise
/// complement and its `Int2Comp` is arithmetic negation. The IR's `Neg` means
/// `-x` and comes from `Int2Comp`; bitwise complement has no unary op at all,
/// the lifter emits `Xor(x, all_ones)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IntUnaryOp {
    Neg,
}

/// `Sub` is absent: the lifter lowers `FloatSub(a, b)` to `Add(a, Neg(b))`.
/// Bit-exact under IEEE 754, since negation only flips the sign bit (NaN and
/// inf included).
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

/// Every variant outputs `I1`. `NotEqual` / `LessEqual` are absent, the lifter
/// lowers them:
///
/// - `FloatNotEqual(a, b)` -> `Xor(FloatEqual(a, b), IntConst(1)):I1`.
/// - `FloatLessEqual(a, b)` -> `Or(FloatLess(a, b), FloatEqual(a, b))`. The
///   `Xor(Less(b, a), 1)` form would be wrong: it yields true on NaN, where
///   IEEE `<=` is false.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FloatCmpOp {
    /// False if either operand is NaN.
    Equal,
    /// False if either operand is NaN.
    Less,
}
