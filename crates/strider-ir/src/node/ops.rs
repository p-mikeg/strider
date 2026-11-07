// Derives `ALL` from the variant list.
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
    /// Outputs `I1`. `a <= b` arrives as `Xor(Less(b, a), 1)`, signed likewise.
    pub enum IntCmpOp {
        Equal,
        /// Signed less-than.
        Sless,
        /// Unsigned less-than, which is also the `l - r` borrow.
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
    /// Wrapping. Subtraction arrives as `Add(a, Neg(b))`.
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
    /// `Neg` is two's-complement `-x`, not complement.
    pub enum IntUnaryOp {
        Neg,
    }
}

op_enum! {
    /// Subtraction arrives as `Add(a, Neg(b))`.
    pub enum FloatBinaryOp {
        Add,
        Mul,
        Div,
    }
}

op_enum! {
    pub enum FloatUnaryOp {
        Neg,
        Abs,
        Sqrt,
        /// Round toward positive infinity.
        Ceil,
        /// Round toward negative infinity.
        Floor,
        /// Round to nearest, ties away from zero (GHIDRA `opRound`).
        Round,
    }
}

op_enum! {
    /// Outputs `I1`. `FLOAT_NAN(x)` arrives as `Xor(FloatEqual(x, x), 1)`, so
    /// `FloatEqual(x, x)` is a NaN test, not a tautology.
    pub enum FloatCmpOp {
        /// False if either operand is NaN.
        Equal,
        /// False if either operand is NaN.
        Less,
    }
}
