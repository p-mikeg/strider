use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeKind, NodeOutputId};
use ir::{
    BoolBinaryOp, BoolUnaryOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp,
    IntUnaryOp,
};

use crate::var::Capture;

use super::bindings::Bindings;

// ── Match ─────────────────────────────────────────────────────────────────────

/// The result of a successful pattern match against a single root node.
///
/// Provides access to the captured variable bindings and convenience helpers
/// for reading constant values and op-variant discriminants from each
/// captured node.
#[derive(Clone)]
pub struct Match {
    pub(super) root: NodeId,
    pub(super) bindings: Bindings,
}

impl Match {
    /// The root node where the top-level pattern matched.
    #[must_use]
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Returns the `NodeId` bound to `c`, or `None` if `c` was not
    /// captured in this match.  Every successful capture binds at
    /// least the matched node id.
    #[must_use]
    pub fn node(&self, c: Capture) -> Option<NodeId> {
        self.bindings.get_node(c)
    }

    /// Returns the value `NodeOutputId` bound to `c`, or `None` if
    /// `c` was not captured or the binding was control-flow.
    /// Multi-output nodes (e.g. `Load = [Memory, Value]`) bind the
    /// value slot.
    #[must_use]
    pub fn output(&self, c: Capture) -> Option<NodeOutputId> {
        self.bindings.get_output(c)
    }

    /// If the node bound to `c` is an `IntConst`, returns the stored
    /// constant value masked to the output type's bit width.  Returns
    /// `None` for unbound captures, control-flow bindings, or
    /// non-`IntConst` producers.
    #[must_use]
    pub fn get_uint(&self, c: Capture, graph: &BuiltFunctionGraph) -> Option<u128> {
        self.bindings.get_uint(c, graph)
    }

    /// If the node bound to `c` is an `IntConst`, returns the stored
    /// constant sign-extended from the output type's bit width to
    /// `i128`.  Returns `None` otherwise.
    #[must_use]
    pub fn get_int(&self, c: Capture, graph: &BuiltFunctionGraph) -> Option<i128> {
        self.bindings.get_int(c, graph)
    }

    /// If the node bound to `c` is a `BoolConst`, returns the stored
    /// boolean value.  Returns `None` otherwise.
    #[must_use]
    pub fn get_bool(&self, c: Capture, graph: &BuiltFunctionGraph) -> Option<bool> {
        self.bindings.get_bool(c, graph)
    }

    /// If the node bound to `c` is a `FloatConst`, returns the raw
    /// IEEE 754 bit pattern as `u64`.  Returns `None` otherwise.
    #[must_use]
    pub fn get_float_bits(&self, c: Capture, graph: &BuiltFunctionGraph) -> Option<u64> {
        self.bindings.get_float_bits(c, graph)
    }

    /// If the node bound to `c` is an `IntBinaryOp`, returns the op variant.
    #[must_use]
    pub fn get_int_binary_op(
        &self,
        c: Capture,
        graph: &BuiltFunctionGraph,
    ) -> Option<IntBinaryOp> {
        self.bindings.get_int_binary_op(c, graph)
    }

    /// If the node bound to `c` is an `IntUnaryOp`, returns the op variant.
    #[must_use]
    pub fn get_int_unary_op(
        &self,
        c: Capture,
        graph: &BuiltFunctionGraph,
    ) -> Option<IntUnaryOp> {
        self.bindings.get_int_unary_op(c, graph)
    }

    /// If the node bound to `c` is an `IntCmpOp`, returns the op variant.
    #[must_use]
    pub fn get_int_cmp_op(&self, c: Capture, graph: &BuiltFunctionGraph) -> Option<IntCmpOp> {
        self.bindings.get_int_cmp_op(c, graph)
    }

    /// If the node bound to `c` is a `BoolBinaryOp`, returns the op variant.
    #[must_use]
    pub fn get_bool_binary_op(
        &self,
        c: Capture,
        graph: &BuiltFunctionGraph,
    ) -> Option<BoolBinaryOp> {
        self.bindings.get_bool_binary_op(c, graph)
    }

    /// If the node bound to `c` is a `BoolUnaryOp`, returns the op variant.
    #[must_use]
    pub fn get_bool_unary_op(
        &self,
        c: Capture,
        graph: &BuiltFunctionGraph,
    ) -> Option<BoolUnaryOp> {
        self.bindings.get_bool_unary_op(c, graph)
    }

    /// If the node bound to `c` is a `FloatBinaryOp`, returns the op variant.
    #[must_use]
    pub fn get_float_binary_op(
        &self,
        c: Capture,
        graph: &BuiltFunctionGraph,
    ) -> Option<FloatBinaryOp> {
        self.bindings.get_float_binary_op(c, graph)
    }

    /// If the node bound to `c` is a `FloatUnaryOp`, returns the op variant.
    #[must_use]
    pub fn get_float_unary_op(
        &self,
        c: Capture,
        graph: &BuiltFunctionGraph,
    ) -> Option<FloatUnaryOp> {
        self.bindings.get_float_unary_op(c, graph)
    }

    /// If the node bound to `c` is a `FloatCmpOp`, returns the op variant.
    #[must_use]
    pub fn get_float_cmp_op(
        &self,
        c: Capture,
        graph: &BuiltFunctionGraph,
    ) -> Option<FloatCmpOp> {
        self.bindings.get_float_cmp_op(c, graph)
    }

    /// Returns the [`rsleigh::Vn`] associated with the binding, if one
    /// can be determined.  The output-to-varnode mapping is well-defined
    /// only for a handful of producer kinds:
    ///
    /// * `InitialVar(vn)` — the varnode whose function-entry value is
    ///   read.
    /// * `Call` outputs at slot `2 + i` — the varnode at
    ///   `BuiltFunctionGraph::call_clobbered[i]`.
    ///
    /// Returns `None` for unbound captures or producers without a
    /// well-defined varnode mapping.
    #[must_use]
    pub fn get_vn(&self, c: Capture, graph: &BuiltFunctionGraph) -> Option<rsleigh::Vn> {
        let binding = self.bindings.get_binding(c)?;
        if let Some(out) = binding.output {
            let (node, slot) = graph.graph.output_definition(out);
            if matches!(graph.graph.node_kind(node), NodeKind::Call) && slot >= 2 {
                let idx = (slot - 2) as usize;
                return graph.call_clobbered.get(idx).copied();
            }
        }
        match graph.graph.node_kind(binding.node) {
            NodeKind::InitialVar(vn) => Some(*vn),
            _ => None,
        }
    }

    /// Returns an owned copy of the full [`Bindings`] captured by this match.
    /// Used by the rewrite-rule interpreter (drops the `Matcher` borrow
    /// before mutating the graph) and by tests.
    #[must_use]
    pub fn bindings_clone(&self) -> Bindings {
        self.bindings.clone()
    }
}
