//! The match-side imperative builder, [`MatcherBuilder`].
//!
//! [`MatcherBuilder`] is the single lowering target for every match-side
//! pattern API. Each verb wires one more node/output/edge into the
//! [`Pattern`] under construction and returns a handle into the store.
//! Its build-side mirror is
//! [`TemplateBuilder`](crate::template::TemplateBuilder), a separate
//! type over a separate graph — the two builders share no `template`
//! flag and expose only the verbs their respective sides need.

mod refs;

pub use refs::{PatNodeRef, PatOutRef};

use petgraph::stable_graph::NodeIndex;
use strider_ir::IntBinaryOp;
use strider_ir::node::{NodeKind, NodeOutputType};

use crate::pattern::{KindSpec, OutputKindSpec, PatNode, PatOutput, Pattern};

/// Imperative builder for a match-side [`Pattern`].
///
/// Owns a single [`Pattern`] under construction; each verb wires one
/// more node/output/edge and returns a handle into the store. Call
/// [`finish`](Self::finish) (value root) or
/// [`finish_node`](Self::finish_node) (zero-value-output root) to seal
/// the graph.
///
/// The returned [`PatOutRef`] / [`PatNodeRef`] handles are scoped to the
/// builder that produced them. Mixing handles across separate builder
/// instances will panic in `finish` / the annotators.
pub struct MatcherBuilder {
    p: Pattern,
}

impl Default for MatcherBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl MatcherBuilder {
    /// A builder over an empty pattern.
    #[must_use]
    pub fn new() -> Self {
        Self { p: Pattern::new() }
    }

    // ── construction verbs ───────────────────────────────────────────

    /// A leaf node with the given kind spec and one value output at slot
    /// `0`.
    pub fn leaf(&mut self, kind: KindSpec) -> PatOutRef {
        let n = self.p.add_node(PatNode::from_kind(kind));
        PatOutRef(self.p.add_output(n, PatOutput::value(0)))
    }

    /// A unary node of the given kind consuming `inner` at slot `0`,
    /// with one value output at slot `0`.
    pub fn unary(&mut self, kind: KindSpec, inner: PatOutRef) -> PatOutRef {
        let n = self.p.add_node(PatNode::from_kind(kind));
        self.p.consume(n, 0, inner.0);
        PatOutRef(self.p.add_output(n, PatOutput::value(0)))
    }

    /// A binary [`IntBinaryOp`] node consuming `l` at slot `0` and `r`
    /// at slot `1`, with one value output at slot `0`.
    pub fn binary(&mut self, op: IntBinaryOp, l: PatOutRef, r: PatOutRef) -> PatOutRef {
        let n = self
            .p
            .add_node(PatNode::exact(NodeKind::IntBinaryOp(op)));
        self.p.consume(n, 0, l.0);
        self.p.consume(n, 1, r.0);
        PatOutRef(self.p.add_output(n, PatOutput::value(0)))
    }

    /// A bare node with the given kind spec and no inputs/outputs yet.
    pub fn node(&mut self, kind: KindSpec) -> PatNodeRef {
        PatNodeRef(self.p.add_node(PatNode::from_kind(kind)))
    }

    /// Wires `prod` into `node`'s input `slot`.
    pub fn input(&mut self, node: PatNodeRef, slot: usize, prod: PatOutRef) {
        self.p.consume(node.0, slot, prod.0);
    }

    /// Adds a value output at `slot` to `node`.
    pub fn value_output(&mut self, node: PatNodeRef, slot: usize) -> PatOutRef {
        PatOutRef(self.p.add_output(node.0, PatOutput::value(slot)))
    }

    /// Adds a control output at `slot` to `node`.
    pub fn control_output(&mut self, node: PatNodeRef, slot: usize) -> PatOutRef {
        PatOutRef(self.p.add_output(node.0, PatOutput::control(slot)))
    }

    /// Adds a memory-token output at `slot` to `node`.
    pub fn memory_output(&mut self, node: PatNodeRef, slot: usize) -> PatOutRef {
        PatOutRef(self.p.add_output(node.0, PatOutput::memory(slot)))
    }

    // ── annotators ───────────────────────────────────────────────────

    /// Pins `out`'s value output to an exact type.
    pub fn set_output_ty(&mut self, out: PatOutRef, ty: NodeOutputType) {
        self.out_of(out).kind = OutputKindSpec::Value(Some(ty));
    }

    /// Relaxes `out`'s declarative kind to match a control-flow output
    /// instead of a value output. Used by the control builders to wire a
    /// control-predecessor sub-pattern (`ctrl` / `preceded_by`) whose
    /// root produces a `Control` edge, not a value.
    pub fn set_output_control(&mut self, out: PatOutRef) {
        self.out_of(out).kind = OutputKindSpec::Control;
    }

    /// Relaxes `out`'s declarative kind to the unconstrained wildcard
    /// ([`OutputKindSpec::Any`]) — matches any output kind, not just a
    /// value. Used by `any()` / `var()` so a bare wildcard matches any
    /// node, including value-less kinds (`Region`, `MemPhi`, …).
    pub fn set_output_any(&mut self, out: PatOutRef) {
        self.out_of(out).kind = OutputKindSpec::Any;
    }

    /// Pins `out`'s value-output bit width.
    pub fn set_output_width(&mut self, out: PatOutRef, bits: u32) {
        self.out_of(out).width = Some(bits);
    }

    /// Captures the node producing `out`.
    pub fn capture_node(&mut self, out: PatOutRef, c: crate::capture::Capture) {
        self.node_of(out).capture = Some(c);
    }

    /// Captures a node vertex directly (for zero-value-output roots like
    /// `Return` / `If` that have no value output to anchor on).
    pub fn capture_node_for(&mut self, node: PatNodeRef, c: crate::capture::Capture) {
        self.node_at(node).capture = Some(c);
    }

    /// Sets a node-local limit on the node producing `out`.
    pub fn set_node_limit(&mut self, out: PatOutRef, f: crate::pattern::LocalLimit) {
        self.node_of(out).node_limit = Some(f);
    }

    /// Sets a post-match hook on the node producing `out`.
    pub fn set_post_match(&mut self, out: PatOutRef, f: crate::pattern::PostMatchFn) {
        self.node_of(out).post_match = Some(f);
    }

    /// Disables commutative operand reordering for the node producing
    /// `out`.
    pub fn set_force_ordered(&mut self, out: PatOutRef) {
        self.node_of(out).force_ordered = true;
    }

    /// Sets `bits` as the width on every value input consumed by `out`'s
    /// producing node (the `inputs_of_width` primitive).
    pub fn constrain_input_widths(&mut self, out: PatOutRef, bits: u32) {
        let node = self.producing_node_idx(out.0);
        let input_outputs: Vec<NodeIndex> = self
            .p
            .graph
            .consumed_inputs(node)
            .map(|(_slot, io)| io)
            .collect();
        for io in input_outputs {
            if let Some(o) = self.p.graph.output_weight_mut(io) {
                o.width = Some(bits);
            }
        }
    }

    // ── sealing ──────────────────────────────────────────────────────

    /// Seals the pattern with the node producing `root` as its root.
    #[must_use]
    #[allow(clippy::expect_used)]
    pub fn finish(mut self, root: PatOutRef) -> Pattern {
        let producer = self.producing_node_idx(root.0);
        self.p.set_root(producer);
        crate::bigraph::assert_dag(&self.p.graph, producer).expect("builder produced a DAG");
        self.p
    }

    /// Seals the pattern with `root` (a node vertex) as its root.
    #[must_use]
    #[allow(clippy::expect_used)]
    pub fn finish_node(mut self, root: PatNodeRef) -> Pattern {
        self.p.set_root(root.0);
        crate::bigraph::assert_dag(&self.p.graph, root.0).expect("builder produced a DAG");
        self.p
    }

    // ── internal plumbing ────────────────────────────────────────────

    /// The node index that produces output vertex `out`.
    #[allow(clippy::expect_used)]
    fn producing_node_idx(&self, out: NodeIndex) -> NodeIndex {
        self.p
            .graph
            .producer_of(out)
            .expect("output vertex has a producer node")
    }

    #[allow(clippy::unreachable)]
    fn node_of(&mut self, out: PatOutRef) -> &mut PatNode {
        let pi = self.producing_node_idx(out.0);
        match self.p.graph.node_weight_mut(pi) {
            Some(n) => n,
            None => unreachable!("producing node index resolves to a node vertex"),
        }
    }

    /// Mutable access to the `PatNode` weight at a node vertex handle.
    #[allow(clippy::unreachable)]
    fn node_at(&mut self, node: PatNodeRef) -> &mut PatNode {
        match self.p.graph.node_weight_mut(node.0) {
            Some(n) => n,
            None => unreachable!("PatNodeRef references a node vertex"),
        }
    }

    #[allow(clippy::unreachable)]
    fn out_of(&mut self, out: PatOutRef) -> &mut PatOutput {
        match self.p.graph.output_weight_mut(out.0) {
            Some(o) => o,
            None => unreachable!("PatOutRef references an output vertex"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strider_ir::{IntBinaryOp, node::NodeKind};

    #[test]
    fn binary_builder_wires_two_inputs_and_one_output() {
        let mut b = MatcherBuilder::new();
        let x = b.leaf(crate::pattern::KindSpec::Any);
        let k = b.leaf(crate::pattern::KindSpec::Exact(NodeKind::IntConst(1)));
        let sum = b.binary(IntBinaryOp::Add, x, k);
        let p = b.finish(sum);
        assert_eq!(p.node_count(), 3);
        assert_eq!(p.output_count(), 3);
    }
}
