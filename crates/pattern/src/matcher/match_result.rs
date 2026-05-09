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
    /// Constructs a `Match` directly from a root and a bindings map.
    /// Test-only entry point: production matches go through
    /// [`crate::Matcher`].  Exposed so external tests (and the
    /// rewrite-rule interpreter) can synthesise matches without
    /// running a full pattern walk.
    #[must_use]
    pub fn new_for_test(root: NodeId, bindings: super::bindings::Bindings) -> Self {
        Self { root, bindings }
    }

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
    pub fn get_uint(&self, c: Capture, graph: &ir::Graph) -> Option<u128> {
        self.bindings.get_uint(c, graph)
    }

    /// If the node bound to `c` is an `IntConst`, returns the stored
    /// constant sign-extended from the output type's bit width to
    /// `i128`.  Returns `None` otherwise.
    #[must_use]
    pub fn get_int(&self, c: Capture, graph: &ir::Graph) -> Option<i128> {
        self.bindings.get_int(c, graph)
    }

    /// If the node bound to `c` is a `BoolConst`, returns the stored
    /// boolean value.  Returns `None` otherwise.
    #[must_use]
    pub fn get_bool(&self, c: Capture, graph: &ir::Graph) -> Option<bool> {
        self.bindings.get_bool(c, graph)
    }

    /// If the node bound to `c` is a `FloatConst`, returns the raw
    /// IEEE 754 bit pattern as `u64`.  Returns `None` otherwise.
    #[must_use]
    pub fn get_float_bits(&self, c: Capture, graph: &ir::Graph) -> Option<u64> {
        self.bindings.get_float_bits(c, graph)
    }

    /// If the node bound to `c` is an `IntBinaryOp`, returns the op variant.
    #[must_use]
    pub fn get_int_binary_op(
        &self,
        c: Capture,
        graph: &ir::Graph,
    ) -> Option<IntBinaryOp> {
        self.bindings.get_int_binary_op(c, graph)
    }

    /// If the node bound to `c` is an `IntUnaryOp`, returns the op variant.
    #[must_use]
    pub fn get_int_unary_op(
        &self,
        c: Capture,
        graph: &ir::Graph,
    ) -> Option<IntUnaryOp> {
        self.bindings.get_int_unary_op(c, graph)
    }

    /// If the node bound to `c` is an `IntCmpOp`, returns the op variant.
    #[must_use]
    pub fn get_int_cmp_op(&self, c: Capture, graph: &ir::Graph) -> Option<IntCmpOp> {
        self.bindings.get_int_cmp_op(c, graph)
    }

    /// If the node bound to `c` is a `BoolBinaryOp`, returns the op variant.
    #[must_use]
    pub fn get_bool_binary_op(
        &self,
        c: Capture,
        graph: &ir::Graph,
    ) -> Option<BoolBinaryOp> {
        self.bindings.get_bool_binary_op(c, graph)
    }

    /// If the node bound to `c` is a `BoolUnaryOp`, returns the op variant.
    #[must_use]
    pub fn get_bool_unary_op(
        &self,
        c: Capture,
        graph: &ir::Graph,
    ) -> Option<BoolUnaryOp> {
        self.bindings.get_bool_unary_op(c, graph)
    }

    /// If the node bound to `c` is a `FloatBinaryOp`, returns the op variant.
    #[must_use]
    pub fn get_float_binary_op(
        &self,
        c: Capture,
        graph: &ir::Graph,
    ) -> Option<FloatBinaryOp> {
        self.bindings.get_float_binary_op(c, graph)
    }

    /// If the node bound to `c` is a `FloatUnaryOp`, returns the op variant.
    #[must_use]
    pub fn get_float_unary_op(
        &self,
        c: Capture,
        graph: &ir::Graph,
    ) -> Option<FloatUnaryOp> {
        self.bindings.get_float_unary_op(c, graph)
    }

    /// If the node bound to `c` is a `FloatCmpOp`, returns the op variant.
    #[must_use]
    pub fn get_float_cmp_op(
        &self,
        c: Capture,
        graph: &ir::Graph,
    ) -> Option<FloatCmpOp> {
        self.bindings.get_float_cmp_op(c, graph)
    }

    /// Returns the [`rsleigh::Vn`] associated with the binding, if one
    /// can be determined.  The output-to-varnode mapping is well-defined
    /// only for a handful of producer kinds:
    ///
    /// * `InitialVar(vn)` — the varnode whose function-entry value is
    ///   read.
    /// * `Call` outputs at slot `2 + i` — the varnode at the per-Call
    ///   override on [`ir::Graph::call_clobbered_override`] when one
    ///   was recorded (e.g. `__fentry__` callbacks built via
    ///   [`ir::FunctionBuilder::build_call_with_cc`]), otherwise the
    ///   varnode at `BuiltFunctionGraph::call_clobbered[i]`.
    /// * `CallOther` outputs in their clobber slot range (slot 2.. for
    ///   value-less CallOther, slot 3.. for CallOther with a value
    ///   output) — the varnode at the per-CallOther override on
    ///   [`ir::Graph::call_clobbered_override`] when one was recorded,
    ///   otherwise the varnode at
    ///   `BuiltFunctionGraph::call_other_clobbered[i]`.
    ///
    /// Returns `None` for unbound captures or producers without a
    /// well-defined varnode mapping.
    #[must_use]
    pub fn get_vn(&self, c: Capture, graph: &BuiltFunctionGraph) -> Option<rsleigh::Vn> {
        let binding = self.bindings.get_binding(c)?;
        if let Some(out) = binding.output {
            let (node, slot) = graph.graph.output_definition(out);
            let kind = graph.graph.node_kind(node);
            // Call: clobber slots start at index 2.
            if matches!(kind, NodeKind::Call) && slot >= 2 {
                let idx = (slot - 2) as usize;
                if let Some(override_list) = graph.graph.call_clobbered_override(node) {
                    return override_list.get(idx).copied();
                }
                return graph.call_clobbered.get(idx).copied();
            }
            // CallOther: clobber slots start at index 2 (no value
            // output) or 3 (with value output).  Detect by total
            // output count: `2 + clobber_len` for value-less,
            // `3 + clobber_len` for value-bearing.
            //
            // The clobber length here is per-CallOther: a precise-ABI
            // CallOther carries its own `call_clobbered_override` list,
            // and that list's length may differ from the function-default
            // `call_other_clobbered` (e.g. `syscall` writes RAX/RCX/R11
            // = 3 slots, while a SWI emits only `[r0]` = 1 slot, while
            // the function-default may be empty).  Use the override
            // length when present so `clobber_start` matches the actual
            // node shape — a function-default-based check would produce
            // a "shape we don't recognise" miss for every per-CallOther
            // override whose length differs from the default.
            if matches!(kind, NodeKind::CallOther { .. }) {
                let total_outputs = graph.graph.node_outputs(node).len();
                let clobber_len = graph
                    .graph
                    .call_clobbered_override(node)
                    .map_or(graph.call_other_clobbered.len(), |ov| ov.len());
                let clobber_start: u32 = if total_outputs == 2 + clobber_len {
                    2
                } else if total_outputs == 3 + clobber_len {
                    3
                } else {
                    // Shape we don't recognise; bail.
                    return None;
                };
                if slot < clobber_start {
                    // Slot 0/1 are Control/Memory; slot 2 (value-bearing
                    // form) is the user-op's value output — none of these
                    // map to a varnode.
                    return None;
                }
                let idx = (slot - clobber_start) as usize;
                if let Some(override_list) = graph.graph.call_clobbered_override(node) {
                    return override_list.get(idx).copied();
                }
                return graph.call_other_clobbered.get(idx).copied();
            }
        }
        match graph.graph.node_kind(binding.node) {
            NodeKind::InitialVar(vn) => Some(*vn),
            _ => None,
        }
    }

    /// If the node bound to `c` is a [`NodeKind::StackStore`], returns
    /// its compile-time SP-relative offset.  Returns `None` for an
    /// unbound capture or a non-`StackStore` producer.
    ///
    /// Pairs with `pattern::stack_store().capture(c)` for queries that
    /// need to recover the offset value (e.g. cross-pattern field-
    /// offset recoveries: capture the stack store, capture an
    /// `any_int_const(other)` on the call-arg side, compute the
    /// difference).
    #[must_use]
    pub fn stack_offset(&self, c: Capture, graph: &ir::Graph) -> Option<i64> {
        let node = self.bindings.get_node(c)?;
        match graph.node_kind(node) {
            NodeKind::StackStore { offset, .. } => Some(*offset),
            _ => None,
        }
    }

    /// If the node bound to `c` is a [`NodeKind::StackStorePhi`] with
    /// a populated side-table, returns its per-predecessor offset
    /// slice from [`ir::Graph::stack_phi_offsets`].  Slice ordering
    /// follows IR predecessor order.
    ///
    /// Returns `None` for:
    /// * an unbound capture,
    /// * a non-`StackStorePhi` producer, OR
    /// * a `StackStorePhi` whose side-table entry is empty (a
    ///   malformed-graph / un-populated state — the side table
    ///   defaults to an empty `Vec` for any node never written by
    ///   `StackStoreDetect`).  Collapsing the empty side-table case
    ///   to `None` keeps callers from silently iterating zero
    ///   offsets thinking they captured a real phi shape.
    #[must_use]
    pub fn stack_phi_offsets<'g>(
        &self,
        c: Capture,
        graph: &'g ir::Graph,
    ) -> Option<&'g [i64]> {
        let node = self.bindings.get_node(c)?;
        match graph.node_kind(node) {
            NodeKind::StackStorePhi { .. } => {
                let slice = graph.stack_phi_offsets(node);
                if slice.is_empty() { None } else { Some(slice) }
            }
            _ => None,
        }
    }

    /// Returns the asm-instruction-address fingerprint of the node bound
    /// to `c`, as a sorted-deduplicated slice.  Returns an empty slice
    /// when the capture is unbound or when the bound node has no
    /// recorded contributors (legitimately empty for region / phi /
    /// initial-state kinds — see
    /// [`ir::Graph::asm_fingerprint`] for the documented exempt set).
    ///
    /// This is the proof-of-correctness aid: when a pattern query
    /// captures a value node, this slice lists the machine
    /// instructions whose lifting (or subsequent rewrite) contributed
    /// to that node's value.  See
    /// `docs/superpowers/specs/2026-05-03-asm-fingerprints-design.md`
    /// for the full contract.
    #[must_use]
    pub fn asm_fingerprint<'g>(
        &self,
        c: Capture,
        graph: &'g ir::Graph,
    ) -> &'g [u64] {
        match self.bindings.get_node(c) {
            Some(node) => graph.asm_fingerprint(node),
            None => &[],
        }
    }

    /// If the node bound to `c` is an [`ir::node::NodeKind::IntConstWide`],
    /// returns the raw little-endian bytes of its stored value (32 bytes
    /// for `U256`, 64 for `U512`).  Returns `None` for unbound captures
    /// or non-`IntConstWide` producers — narrow constants go through
    /// [`Self::get_uint`] / [`Self::get_int`] instead.
    #[must_use]
    pub fn get_wide_bytes(&self, c: Capture, graph: &ir::Graph) -> Option<Vec<u8>> {
        let node = self.bindings.get_node(c)?;
        match graph.node_kind(node) {
            ir::node::NodeKind::IntConstWide(id) => Some(graph.wide_const(*id).to_le_bytes()),
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
