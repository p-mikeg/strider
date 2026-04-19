use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeKind, NodeOutputId};
use ir::{
    BoolBinaryOp, BoolUnaryOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp,
    IntUnaryOp,
};

use crate::var::{
    BoolBinaryOpVar, BoolUnaryOpVar, BoolVar, FloatBinaryOpVar, FloatCmpOpVar, FloatUnaryOpVar,
    FloatVar, IntBinaryOpVar, IntCmpOpVar, IntUnaryOpVar, IntVar, NodeVar, Var,
};

use super::bindings::Bindings;

// ── Match ─────────────────────────────────────────────────────────────────────

/// The result of a successful pattern match against a single root node.
///
/// Provides access to the captured variable bindings and convenience helpers
/// for reading constant values.
pub struct Match {
    /// The root node where the top-level pattern matched.
    pub root: NodeId,
    pub(super) bindings: Bindings,
}

impl Match {
    /// Returns the `NodeOutputId` bound to the data-capture variable `v`,
    /// or `None` if `v` was not captured in this match.
    pub fn get(&self, v: Var) -> Option<NodeOutputId> {
        self.bindings.get(v)
    }

    /// Returns the `NodeId` bound to the control-capture variable `nv`,
    /// or `None` if `nv` was not captured in this match.
    pub fn get_node(&self, nv: NodeVar) -> Option<NodeId> {
        self.bindings.get_node(nv)
    }

    /// Returns the integer constant value bound to the [`IntVar`] `iv`, or
    /// `None` if `iv` was not captured in this match.
    pub fn get_int(&self, iv: IntVar) -> Option<u64> {
        self.bindings.get_int(iv)
    }

    /// Returns the boolean constant value bound to the [`BoolVar`] `bv`, or
    /// `None` if `bv` was not captured in this match.
    pub fn get_bool(&self, bv: BoolVar) -> Option<bool> {
        self.bindings.get_bool(bv)
    }

    /// Returns the float constant IEEE 754 bit pattern bound to the [`FloatVar`]
    /// `fv`, or `None` if `fv` was not captured in this match.
    ///
    /// Named `get_float` (not `get_float_bits`) to avoid colliding with the
    /// graph-lookup helper [`Match::get_float_bits`] which takes a [`Var`] and a
    /// graph reference.
    pub fn get_float(&self, fv: FloatVar) -> Option<u64> {
        self.bindings.get_float_bits(fv)
    }

    /// Returns the [`IntBinaryOp`] variant bound to `v`, or `None` if unbound.
    pub fn get_int_binary_op(&self, v: IntBinaryOpVar) -> Option<IntBinaryOp> {
        self.bindings.get_int_binary_op(v)
    }

    /// Returns the [`IntUnaryOp`] variant bound to `v`, or `None` if unbound.
    pub fn get_int_unary_op(&self, v: IntUnaryOpVar) -> Option<IntUnaryOp> {
        self.bindings.get_int_unary_op(v)
    }

    /// Returns the [`IntCmpOp`] variant bound to `v`, or `None` if unbound.
    pub fn get_int_cmp_op(&self, v: IntCmpOpVar) -> Option<IntCmpOp> {
        self.bindings.get_int_cmp_op(v)
    }

    /// Returns the [`BoolBinaryOp`] variant bound to `v`, or `None` if unbound.
    pub fn get_bool_binary_op(&self, v: BoolBinaryOpVar) -> Option<BoolBinaryOp> {
        self.bindings.get_bool_binary_op(v)
    }

    /// Returns the [`BoolUnaryOp`] variant bound to `v`, or `None` if unbound.
    pub fn get_bool_unary_op(&self, v: BoolUnaryOpVar) -> Option<BoolUnaryOp> {
        self.bindings.get_bool_unary_op(v)
    }

    /// Returns the [`FloatBinaryOp`] variant bound to `v`, or `None` if unbound.
    pub fn get_float_binary_op(&self, v: FloatBinaryOpVar) -> Option<FloatBinaryOp> {
        self.bindings.get_float_binary_op(v)
    }

    /// Returns the [`FloatUnaryOp`] variant bound to `v`, or `None` if unbound.
    pub fn get_float_unary_op(&self, v: FloatUnaryOpVar) -> Option<FloatUnaryOp> {
        self.bindings.get_float_unary_op(v)
    }

    /// Returns the [`FloatCmpOp`] variant bound to `v`, or `None` if unbound.
    pub fn get_float_cmp_op(&self, v: FloatCmpOpVar) -> Option<FloatCmpOp> {
        self.bindings.get_float_cmp_op(v)
    }

    /// Returns an owned copy of the full [`Bindings`] captured by this match.
    ///
    /// Useful when a caller needs to keep the bindings alive past the match —
    /// e.g. the rewrite-rule engine drops the [`Matcher`] borrow before
    /// constructing fresh graph nodes, so it needs an owned snapshot of the
    /// captures to consult while mutating the graph.
    pub fn bindings_clone(&self) -> Bindings {
        self.bindings.clone()
    }

    /// If the output bound to `v` was produced by an `IntConst` node, returns
    /// the stored constant value.  Returns `None` for unbound vars or non-const
    /// outputs.
    pub fn get_int_const(&self, v: Var, graph: &BuiltFunctionGraph) -> Option<u64> {
        let out = self.bindings.get(v)?;
        let node = graph.graph.get_node_from_output(out);
        match graph.graph.node_kind(node) {
            NodeKind::IntConst(val) => Some(*val),
            _ => None,
        }
    }

    /// If the output bound to `v` was produced by a `BoolConst` node, returns
    /// the constant value.  Returns `None` for unbound vars or non-bool-const
    /// outputs.
    pub fn get_bool_const(&self, v: Var, graph: &BuiltFunctionGraph) -> Option<bool> {
        let out = self.bindings.get(v)?;
        let node = graph.graph.get_node_from_output(out);
        match graph.graph.node_kind(node) {
            NodeKind::BoolConst(val) => Some(*val),
            _ => None,
        }
    }

    /// If the output bound to `v` was produced by a `FloatConst` node, returns
    /// the raw IEEE 754 bit pattern stored as `u64`.  Returns `None` for
    /// unbound vars or non-float-const outputs.
    pub fn get_float_bits(&self, v: Var, graph: &BuiltFunctionGraph) -> Option<u64> {
        let out = self.bindings.get(v)?;
        let node = graph.graph.get_node_from_output(out);
        match graph.graph.node_kind(node) {
            NodeKind::FloatConst(bits) => Some(*bits),
            _ => None,
        }
    }
}
