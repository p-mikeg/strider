//! The match-side imperative builder, [`MatcherBuilder`].
//!
//! [`MatcherBuilder`] is the single lowering target for every match-side
//! pattern API. Each verb wires one more node/output/edge into the pattern
//! under construction and returns a handle into the build staging area.
//! Its build-side mirror is
//! [`TemplateBuilder`](crate::template::TemplateBuilder), a separate type
//! over a separate graph — the two builders share no `template` flag and
//! expose only the verbs their respective sides need.
//!
//! ## Why staging
//!
//! The generic [`strider_graph::Graph`] creates a node together with all of
//! its outputs and resolves its inputs at creation time (no incremental
//! output addition). The builder API, by contrast, is incremental — a bare
//! `node()` is created, its value outputs added later, and its inputs wired
//! after the producers have been compiled. Both builders therefore share a
//! [`StagedGraph`] staging core that stages every node and materialises the
//! whole DAG into the generic graph at [`finish`](MatcherBuilder::finish), in
//! producer-before-consumer order (`petgraph::algo::toposort`). The sparse
//! consumer slot of each input is recorded on the materialised [`PatNode`]'s
//! `input_slots` via the [`SealNode`] bridge (see [`crate::graph_ext`]).

use strider_ir::IntBinaryOp;
use strider_ir::node::{NodeKind, ValueType};

use crate::matcher::{KindSpec, OutputKindSpec, PatNode, PatValue, Pattern};
use crate::staging::{SealNode, StagedGraph};

/// Handle to a pattern **value** vertex — a value/control output a
/// downstream node can consume. Returned by [`MatcherBuilder`] while
/// wiring a pattern graph; names a staged node's output by position.
#[derive(Clone, Copy)]
pub struct PatValueRef {
    node: usize,
    output: usize,
}

/// Handle to a pattern **node** vertex — used by the variadic / control
/// builders that wire inputs and outputs by hand. Names a staged node by
/// position.
#[derive(Clone, Copy)]
pub struct PatNodeRef(pub(crate) usize);

/// Seals a staged [`PatNode`] into its materialised form by stamping the
/// recovered consumer-slot list onto its `input_slots` field (the match side
/// stores its sealed payload in the same `PatNode` type).
impl SealNode for PatNode {
    type Sealed = PatNode;
    fn seal(mut self, input_slots: Vec<usize>) -> PatNode {
        self.input_slots = input_slots;
        self
    }
}

/// Imperative builder for a match-side [`Pattern`].
///
/// Stages each node verb into a shared `StagedGraph` and materialises the
/// whole DAG at [`finish`](Self::finish). The match root is derived
/// structurally, so the seal takes no root handle.
///
/// The returned [`PatValueRef`] / [`PatNodeRef`] handles are scoped to the
/// builder that produced them.
pub struct MatcherBuilder {
    core: StagedGraph<PatNode, PatValue>,
}

impl Default for MatcherBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl MatcherBuilder {
    /// A builder over an empty pattern.
    pub fn new() -> Self {
        Self {
            core: StagedGraph::new(),
        }
    }

    // ── construction verbs ───────────────────────────────────────────

    /// A leaf node with the given kind spec and one value output at slot
    /// `0`.
    pub fn leaf(&mut self, kind: KindSpec) -> PatValueRef {
        let n = self.stage(PatNode::from_kind(kind));
        self.value_output(n, 0)
    }

    /// A unary node of the given kind consuming `inner` at slot `0`,
    /// with one value output at slot `0`.
    pub fn unary(&mut self, kind: KindSpec, inner: PatValueRef) -> PatValueRef {
        let n = self.stage(PatNode::from_kind(kind));
        self.input(n, 0, inner);
        self.value_output(n, 0)
    }

    /// A binary [`IntBinaryOp`] node consuming `l` at slot `0` and `r`
    /// at slot `1`, with one value output at slot `0`.
    pub fn binary(&mut self, op: IntBinaryOp, l: PatValueRef, r: PatValueRef) -> PatValueRef {
        let n = self.stage(PatNode::exact(NodeKind::IntBinaryOp(op)));
        self.input(n, 0, l);
        self.input(n, 1, r);
        self.value_output(n, 0)
    }

    /// A bare node with the given kind spec and no inputs/outputs yet.
    pub fn node(&mut self, kind: KindSpec) -> PatNodeRef {
        self.stage(PatNode::from_kind(kind))
    }

    /// Wires `prod` into `node`'s input `slot`.
    pub fn input(&mut self, node: PatNodeRef, slot: usize, prod: PatValueRef) {
        self.core.add_input(node.0, slot, prod.node, prod.output);
    }

    /// Adds a value output at `slot` to `node`.
    pub fn value_output(&mut self, node: PatNodeRef, slot: usize) -> PatValueRef {
        self.add_output(node, PatValue::value(slot))
    }

    /// Adds a control output at `slot` to `node`.
    pub fn control_output(&mut self, node: PatNodeRef, slot: usize) -> PatValueRef {
        self.add_output(node, PatValue::control(slot))
    }

    /// Adds a memory-token output at `slot` to `node`.
    pub fn memory_output(&mut self, node: PatNodeRef, slot: usize) -> PatValueRef {
        self.add_output(node, PatValue::memory(slot))
    }

    // ── annotators ───────────────────────────────────────────────────

    /// Pins `out`'s value output to an exact type.
    pub fn set_value_ty(&mut self, out: PatValueRef, ty: ValueType) {
        self.out_of(out).kind = OutputKindSpec::Value(ty);
    }

    /// Relaxes `out`'s declarative kind to match a control-flow output
    /// instead of a value output. Used by the control builders to wire a
    /// control-predecessor sub-pattern (`ctrl` / `preceded_by`) whose
    /// root produces a `Control` edge, not a value.
    pub fn set_output_control(&mut self, out: PatValueRef) {
        self.out_of(out).kind = OutputKindSpec::Control;
    }

    /// Relaxes `out`'s declarative kind to the unconstrained wildcard
    /// ([`OutputKindSpec::Any`]) — matches any output kind, not just a
    /// value. Used by `any()` / `var()` so a bare wildcard matches any
    /// node, including value-less kinds (`Region`, `MemPhi`, …).
    pub fn set_output_any(&mut self, out: PatValueRef) {
        self.out_of(out).kind = OutputKindSpec::Any;
    }

    /// Pins `out`'s value-output bit width.
    pub fn set_value_width(&mut self, out: PatValueRef, bits: u32) {
        self.out_of(out).width = Some(bits);
    }

    /// Enforces that the matched value is produced at output slot `slot`
    /// (see [`PatValue::match_slot`]). Used to pin a nested Call/CallOther
    /// value operand to a specific output (`.res()`).
    pub fn set_value_out_slot(&mut self, out: PatValueRef, slot: usize) {
        self.out_of(out).match_slot = Some(slot);
    }

    /// Captures the output vertex `out` — a value capture, bound to the
    /// matched output's value (`Binding::Value`). This is the common case:
    /// `add(var(x), …)` captures `x`'s *value*, not its node.
    pub fn capture_output(&mut self, out: PatValueRef, c: crate::capture::Capture) {
        self.out_of(out).capture = Some(c);
    }

    /// Captures a node vertex directly (for zero-value-output roots like
    /// `Return` / `If` that have no value output to anchor a value capture
    /// on); bound to the matched node (`Binding::Node`).
    pub fn capture_node(&mut self, node: PatNodeRef, c: crate::capture::Capture) {
        self.core.kind_mut(node.0).capture = Some(c);
    }

    /// Sets a node predicate on the node producing `out`.
    pub fn set_node_predicate(&mut self, out: PatValueRef, f: crate::matcher::NodePredicate) {
        self.core.kind_mut(out.node).node_predicate = Some(f);
    }

    /// Sets a post-match hook on the node producing `out`.
    pub fn set_post_match(&mut self, out: PatValueRef, f: crate::matcher::PostMatchFn) {
        self.core.kind_mut(out.node).post_match = Some(f);
    }

    /// Disables commutative operand reordering for the node producing
    /// `out`.
    pub fn set_force_ordered(&mut self, out: PatValueRef) {
        self.core.kind_mut(out.node).force_ordered = true;
    }

    /// Sets `bits` as the width on every value input consumed by `out`'s
    /// producing node (the `inputs_of_width` primitive).
    pub fn constrain_input_widths(&mut self, out: PatValueRef, bits: u32) {
        for (pn, po) in self.core.input_producers(out.node) {
            self.core.output_mut(pn, po).width = Some(bits);
        }
    }

    // ── sealing ──────────────────────────────────────────────────────

    /// Seals the staged graph into a [`Pattern`].
    ///
    /// Materialises every staged node into the generic graph in
    /// producer-before-consumer order. The seal performs **no** structural
    /// validation: whether the result is a single-rooted, acyclic shape the
    /// matcher can handle is resolved (and reported as an error) at match
    /// time, not here.
    ///
    /// # Panics
    /// Panics on a cyclic staged graph (a pattern is always a DAG; a cycle
    /// would be a builder bug, not a user error).
    #[allow(clippy::expect_used)]
    pub fn finish(self) -> Pattern {
        let graph = self.core.seal().expect("cyclic staged pattern graph");
        Pattern::from_graph(graph)
    }

    // ── internal plumbing ────────────────────────────────────────────

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
