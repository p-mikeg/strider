//! The one reader of [`NodeKind::operand_width`]: where an input slot's width
//! comes from, and the equivalence classes that answer folds into.

use crate::node::{NodeKind, ValueId};
use crate::node_signature::{Conversion, OperandWidth};

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

/// Union-find over value vertices, grouping the ones a node evaluates at one
/// width.
pub struct WidthGroups {
    parent: Vec<u32>,
}

impl WidthGroups {
    /// Over vertex ids below `len`.
    #[must_use]
    pub fn new(len: usize) -> Self {
        Self {
            parent: (0..len as u32).collect(),
        }
    }

    pub fn find(&mut self, v: ValueId) -> usize {
        let mut i = v.as_u32() as usize;
        while self.parent[i] as usize != i {
            let grandparent = self.parent[self.parent[i] as usize];
            self.parent[i] = grandparent;
            i = grandparent as usize;
        }
        i
    }

    pub fn union(&mut self, a: ValueId, b: ValueId) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[rb] = ra as u32;
        }
    }

    /// [`width_tie`] with a `Tied` head joined to `operand`. A head that IS
    /// the operand ties nothing and reads back as `NoHead`.
    #[must_use]
    pub fn tie(
        &mut self,
        kind: &NodeKind,
        slot: usize,
        operand: ValueId,
        out: Option<ValueId>,
        first: Option<ValueId>,
    ) -> WidthTie<ValueId> {
        match width_tie(kind, slot, out, first) {
            WidthTie::Tied(head) if head != operand => {
                self.union(head, operand);
                WidthTie::Tied(head)
            }
            WidthTie::Tied(_) => WidthTie::NoHead,
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{WidthGroups, WidthTie, width_tie};
    use crate::node::{Conversion, ExtendOp, NodeKind, ValueId, ValueType};
    use cranelift_entity::EntityRef;

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

    fn v(i: usize) -> ValueId {
        ValueId::new(i)
    }

    #[test]
    fn every_vertex_starts_in_its_own_group() {
        let mut g = WidthGroups::new(4);
        let roots: Vec<usize> = (0..4).map(|i| g.find(v(i))).collect();
        assert_eq!(roots, vec![0, 1, 2, 3]);
    }

    /// Union is transitive, and a chain built one link at a time still
    /// resolves: `find` halves the path rather than following it twice.
    #[test]
    fn a_union_chain_resolves_to_one_group() {
        let mut g = WidthGroups::new(64);
        for i in 1..64 {
            g.union(v(i - 1), v(i));
        }
        let root = g.find(v(63));
        assert!((0..64).all(|i| g.find(v(i)) == root), "one group");
        // Path halving leaves every member pointing at or near the root, so a
        // second pass cannot walk the chain again.
        assert_eq!(g.find(v(63)), root);
    }

    #[test]
    fn unrelated_groups_stay_apart() {
        let mut g = WidthGroups::new(4);
        g.union(v(0), v(1));
        g.union(v(2), v(3));
        assert_eq!(g.find(v(0)), g.find(v(1)));
        assert_ne!(g.find(v(0)), g.find(v(2)));
    }

    /// `tie` joins the head it names, and reports it so a caller can record
    /// the pairing.
    #[test]
    fn tie_joins_the_head_the_rule_names() {
        let add = NodeKind::IntBinaryOp(crate::IntBinaryOp::Add);
        let mut g = WidthGroups::new(4);
        assert_eq!(
            g.tie(&add, 0, v(1), Some(v(0)), Some(v(1))),
            WidthTie::Tied(v(0))
        );
        assert_eq!(g.find(v(0)), g.find(v(1)));
        assert_ne!(g.find(v(0)), g.find(v(2)));
    }

    /// A head that IS the operand ties nothing, and must not read back as an
    /// agreement a caller would record against itself.
    #[test]
    fn tie_to_itself_ties_nothing() {
        let add = NodeKind::IntBinaryOp(crate::IntBinaryOp::Add);
        let mut g = WidthGroups::new(4);
        assert_eq!(
            g.tie(&add, 0, v(2), Some(v(2)), Some(v(2))),
            WidthTie::NoHead
        );
        assert_eq!(g.find(v(2)), 2, "still its own root");
        assert_ne!(g.find(v(2)), g.find(v(3)));
    }

    /// A free slot and a conversion join nothing.
    #[test]
    fn tie_leaves_a_free_or_converted_slot_alone() {
        let shl = NodeKind::IntBinaryOp(crate::IntBinaryOp::ShiftLeft);
        let mut g = WidthGroups::new(4);
        assert_eq!(g.tie(&shl, 1, v(1), Some(v(0)), Some(v(1))), WidthTie::Free);
        assert_ne!(g.find(v(0)), g.find(v(1)));

        assert_eq!(
            g.tie(&NodeKind::Truncate, 0, v(3), Some(v(2)), Some(v(3))),
            WidthTie::Conversion(Conversion::Narrow)
        );
        assert_ne!(g.find(v(2)), g.find(v(3)));
    }
}
