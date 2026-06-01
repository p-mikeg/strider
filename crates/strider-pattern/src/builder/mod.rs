//! Imperative builder over the bipartite [`Pattern`] store.
//!
//! [`MatcherBuilder`] is the single lowering target for every match-side
//! pattern API: the typed `MatchPat` structs and the fluent control
//! builders both lower onto these primitives. It exposes the raw
//! graph-wiring verbs ([`leaf`](MatcherBuilder::leaf),
//! [`unary`](MatcherBuilder::unary), [`binary`](MatcherBuilder::binary),
//! [`node`](MatcherBuilder::node) + [`input`](MatcherBuilder::input) +
//! the `*_output` verbs) plus the annotator surface (capture, width,
//! output type, node limit, post-match, force-ordered) that the API
//! layers call to decorate a freshly-wired sub-pattern.

// `dead_code` allow: the annotator surface + node/output verbs are
// consumed by the typed-struct and control-builder API layers landing in
// later changes; this crate's lints run with `-D warnings`.
#![allow(dead_code)]

mod refs;

pub use refs::{PatNodeRef, PatOutRef};

use petgraph::stable_graph::NodeIndex;
use petgraph::visit::EdgeRef;
use strider_ir::IntBinaryOp;
use strider_ir::node::{NodeKind, NodeOutputType};

use crate::pattern::{KindSpec, OutputKindSpec, PatEdge, PatNode, PatOutput, PatVertex, Pattern};

/// Imperative builder for a [`Pattern`].
///
/// Owns a single [`Pattern`] under construction; each verb wires one
/// more node/output/edge and returns a handle into the store. Call
/// [`finish`](Self::finish) (value root) or
/// [`finish_node`](Self::finish_node) (zero-value-output root) to seal
/// the graph.
///
/// The returned [`PatOutRef`] / [`PatNodeRef`] handles are scoped to the
/// builder that produced them — they index that builder's store. Mixing
/// handles across separate `MatcherBuilder` instances will panic in
/// `finish` / the annotators.
pub struct MatcherBuilder {
    pub(crate) p: Pattern,
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
        let n = self.p.add_node(PatNode::exact(NodeKind::IntBinaryOp(op)));
        self.p.consume(n, 0, l.0);
        self.p.consume(n, 1, r.0);
        PatOutRef(self.p.add_output(n, PatOutput::value(0)))
    }

    /// A bare node with the given kind spec and no inputs/outputs yet.
    /// Used by the variadic / control builders that wire inputs and
    /// outputs by hand.
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

    /// The node index that produces output vertex `out`.
    ///
    /// Every output vertex is wired with a `Produces` edge from its
    /// node at creation (see [`Pattern::add_output`]), so the lookup is
    /// total over builder-created handles.
    #[allow(clippy::expect_used)]
    fn producing_node_idx(&self, out: NodeIndex) -> NodeIndex {
        self.p
            .inner
            .edges_directed(out, petgraph::Incoming)
            .find(|e| matches!(e.weight(), PatEdge::Produces))
            .map(|e| e.source())
            .expect("output vertex has a producer node")
    }

    /// Mutable borrow of the node producing `out`.
    #[allow(clippy::unreachable)]
    pub(crate) fn node_of(&mut self, out: PatOutRef) -> &mut PatNode {
        let pi = self.producing_node_idx(out.0);
        match self.p.inner.node_weight_mut(pi) {
            Some(PatVertex::Node(n)) => n,
            _ => unreachable!("producing node index resolves to a node vertex"),
        }
    }

    /// Mutable borrow of the output vertex `out`.
    #[allow(clippy::unreachable)]
    pub(crate) fn out_of(&mut self, out: PatOutRef) -> &mut PatOutput {
        match self.p.inner.node_weight_mut(out.0) {
            Some(PatVertex::Output(o)) => o,
            _ => unreachable!("PatOutRef references an output vertex"),
        }
    }

    /// Pins `out`'s value output to an exact type.
    pub fn set_output_ty(&mut self, out: PatOutRef, ty: NodeOutputType) {
        self.out_of(out).kind = OutputKindSpec::Value(Some(ty));
    }

    /// Pins `out`'s value-output bit width.
    pub fn set_output_width(&mut self, out: PatOutRef, bits: u32) {
        self.out_of(out).width = Some(bits);
    }

    /// Captures the node producing `out`.
    pub fn capture_node(&mut self, out: PatOutRef, c: crate::capture::Capture) {
        self.node_of(out).capture = Some(c);
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
            .inner
            .edges_directed(node, petgraph::Incoming)
            .filter(|e| matches!(e.weight(), PatEdge::Consumes { .. }))
            .map(|e| e.source())
            .collect();
        for io in input_outputs {
            if let Some(PatVertex::Output(o)) = self.p.inner.node_weight_mut(io) {
                o.width = Some(bits);
            }
        }
    }

    /// Seals the pattern with the node producing `root` as its root and
    /// returns the finished [`Pattern`].
    #[must_use]
    #[allow(clippy::expect_used)]
    pub fn finish(mut self, root: PatOutRef) -> Pattern {
        let producer = self.producing_node_idx(root.0);
        self.p.set_root(producer);
        crate::pattern::assert_dag(&self.p.inner, producer).expect("builder produced a DAG");
        self.p
    }

    /// Seals the pattern with `root` (a node vertex, typically a
    /// zero-value-output kind) as its root and returns the finished
    /// [`Pattern`].
    #[must_use]
    #[allow(clippy::expect_used)]
    pub fn finish_node(mut self, root: PatNodeRef) -> Pattern {
        self.p.set_root(root.0);
        crate::pattern::assert_dag(&self.p.inner, root.0).expect("builder produced a DAG");
        self.p
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
