// Each op enum and its `ALL` come from one variant list, so a roster built on
// `ALL` cannot miss a new op.
macro_rules! op_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident { $( $(#[$vmeta:meta])* $variant:ident ),* $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name {
            $( $(#[$vmeta])* $variant, )*
        }

        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: [Self; [$($name::$variant),*].len()] = [$(Self::$variant),*];
        }
    };
}

op_enum! {
    pub enum ExtendOp {
        ZeroExtend,
        SignExtend,
    }
}

op_enum! {
    /// Every variant outputs `I1`. The lifter lowers `IntLessEqual(a, b)` to
    /// `Xor(Less(b, a), IntConst(1)):I1` and `IntSlessEqual` likewise through
    /// `Sless`, so both arrive as this set.
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
}

op_enum! {
    /// Arithmetic wraps. `IntSub(a, b)` arrives lowered to `Add(a, Neg(b))`.
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
}

impl IntBinaryOp {
    /// Operand 1 is a shift count, which p-code leaves any width.
    pub const fn is_shift(self) -> bool {
        match self {
            Self::ShiftRight | Self::SShiftRight | Self::ShiftLeft => true,
            Self::Add
            | Self::And
            | Self::Or
            | Self::Xor
            | Self::Div
            | Self::Sdiv
            | Self::Rem
            | Self::Srem
            | Self::Mul => false,
        }
    }
}

op_enum! {
    /// `Neg` is two's-complement negation `-x`, not bitwise complement.
    pub enum IntUnaryOp {
        Neg,
    }
}

op_enum! {
    /// `FloatSub(a, b)` arrives lowered to `Add(a, Neg(b))`.
    pub enum FloatBinaryOp {
        Add,
        Mul,
        Div,
    }
}

op_enum! {
    /// Float in, float out.
    pub enum FloatUnaryOp {
        Neg,
        Abs,
        Sqrt,
        /// Round toward positive infinity.
        Ceil,
        /// Round toward negative infinity.
        Floor,
        /// Round to nearest, ties AWAY FROM ZERO, matching GHIDRA's
        /// `FloatFormat::opRound`.
        Round,
    }
}

op_enum! {
    /// Every variant outputs `I1`. The remaining float predicates arrive lowered:
    ///
    /// - `FloatNotEqual(a, b)` -> `Xor(FloatEqual(a, b), IntConst(1)):I1`.
    /// - `FloatLessEqual(a, b)` -> `Or(FloatLess(a, b), FloatEqual(a, b))`.
    /// - `FLOAT_NAN(x)` -> `Xor(FloatEqual(x, x), IntConst(1)):I1`, so a
    ///   `FloatEqual(x, x)` in the IR is a NaN test, not a tautology.
    pub enum FloatCmpOp {
        /// False if either operand is NaN.
        Equal,
        /// False if either operand is NaN.
        Less,
    }
}
