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
