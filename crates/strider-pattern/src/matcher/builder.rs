//! # Why staging
//!
//! [`strider_graph::Graph`] creates a node with all its outputs and resolves
//! its inputs at creation time. The builder API is incremental: a bare `node()`
//! comes first, outputs are added later, and inputs are wired once producers
//! are compiled. So both builders stage into a [`StagedGraph`] and materialise
//! the whole DAG at [`finish`](MatcherBuilder::finish) in
//! producer-before-consumer order. Each input's sparse consumer slot is
//! recorded on the materialised [`PatNode`]'s `input_slots` via [`SealNode`].

use strider_ir::IntBinaryOp;
use strider_ir::node::{NodeKind, ValueType};

use crate::matcher::{KindSpec, OutputKindSpec, PatNode, PatValue, Pattern};
use crate::staging::{SealNode, StagedGraph};

/// Names a staged node's output by position.
#[derive(Clone, Copy)]
pub struct PatValueRef {
    node: usize,
    output: usize,
}

/// Names a staged node by position.
#[derive(Clone, Copy)]
pub struct PatNodeRef(pub(crate) usize);

// The match side seals into the same `PatNode` type it stages.
impl SealNode for PatNode {
    type Sealed = PatNode;
    fn seal(mut self, input_slots: Vec<usize>) -> PatNode {
        self.input_slots = input_slots;
        self
    }
}

/// The match root is derived structurally, so the seal takes no root handle.
/// Returned [`PatValueRef`] / [`PatNodeRef`] handles are scoped to the builder
/// that produced them.
pub struct MatcherBuilder {
    core: StagedGraph<PatNode, PatValue>,
}

impl Default for MatcherBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl MatcherBuilder {
    pub fn new() -> Self {
        Self {
            core: StagedGraph::new(),
        }
    }

    /// One value output at slot `0`, no inputs.
    pub fn leaf(&mut self, kind: KindSpec) -> PatValueRef {
        let n = self.stage(PatNode::from_kind(kind));
        self.value_output(n, 0)
    }

    /// Consumes `inner` at slot `0`, one value output at slot `0`.
    pub fn unary(&mut self, kind: KindSpec, inner: PatValueRef) -> PatValueRef {
        let n = self.stage(PatNode::from_kind(kind));
        self.input(n, 0, inner);
        self.value_output(n, 0)
    }

    /// Consumes `l` at slot `0` and `r` at slot `1`, one value output at
    /// slot `0`.
    pub fn binary(&mut self, op: IntBinaryOp, l: PatValueRef, r: PatValueRef) -> PatValueRef {
        let n = self.stage(PatNode::exact(NodeKind::IntBinaryOp(op)));
        self.input(n, 0, l);
        self.input(n, 1, r);
        self.value_output(n, 0)
    }

    /// No inputs or outputs yet.
    pub fn node(&mut self, kind: KindSpec) -> PatNodeRef {
        self.stage(PatNode::from_kind(kind))
    }

    /// Matches a value if any alternative does. The alternatives wire as this
    /// node's inputs, but the matcher tries each against the *same* IR node
    /// rather than as operands, first match winning. Empty `alts` matches
    /// nothing.
    pub fn one_of(&mut self, alts: &[PatValueRef]) -> PatValueRef {
        let mut alt_node = PatNode::from_kind(KindSpec::Any);
        alt_node.alternation = true;
        let n = self.stage(alt_node);
        for (slot, alt) in alts.iter().enumerate() {
            self.input(n, slot, *alt);
        }
        self.value_output(n, 0)
    }

    pub fn input(&mut self, node: PatNodeRef, slot: usize, prod: PatValueRef) {
        self.core.add_input(node.0, slot, prod.node, prod.output);
    }

    pub fn value_output(&mut self, node: PatNodeRef, slot: usize) -> PatValueRef {
        self.add_output(node, PatValue::value(slot))
    }

    pub fn control_output(&mut self, node: PatNodeRef, slot: usize) -> PatValueRef {
        self.add_output(node, PatValue::control(slot))
    }

    pub fn memory_output(&mut self, node: PatNodeRef, slot: usize) -> PatValueRef {
        self.add_output(node, PatValue::memory(slot))
    }

    pub fn set_value_ty(&mut self, out: PatValueRef, ty: ValueType) {
        self.out_of(out).kind = OutputKindSpec::Value(ty);
    }

    pub fn set_output_control(&mut self, out: PatValueRef) {
        self.out_of(out).kind = OutputKindSpec::Control;
    }

    /// Relaxes `out` to [`OutputKindSpec::Any`], so `any()` / `var()` match
    /// value-less kinds (`Region`, `MemPhi`, ...) too.
    pub fn set_output_any(&mut self, out: PatValueRef) {
        self.out_of(out).kind = OutputKindSpec::Any;
    }

    pub fn set_value_width(&mut self, out: PatValueRef, bits: u32) {
        self.out_of(out).width = Some(bits);
    }

    /// See [`PatValue::match_slot`]. Pins the value operand to a specific
    /// output slot.
    pub fn set_value_out_slot(&mut self, out: PatValueRef, slot: usize) {
        self.out_of(out).match_slot = Some(slot);
    }

    /// Binds the matched output's value (`Binding::Value`), e.g.
    /// `add(var(x), ..)` captures `x`'s value, not its node.
    pub fn capture_output(&mut self, out: PatValueRef, c: crate::capture::Capture) {
        self.out_of(out).capture = Some(c);
    }

    /// Binds the matched node (`Binding::Node`), for zero-value-output roots
    /// like `Return` / `If`.
    pub fn capture_node(&mut self, node: PatNodeRef, c: crate::capture::Capture) {
        self.core.kind_mut(node.0).capture = Some(c);
    }

    pub fn set_node_predicate(&mut self, out: PatValueRef, f: crate::matcher::NodePredicate) {
        self.core.kind_mut(out.node).node_predicate = Some(f);
    }

    pub fn set_post_match(&mut self, out: PatValueRef, f: crate::matcher::PostMatchFn) {
        self.core.kind_mut(out.node).post_match = Some(f);
    }

    /// Disables commutative operand reordering on `out`'s producer.
    pub fn set_force_ordered(&mut self, out: PatValueRef) {
        self.core.kind_mut(out.node).force_ordered = true;
    }

    /// Sets `bits` on every value input consumed by `out`'s producer.
    pub fn constrain_input_widths(&mut self, out: PatValueRef, bits: u32) {
        for (pn, po) in self.core.input_producers(out.node) {
            self.core.output_mut(pn, po).width = Some(bits);
        }
    }

    /// Materialises every staged node in producer-before-consumer order.
    /// Performs no structural validation: single-rootedness and acyclicity are
    /// reported at match time, not here.
    ///
    /// # Panics
    /// On a cyclic staged graph (a builder bug).
    #[allow(clippy::expect_used)]
    pub fn finish(self) -> Pattern {
        let graph = self.core.seal().expect("cyclic staged pattern graph");
        Pattern::from_graph(graph)
    }

    fn stage(&mut self, kind: PatNode) -> PatNodeRef {
        PatNodeRef(self.core.add_node(kind))
    }

    fn add_output(&mut self, node: PatNodeRef, out: PatValue) -> PatValueRef {
        let output = self.core.add_output(node.0, out);
        PatValueRef {
            node: node.0,
            output,
        }
    }

    fn out_of(&mut self, out: PatValueRef) -> &mut PatValue {
        self.core.output_mut(out.node, out.output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strider_ir::node::NodeKind;
    use strider_ir::{ConstId, IntBinaryOp};

    #[test]
    fn binary_builder_wires_two_inputs_and_one_output() {
        let mut b = MatcherBuilder::new();
        let x = b.leaf(crate::matcher::KindSpec::Any);
        let k = b.leaf(crate::matcher::KindSpec::Exact(NodeKind::IntConst(
            ConstId::from_u32(1),
        )));
        let _sum = b.binary(IntBinaryOp::Add, x, k);
        let p = b.finish();
        assert_eq!(p.graph.all_node_ids().count(), 3);
        assert_eq!(p.graph.all_value_ids().count(), 3);
    }
}
