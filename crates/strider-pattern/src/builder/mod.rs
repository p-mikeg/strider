//! Imperative builders over the bipartite [`Pattern`] store.
//!
//! [`MatcherBuilder`] is the single lowering target for every match-side
//! pattern API; [`TemplateBuilder`] is its build-side mirror. Both share
//! the same primitive surface (`leaf` / `unary` / `binary` / `node` +
//! `input` + the `*_output` verbs) plus the annotator surface (capture,
//! width, output type, node limit, post-match, force-ordered). The only
//! difference: each node a `TemplateBuilder` creates is stamped with a
//! [`TemplateKind`] build spec (and a [`TemplateTy`] output type) so the
//! finished pattern can be materialised as fresh IR by
//! [`instantiate`](crate::template::instantiate).
//!
//! The shared graph-wiring lives on a private [`BuilderCore`] both
//! builders delegate to, parameterised by whether to stamp the build
//! spec — there is one copy of the node/output/edge plumbing.

// `dead_code` allow: the annotator surface + node/output verbs are
// consumed by the typed-struct and control-builder API layers; this
// crate's lints run with `-D warnings`.
#![allow(dead_code)]

mod refs;

pub use refs::{PatNodeRef, PatOutRef};

use petgraph::stable_graph::NodeIndex;
use petgraph::visit::EdgeRef;
use strider_ir::IntBinaryOp;
use strider_ir::node::{NodeKind, NodeOutputType};

use crate::pattern::{KindSpec, OutputKindSpec, PatEdge, PatNode, PatOutput, PatVertex, Pattern};
use crate::template::{TemplateKind, TemplateTy};

// ── Shared wiring core ───────────────────────────────────────────────

/// The shared graph-wiring engine behind [`MatcherBuilder`] and
/// [`TemplateBuilder`]. Owns the [`Pattern`] under construction and the
/// node/output/edge plumbing both builders use. The match vs template
/// distinction is a single `template` flag: when set, each freshly
/// created node is stamped with a [`TemplateKind::Exact`] build spec
/// (templates that need a dynamic kind overwrite it afterwards).
struct BuilderCore {
    p: Pattern,
    /// Whether to stamp a build spec on each created node.
    template: bool,
}

impl BuilderCore {
    fn new(template: bool) -> Self {
        Self {
            p: Pattern::new(),
            template,
        }
    }

    /// The build spec derived from `kind` for a node created by this
    /// core. Only exact-kind nodes built on the template side carry a
    /// static build kind: `Variant` / `Any` / `VariantWith` specs have
    /// no concrete `NodeKind`, so a template using them must overwrite
    /// `build` with a `TemplateKind::Fn` (or supply a capture) —
    /// returning `None` here makes `instantiate` reject the
    /// un-buildable node with a clear error. Match-side cores
    /// (`template == false`) never stamp a build spec.
    ///
    /// Crucially this only *reads* `kind`; the original spec (predicate
    /// closure intact) is moved into the match node by the caller, so a
    /// `VariantWith` constraint is preserved at match time.
    fn build_spec_for(&self, kind: &KindSpec) -> Option<TemplateKind> {
        match kind {
            KindSpec::Exact(k) if self.template => Some(TemplateKind::Exact(*k)),
            _ => None,
        }
    }

    fn leaf(&mut self, kind: KindSpec) -> PatOutRef {
        let build = self.build_spec_for(&kind);
        let mut node = PatNode::from_kind(kind);
        node.build = build;
        let n = self.p.add_node(node);
        PatOutRef(self.p.add_output(n, PatOutput::value(0)))
    }

    fn unary(&mut self, kind: KindSpec, inner: PatOutRef) -> PatOutRef {
        let build = self.build_spec_for(&kind);
        let mut node = PatNode::from_kind(kind);
        node.build = build;
        let n = self.p.add_node(node);
        self.p.consume(n, 0, inner.0);
        PatOutRef(self.p.add_output(n, PatOutput::value(0)))
    }

    fn binary(&mut self, op: IntBinaryOp, l: PatOutRef, r: PatOutRef) -> PatOutRef {
        let kind = KindSpec::Exact(NodeKind::IntBinaryOp(op));
        let build = self.build_spec_for(&kind);
        let mut node = PatNode::from_kind(kind);
        node.build = build;
        let n = self.p.add_node(node);
        self.p.consume(n, 0, l.0);
        self.p.consume(n, 1, r.0);
        PatOutRef(self.p.add_output(n, PatOutput::value(0)))
    }

    fn node(&mut self, kind: KindSpec) -> PatNodeRef {
        let build = self.build_spec_for(&kind);
        let mut node = PatNode::from_kind(kind);
        node.build = build;
        let n = self.p.add_node(node);
        PatNodeRef(n)
    }

    fn input(&mut self, node: PatNodeRef, slot: usize, prod: PatOutRef) {
        self.p.consume(node.0, slot, prod.0);
    }

    fn value_output(&mut self, node: PatNodeRef, slot: usize) -> PatOutRef {
        PatOutRef(self.p.add_output(node.0, PatOutput::value(slot)))
    }

    fn control_output(&mut self, node: PatNodeRef, slot: usize) -> PatOutRef {
        PatOutRef(self.p.add_output(node.0, PatOutput::control(slot)))
    }

    fn memory_output(&mut self, node: PatNodeRef, slot: usize) -> PatOutRef {
        PatOutRef(self.p.add_output(node.0, PatOutput::memory(slot)))
    }

    fn phi_token_output(&mut self, node: PatNodeRef, slot: usize) -> PatOutRef {
        PatOutRef(self.p.add_output(node.0, PatOutput::phi_token(slot)))
    }

    /// The node index that produces output vertex `out`.
    #[allow(clippy::expect_used)]
    fn producing_node_idx(&self, out: NodeIndex) -> NodeIndex {
        self.p
            .inner
            .edges_directed(out, petgraph::Incoming)
            .find(|e| matches!(e.weight(), PatEdge::Produces))
            .map(|e| e.source())
            .expect("output vertex has a producer node")
    }

    #[allow(clippy::unreachable)]
    fn node_of(&mut self, out: PatOutRef) -> &mut PatNode {
        let pi = self.producing_node_idx(out.0);
        match self.p.inner.node_weight_mut(pi) {
            Some(PatVertex::Node(n)) => n,
            _ => unreachable!("producing node index resolves to a node vertex"),
        }
    }

    /// Mutable access to the `PatNode` weight at a node vertex handle.
    #[allow(clippy::unreachable)]
    fn node_at(&mut self, node: PatNodeRef) -> &mut PatNode {
        match self.p.inner.node_weight_mut(node.0) {
            Some(PatVertex::Node(n)) => n,
            _ => unreachable!("PatNodeRef references a node vertex"),
        }
    }

    #[allow(clippy::unreachable)]
    fn out_of(&mut self, out: PatOutRef) -> &mut PatOutput {
        match self.p.inner.node_weight_mut(out.0) {
            Some(PatVertex::Output(o)) => o,
            _ => unreachable!("PatOutRef references an output vertex"),
        }
    }

    fn constrain_input_widths(&mut self, out: PatOutRef, bits: u32) {
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

    #[allow(clippy::expect_used)]
    fn finish(mut self, root: PatOutRef) -> Pattern {
        let producer = self.producing_node_idx(root.0);
        self.p.set_root(producer);
        crate::pattern::assert_dag(&self.p.inner, producer).expect("builder produced a DAG");
        self.p
    }

    #[allow(clippy::expect_used)]
    fn finish_node(mut self, root: PatNodeRef) -> Pattern {
        self.p.set_root(root.0);
        crate::pattern::assert_dag(&self.p.inner, root.0).expect("builder produced a DAG");
        self.p
    }
}

// ── MatcherBuilder ───────────────────────────────────────────────────

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
    core: BuilderCore,
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
        Self {
            core: BuilderCore::new(false),
        }
    }

    /// The [`Pattern`] under construction.
    pub(crate) fn pattern_mut(&mut self) -> &mut Pattern {
        &mut self.core.p
    }

    /// A leaf node with the given kind spec and one value output at slot
    /// `0`.
    pub fn leaf(&mut self, kind: KindSpec) -> PatOutRef {
        self.core.leaf(kind)
    }

    /// A unary node of the given kind consuming `inner` at slot `0`,
    /// with one value output at slot `0`.
    pub fn unary(&mut self, kind: KindSpec, inner: PatOutRef) -> PatOutRef {
        self.core.unary(kind, inner)
    }

    /// A binary [`IntBinaryOp`] node consuming `l` at slot `0` and `r`
    /// at slot `1`, with one value output at slot `0`.
    pub fn binary(&mut self, op: IntBinaryOp, l: PatOutRef, r: PatOutRef) -> PatOutRef {
        self.core.binary(op, l, r)
    }

    /// A bare node with the given kind spec and no inputs/outputs yet.
    pub fn node(&mut self, kind: KindSpec) -> PatNodeRef {
        self.core.node(kind)
    }

    /// Wires `prod` into `node`'s input `slot`.
    pub fn input(&mut self, node: PatNodeRef, slot: usize, prod: PatOutRef) {
        self.core.input(node, slot, prod);
    }

    /// Adds a value output at `slot` to `node`.
    pub fn value_output(&mut self, node: PatNodeRef, slot: usize) -> PatOutRef {
        self.core.value_output(node, slot)
    }

    /// Adds a control output at `slot` to `node`.
    pub fn control_output(&mut self, node: PatNodeRef, slot: usize) -> PatOutRef {
        self.core.control_output(node, slot)
    }

    /// Adds a memory-token output at `slot` to `node`.
    pub fn memory_output(&mut self, node: PatNodeRef, slot: usize) -> PatOutRef {
        self.core.memory_output(node, slot)
    }

    /// Adds a phi-token output at `slot` to `node`.
    pub fn phi_token_output(&mut self, node: PatNodeRef, slot: usize) -> PatOutRef {
        self.core.phi_token_output(node, slot)
    }

    /// Pins `out`'s value output to an exact type.
    pub fn set_output_ty(&mut self, out: PatOutRef, ty: NodeOutputType) {
        self.core.out_of(out).kind = OutputKindSpec::Value(Some(ty));
    }

    /// Relaxes `out`'s declarative kind to match a control-flow output
    /// instead of a value output. Used by the control builders to wire a
    /// control-predecessor sub-pattern (`ctrl` / `preceded_by`) whose
    /// root produces a `Control` edge, not a value.
    pub fn set_output_control(&mut self, out: PatOutRef) {
        self.core.out_of(out).kind = OutputKindSpec::Control;
    }

    /// Pins `out`'s value-output bit width.
    pub fn set_output_width(&mut self, out: PatOutRef, bits: u32) {
        self.core.out_of(out).width = Some(bits);
    }

    /// Captures the node producing `out`.
    pub fn capture_node(&mut self, out: PatOutRef, c: crate::capture::Capture) {
        self.core.node_of(out).capture = Some(c);
    }

    /// Captures a node vertex directly (for zero-value-output roots like
    /// `Return` / `If` that have no value output to anchor on).
    pub fn capture_node_for(&mut self, node: PatNodeRef, c: crate::capture::Capture) {
        self.core.node_at(node).capture = Some(c);
    }

    /// Sets a node-local limit on the node producing `out`.
    pub fn set_node_limit(&mut self, out: PatOutRef, f: crate::pattern::LocalLimit) {
        self.core.node_of(out).node_limit = Some(f);
    }

    /// Sets a node-local limit on a node vertex directly.
    pub fn set_node_limit_for(&mut self, node: PatNodeRef, f: crate::pattern::LocalLimit) {
        self.core.node_at(node).node_limit = Some(f);
    }

    /// Sets a post-match hook on the node producing `out`.
    pub fn set_post_match(&mut self, out: PatOutRef, f: crate::pattern::PostMatchFn) {
        self.core.node_of(out).post_match = Some(f);
    }

    /// Disables commutative operand reordering for the node producing
    /// `out`.
    pub fn set_force_ordered(&mut self, out: PatOutRef) {
        self.core.node_of(out).force_ordered = true;
    }

    /// Sets `bits` as the width on every value input consumed by `out`'s
    /// producing node (the `inputs_of_width` primitive).
    pub fn constrain_input_widths(&mut self, out: PatOutRef, bits: u32) {
        self.core.constrain_input_widths(out, bits);
    }

    /// Seals the pattern with the node producing `root` as its root.
    #[must_use]
    pub fn finish(self, root: PatOutRef) -> Pattern {
        self.core.finish(root)
    }

    /// Seals the pattern with `root` (a node vertex) as its root.
    #[must_use]
    pub fn finish_node(self, root: PatNodeRef) -> Pattern {
        self.core.finish_node(root)
    }
}

// ── TemplateBuilder ──────────────────────────────────────────────────

/// Imperative builder for a build-side (template) [`Pattern`].
///
/// Mirrors [`MatcherBuilder`]'s primitive + annotator surface, but each
/// node it creates is stamped with a [`TemplateKind::Exact`] build spec
/// so the finished pattern is materialisable by
/// [`instantiate`](crate::template::instantiate). Nodes whose materialised
/// kind is computed at rewrite time (the `*_const_with` family) overwrite
/// the build spec with [`set_template_kind`](Self::set_template_kind).
pub struct TemplateBuilder {
    core: BuilderCore,
}

impl Default for TemplateBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TemplateBuilder {
    /// A template builder over an empty pattern.
    #[must_use]
    pub fn new() -> Self {
        Self {
            core: BuilderCore::new(true),
        }
    }

    /// A leaf node with the given kind spec and one value output.
    pub fn leaf(&mut self, kind: KindSpec) -> PatOutRef {
        self.core.leaf(kind)
    }

    /// A unary node consuming `inner`, with one value output.
    pub fn unary(&mut self, kind: KindSpec, inner: PatOutRef) -> PatOutRef {
        self.core.unary(kind, inner)
    }

    /// A binary [`IntBinaryOp`] node consuming `l` / `r`.
    pub fn binary(&mut self, op: IntBinaryOp, l: PatOutRef, r: PatOutRef) -> PatOutRef {
        self.core.binary(op, l, r)
    }

    /// A bare node with the given kind spec and no inputs/outputs yet.
    pub fn node(&mut self, kind: KindSpec) -> PatNodeRef {
        self.core.node(kind)
    }

    /// Wires `prod` into `node`'s input `slot`.
    pub fn input(&mut self, node: PatNodeRef, slot: usize, prod: PatOutRef) {
        self.core.input(node, slot, prod);
    }

    /// Adds a value output at `slot` to `node`.
    pub fn value_output(&mut self, node: PatNodeRef, slot: usize) -> PatOutRef {
        self.core.value_output(node, slot)
    }

    /// Adds a control output at `slot` to `node`.
    pub fn control_output(&mut self, node: PatNodeRef, slot: usize) -> PatOutRef {
        self.core.control_output(node, slot)
    }

    /// Adds a memory-token output at `slot` to `node`.
    pub fn memory_output(&mut self, node: PatNodeRef, slot: usize) -> PatOutRef {
        self.core.memory_output(node, slot)
    }

    /// Adds a phi-token output at `slot` to `node`.
    pub fn phi_token_output(&mut self, node: PatNodeRef, slot: usize) -> PatOutRef {
        self.core.phi_token_output(node, slot)
    }

    /// Pins `out`'s value output to an exact type and records it as a
    /// fixed build output type (so the materialised node is typed
    /// independently of the rewrite root).
    pub fn set_output_ty(&mut self, out: PatOutRef, ty: NodeOutputType) {
        self.core.out_of(out).kind = OutputKindSpec::Value(Some(ty));
        self.core.node_of(out).build_ty = TemplateTy::Fixed(ty);
    }

    /// Overwrites the build spec of the node producing `out` with a
    /// dynamic-kind closure (the `*_const_with` materialiser path).
    pub fn set_template_kind(&mut self, out: PatOutRef, kind: TemplateKind) {
        self.core.node_of(out).build = Some(kind);
    }

    /// Records the node producing `out` as inheriting the rewrite root's
    /// output type at instantiation time (the default).
    pub fn set_inherit_root_ty(&mut self, out: PatOutRef) {
        self.core.node_of(out).build_ty = TemplateTy::InheritRoot;
    }

    /// Captures the node producing `out`. On the template side a
    /// captured node resolves to its LHS binding at instantiation time;
    /// clearing its build spec marks it capture-only so `instantiate`
    /// takes the binding-resolution path.
    pub fn capture_node(&mut self, out: PatOutRef, c: crate::capture::Capture) {
        let n = self.core.node_of(out);
        n.capture = Some(c);
        n.build = None;
    }

    /// Seals the template with the node producing `root` as its root.
    #[must_use]
    pub fn finish(self, root: PatOutRef) -> Pattern {
        self.core.finish(root)
    }

    /// Seals the template with `root` (a node vertex) as its root.
    #[must_use]
    pub fn finish_node(self, root: PatNodeRef) -> Pattern {
        self.core.finish_node(root)
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

    #[test]
    fn template_builder_stamps_build_spec() {
        let mut b = TemplateBuilder::new();
        let k = b.leaf(crate::pattern::KindSpec::Exact(NodeKind::IntConst(2)));
        let p = b.finish(k);
        // The single node carries a build spec.
        let buildable = p
            .inner
            .node_weights()
            .filter_map(|v| match v {
                PatVertex::Node(n) => Some(n.build.is_some()),
                _ => None,
            })
            .all(|has_build| has_build);
        assert!(buildable);
    }
}
