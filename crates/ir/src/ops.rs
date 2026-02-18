#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BoolBinaryOp {
    Xor,
    And,
    Or
}



#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BoolUnaryOp {
    Neg
}


#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExtendOp {
   ZeroExtend,
   SignExtend
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntCmpOp {
    Equal,
    Sless,
    SlessEqual,
    Less,
    LessEqual,
    Carry,
    Scarry,
    Borrow,
    Sborrow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntBinaryOp {
    Add,
    Sub,
    And,
    Or,
    Xor,
    Div,
    Sdiv,
    Rem,
    Srem,
    ShiftRight,
    SShiftRight,
    ShiftLeft,
    Mul
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntUnaryOp {
    Neg,
    Not,
}
