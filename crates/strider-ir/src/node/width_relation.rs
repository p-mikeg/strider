//! Where an input slot's width comes from.

use crate::node::NodeKind;
use crate::node::signature::{Conversion, OperandWidth};

/// Where input slot's width comes from, over whatever a caller uses to stand
/// for a value: a vertex id, or the type itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidthTie<T> {
    /// It is this head's width.
    Tied(T),
    /// The rule names a head the graph does not supply.
    NoHead,
    /// Input and output are related by a conversion, not by equality.
    Conversion(Conversion),
    /// Nothing is pinned.
    Free,
}

/// The width input `slot` of `kind` owes, given the node's first value output
/// `out` and its first operand `first`.
#[must_use]
pub fn width_tie<T>(kind: &NodeKind, slot: usize, out: Option<T>, first: Option<T>) -> WidthTie<T> {
    match kind.operand_width(slot) {
        OperandWidth::OfOutput => out.map_or(WidthTie::NoHead, WidthTie::Tied),
        OperandWidth::OfFirstOperand => first.map_or(WidthTie::NoHead, WidthTie::Tied),
        OperandWidth::Conversion(c) => WidthTie::Conversion(c),
        OperandWidth::Free => WidthTie::Free,
    }
}

#[cfg(test)]
mod tests {
    use super::{WidthTie, width_tie};
    use crate::node::{Conversion, ExtendOp, NodeKind, ValueType};

    const OUT: ValueType = ValueType::Int(32);
    const FIRST: ValueType = ValueType::Int(8);

    fn tie_of(kind: &NodeKind, slot: usize) -> WidthTie<ValueType> {
        width_tie(kind, slot, Some(OUT), Some(FIRST))
    }

    /// One rule per `OperandWidth` arm, read through the one interpreter.
    #[test]
    fn each_width_rule_names_its_own_head() {
        let add = NodeKind::IntBinaryOp(crate::IntBinaryOp::Add);
        assert_eq!(tie_of(&add, 0), WidthTie::Tied(OUT));
        assert_eq!(tie_of(&add, 1), WidthTie::Tied(OUT));

        let shl = NodeKind::IntBinaryOp(crate::IntBinaryOp::ShiftLeft);
        assert_eq!(tie_of(&shl, 0), WidthTie::Tied(OUT), "the shifted value");
        assert_eq!(tie_of(&shl, 1), WidthTie::Free, "p-code leaves the count");

        let eq = NodeKind::IntCmpOp(crate::IntCmpOp::Equal);
        assert_eq!(tie_of(&eq, 0), WidthTie::Tied(FIRST), "not the I1 output");
        assert_eq!(tie_of(&eq, 1), WidthTie::Tied(FIRST));

        assert_eq!(
            tie_of(&NodeKind::Extend(ExtendOp::ZeroExtend), 0),
            WidthTie::Conversion(Conversion::Widen)
        );
        assert_eq!(
            tie_of(&NodeKind::Truncate, 0),
            WidthTie::Conversion(Conversion::Narrow)
        );
        assert_eq!(
            tie_of(&NodeKind::IntBitsToFloat, 0),
            WidthTie::Conversion(Conversion::SameSize)
        );
        assert_eq!(tie_of(&NodeKind::Region, 0), WidthTie::Free);
    }

    /// A rule that names a head the graph does not supply reads back as
    /// `NoHead`, so a caller cannot mistake it for an agreement.
    #[test]
    fn a_missing_head_is_not_an_agreement() {
        let add = NodeKind::IntBinaryOp(crate::IntBinaryOp::Add);
        assert_eq!(
            width_tie::<ValueType>(&add, 0, None, Some(FIRST)),
            WidthTie::NoHead
        );
        let eq = NodeKind::IntCmpOp(crate::IntCmpOp::Equal);
        assert_eq!(
            width_tie::<ValueType>(&eq, 0, Some(OUT), None),
            WidthTie::NoHead
        );
    }
}
