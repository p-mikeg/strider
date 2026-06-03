//! The build-side imperative builder, [`TemplateBuilder`].
//!
//! [`TemplateBuilder`] is the single lowering target for every
//! build-side ([`TemplatePat`](crate::template_pat::TemplatePat)) typed
//! struct. It exposes **construction verbs only** — `leaf` / `unary` /
//! `binary` / `node` / `input` + the `*_output` slot verbs + a
//! `capture_node` (for `var(c)` template nodes) + the dynamic-kind
//! `set_template_kind` / `set_template_ty` setters (for the
//! `*_const_with` materialiser path). It deliberately exposes **no match
//! verbs** (`set_node_predicate` / `set_value_width` / `set_force_ordered` /
//! `set_post_match` / predicate kindspecs): a [`Template`] is a build
//! recipe, not a query.
//!
//! Every node it creates carries a [`TemplateKind`] build spec (an exact
//! `NodeKind` by default; nodes whose kind is computed at rewrite time
//! overwrite it via [`set_template_kind`](TemplateBuilder::set_template_kind)),
//! so a finished [`Template`] is materialisable by
//! [`instantiate`](crate::template::instantiate) by construction.

use petgraph::stable_graph::NodeIndex;
use strider_ir::IntBinaryOp;
use strider_ir::node::{NodeKind, ValueType};

use crate::pattern::KindSpec;
use crate::template::graph::{Template, TmplNode, TmplOutput};
use crate::template::{TemplateKind, TemplateTy};

/// Handle to a template **output** vertex.
#[derive(Clone, Copy)]
pub struct TmplValueRef(pub(crate) NodeIndex);

/// Handle to a template **node** vertex.
#[derive(Clone, Copy)]
pub struct TmplNodeRef(pub(crate) NodeIndex);

/// Imperative builder for a build-side [`Template`].
///
/// Owns a single [`Template`] under construction; each verb wires one
/// more node/output/edge and returns a handle into the store. Call
/// [`finish`](Self::finish) to seal the graph with a value-producing
/// root.
///
/// The returned [`TmplValueRef`] / [`TmplNodeRef`] handles are scoped to
/// the builder that produced them, and are a distinct type from the
/// match side's `PatValueRef` / `PatNodeRef`, so the two builders' handles
/// cannot be crossed.
///
/// # Output-signature validity is author-owned
///
/// The high-level construction verbs (`leaf` / `unary` / `binary`) and the
/// typed `template::` free functions built on top of them declare canonical
/// output signatures and wire each input slot exactly once, so a
/// [`Template`] built that way always materialises a structurally-valid IR
/// node. The **raw** verbs — [`node`](Self::node) plus
/// [`input`](Self::input) and the `*_output` slot verbs — do not enforce
/// this: a hand-built node can declare an output signature that does not
/// match its `NodeKind`'s `expected_signature`, and wiring two producers
/// into the same input slot silently drops the earlier edge (inputs are
/// collected into a slot-keyed `BTreeMap` at instantiation). Because
/// [`instantiate`](crate::template::instantiate) does **not** run
/// [`strider_ir::validate`], such a malformed node is not caught. Authors
/// using the raw verbs own both invariants: declare an output signature
/// matching the `NodeKind`, and never wire two producers into one input
/// slot.
pub struct TemplateBuilder {
    t: Template,
}

impl Default for TemplateBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TemplateBuilder {
    /// A builder over an empty template.
    #[must_use]
    pub fn new() -> Self {
        Self { t: Template::new() }
    }

    // ── construction verbs ───────────────────────────────────────────

    /// A leaf node materialising the given exact `kind`, with one value
    /// output at slot `0`.
    pub fn leaf(&mut self, kind: KindSpec) -> TmplValueRef {
        let n = self.add_buildable(kind);
        TmplValueRef(self.t.graph.add_output(n, TmplOutput::value(0)))
    }

    /// A unary node materialising `kind`, consuming `inner` at slot `0`,
    /// with one value output at slot `0`.
    pub fn unary(&mut self, kind: KindSpec, inner: TmplValueRef) -> TmplValueRef {
        let n = self.add_buildable(kind);
        self.t.graph.consume(n, 0, inner.0);
        TmplValueRef(self.t.graph.add_output(n, TmplOutput::value(0)))
    }

    /// A binary [`IntBinaryOp`] node consuming `l` at slot `0` and `r`
    /// at slot `1`, with one value output at slot `0`.
    pub fn binary(&mut self, op: IntBinaryOp, l: TmplValueRef, r: TmplValueRef) -> TmplValueRef {
        let n = self.add_buildable(KindSpec::Exact(NodeKind::IntBinaryOp(op)));
        self.t.graph.consume(n, 0, l.0);
        self.t.graph.consume(n, 1, r.0);
        TmplValueRef(self.t.graph.add_output(n, TmplOutput::value(0)))
    }

    /// A bare node materialising `kind`, with no inputs/outputs yet.
    pub fn node(&mut self, kind: KindSpec) -> TmplNodeRef {
        TmplNodeRef(self.add_buildable(kind))
    }

    /// Wires `prod` into `node`'s input `slot`.
    pub fn input(&mut self, node: TmplNodeRef, slot: usize, prod: TmplValueRef) {
        self.t.graph.consume(node.0, slot, prod.0);
    }

    /// Adds a value output at `slot` to `node`.
    pub fn value_output(&mut self, node: TmplNodeRef, slot: usize) -> TmplValueRef {
        TmplValueRef(self.t.graph.add_output(node.0, TmplOutput::value(slot)))
    }

    /// Adds a memory-token output at `slot` to `node`.
    pub fn memory_output(&mut self, node: TmplNodeRef, slot: usize) -> TmplValueRef {
        TmplValueRef(self.t.graph.add_output(node.0, TmplOutput::memory(slot)))
    }

    /// Adds a control output at `slot` to `node`.
    pub fn control_output(&mut self, node: TmplNodeRef, slot: usize) -> TmplValueRef {
        TmplValueRef(self.t.graph.add_output(node.0, TmplOutput::control(slot)))
    }

    // ── annotators ───────────────────────────────────────────────────

    /// Pins `out`'s value output to a fixed build type (so the
    /// materialised node is typed independently of the rewrite root).
    pub fn set_value_ty(&mut self, out: TmplValueRef, ty: ValueType) {
        self.out_of(out).ty = TemplateTy::Fixed(ty);
    }

    /// Overwrites the build spec of the node producing `out` with a
    /// dynamic-kind closure (the `*_const_with` materialiser path).
    pub fn set_template_kind(&mut self, out: TmplValueRef, kind: TemplateKind) {
        self.node_of(out).kind = kind;
    }

    /// Records `out`'s value output as inheriting the rewrite root's
    /// output type at instantiation time (the default).
    pub fn set_inherit_root_ty(&mut self, out: TmplValueRef) {
        self.out_of(out).ty = TemplateTy::InheritRoot;
    }

    /// Captures the node producing `out`. On the build side a captured
    /// node resolves to its LHS binding at instantiation time; marking it
    /// capture-bearing makes `instantiate` take the binding-resolution
    /// path instead of synthesising the node.
    pub fn capture_node(&mut self, out: TmplValueRef, c: crate::capture::Capture) {
        self.node_of(out).capture = Some(c);
    }

    // ── sealing ──────────────────────────────────────────────────────

    /// Seals the template with the node producing `root` as its root.
    #[must_use]
    #[allow(clippy::expect_used)]
    pub fn finish(mut self, root: TmplValueRef) -> Template {
        let producer = self.producing_node_idx(root.0);
        self.t.graph.set_root(producer);
        crate::bigraph::assert_dag(&self.t.graph, producer).expect("builder produced a DAG");
        self.t
    }

    // ── internal plumbing ────────────────────────────────────────────

    /// Adds a buildable node stamped with a [`TemplateKind::Exact`] build
    /// spec derived from `kind` (for the exact-kind case). Non-exact
    /// specs (`Variant` / `Any` / `VariantWith`) have no concrete
    /// `NodeKind`, so the caller must overwrite the spec via
    /// [`set_template_kind`](Self::set_template_kind) before sealing.
    fn add_buildable(&mut self, kind: KindSpec) -> NodeIndex {
        // The only kinds a template ever passes are `Exact` (stamped
        // directly) and the `Variant`-shaped placeholders for `node()`
        // (whose `set_template_kind` overwrite lands before sealing).
        let spec = match kind {
            KindSpec::Exact(k) => TemplateKind::Exact(k),
            // Placeholder; overwritten by `set_template_kind`. Using an
            // arbitrary `NodeKind` keeps the field non-optional.
            _ => TemplateKind::Exact(NodeKind::IntConst(0)),
        };
        self.t.graph.add_node(TmplNode::buildable(spec))
    }

    /// The node index that produces output vertex `out`.
    #[allow(clippy::expect_used)]
    fn producing_node_idx(&self, out: NodeIndex) -> NodeIndex {
        self.t
            .graph
            .producer_of(out)
            .expect("output vertex has a producer node")
    }

    #[allow(clippy::unreachable)]
    fn node_of(&mut self, out: TmplValueRef) -> &mut TmplNode {
        let pi = self.producing_node_idx(out.0);
        match self.t.graph.node_weight_mut(pi) {
            Some(n) => n,
            None => unreachable!("producing node index resolves to a node vertex"),
        }
    }

    #[allow(clippy::unreachable)]
    fn out_of(&mut self, out: TmplValueRef) -> &mut TmplOutput {
        match self.t.graph.output_weight_mut(out.0) {
            Some(o) => o,
            None => unreachable!("TmplValueRef references an output vertex"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strider_ir::IntBinaryOp;
    use strider_ir::node::NodeKind;

    #[test]
    fn binary_builder_wires_two_inputs_and_one_output() {
        let mut b = TemplateBuilder::new();
        let x = b.leaf(KindSpec::Exact(NodeKind::IntConst(5)));
        let k = b.leaf(KindSpec::Exact(NodeKind::IntConst(1)));
        let sum = b.binary(IntBinaryOp::Add, x, k);
        let t = b.finish(sum);
        assert_eq!(t.node_count(), 3);
        assert_eq!(t.output_count(), 3);
        assert!(t.root().is_some());
    }
}
