//! [`strider_graph::Graph`] creates a node with all its outputs and resolves
//! its inputs at creation time, while the builder API is incremental: a bare
//! `node()` comes first, outputs are added later, and inputs are wired once
//! producers are compiled. So both builders stage into a [`StagedGraph`] and
//! materialise the whole DAG at [`finish`](MatcherBuilder::finish) in
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

    /// Matches if any alternative matches the node. The alternatives wire as
    /// this node's inputs, but the matcher tries each against the *same* IR
    /// node rather than as operands, enumerating every arm that matches (a
    /// union). Empty `alts` matches nothing.
    ///
    /// The alternation output is [`OutputKindSpec::Any`], so it nests in a
    /// value, control, or memory slot alike; the arms discriminate.
    ///
    /// Enumeration is multiplicative under
    /// [`find_all`](crate::Matcher::find_all); see [`crate::OneOf::new`].
    pub fn one_of(&mut self, alts: &[PatValueRef]) -> PatValueRef {
        self.alternation(alts, false)
    }

    /// Like [`Self::one_of`] but cuts to the first arm that yields a match
    /// instead of enumerating every match: an ordered choice, not a union.
    /// An arm rejected by a guard above the alternation produces no match, so
    /// the choice falls through to the next arm.
    pub fn first_of(&mut self, alts: &[PatValueRef]) -> PatValueRef {
        self.alternation(alts, true)
    }

    fn alternation(&mut self, alts: &[PatValueRef], first_match: bool) -> PatValueRef {
        let mut alt_node = PatNode::from_kind(KindSpec::Any);
        alt_node.alternation = true;
        alt_node.first_match = first_match;
        let n = self.stage(alt_node);
        for (slot, alt) in alts.iter().enumerate() {
            self.input(n, slot, *alt);
        }
        self.any_value_output(n)
    }

    pub fn input(&mut self, node: PatNodeRef, slot: usize, prod: PatValueRef) {
        self.core.add_input(node.0, slot, prod.node, prod.output);
    }

    pub fn value_output(&mut self, node: PatNodeRef, slot: usize) -> PatValueRef {
        self.add_output(node, PatValue::value(slot))
    }

    /// A slot-0 value output relaxed to [`OutputKindSpec::Any`]: the output an
    /// alternation node or a value-less kind synthesises.
    pub fn any_value_output(&mut self, node: PatNodeRef) -> PatValueRef {
        let out = self.value_output(node, 0);
        self.set_output_any(out);
        out
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

    /// An alternation's arms are matched against the same edge as the
    /// alternation itself, so retyping it to `Control` retypes the arms with
    /// it; a value-anchored arm would otherwise reject the control edge its own
    /// alternation just bound.
    ///
    /// Alone among the output-kind setters in recursing: the value-kind ones
    /// narrow what an arm's `AnyValue` vertex already accepts, while `Control`
    /// contradicts it. Recursion rewrites the arm vertices in place, so an
    /// operand pre-compiled once and fed to both a control and a value slot
    /// (`match_pat::Pre`) is retyped for both uses.
    pub fn set_output_control(&mut self, out: PatValueRef) {
        self.out_of(out).kind = OutputKindSpec::Control;
        if !self.core.kind_mut(out.node).alternation {
            return;
        }
        for (node, output) in self.core.input_producers(out.node) {
            self.set_output_control(PatValueRef { node, output });
        }
    }

    /// Relaxes `out` to [`OutputKindSpec::Any`], so `anything()` / `var()` match
    /// value-less kinds (`Region`, `MemPhi`, ...) too.
    pub fn set_output_any(&mut self, out: PatValueRef) {
        self.out_of(out).kind = OutputKindSpec::Any;
    }

    /// `any_output()`: an output vertex satisfied by any of the node's outputs.
    /// Unlike [`Self::any_value_output`], which relaxes the KIND of slot 0, this
    /// enumerates the slots, so a capture on it binds each in turn.
    ///
    /// Sibling `any_output` vertices on one node claim no slot from each other,
    /// so two of them can bind the same output; the existential `any_input`
    /// counterpart is injective.
    pub fn any_slot_value_output(&mut self, node: PatNodeRef) -> PatValueRef {
        let out = self.any_value_output(node);
        self.out_of(out).any_slot = true;
        out
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
    /// `int_add(var(x), ..)` captures `x`'s value, not its node.
    pub fn capture_output(&mut self, out: PatValueRef, c: crate::capture::Capture) {
        self.out_of(out).capture = Some(c);
    }

    /// Forces every slot consuming `out` to bind the SAME IR value. Wiring one
    /// vertex into two slots does not: the matcher visits them independently.
    /// See [`PatValue::identity`](crate::matcher::PatValue).
    pub(crate) fn pin_shared_identity(&mut self, out: PatValueRef) {
        self.out_of(out).identity = Some(crate::capture::Capture::internal());
    }

    /// Binds the matched node (`Binding::Node`), for zero-value-output roots
    /// like `Return` / `If`.
    pub fn capture_node(&mut self, node: PatNodeRef, c: crate::capture::Capture) {
        self.core.kind_mut(node.0).capture = Some(c);
    }

    /// Composes with any predicate already on `out`'s producer rather than
    /// replacing it: a builder installs its own core constraint through this
    /// slot (`stack_only`, `int_const(v)`, `phi_for(vn)`, ...), and a `.filter()`
    /// layered on top must narrow that, never delete it. Both are node-only, so
    /// evaluation order is immaterial.
    pub fn set_node_predicate(&mut self, out: PatValueRef, f: crate::matcher::NodePredicate) {
        self.set_node_predicate_at(PatNodeRef(out.node), f);
    }

    /// Node-keyed form of [`Self::set_node_predicate`], for a node-rooted
    /// pattern with no anchor output to address it through.
    pub fn set_node_predicate_at(&mut self, node: PatNodeRef, f: crate::matcher::NodePredicate) {
        let slot = &mut self.core.kind_mut(node.0).node_predicate;
        *slot = Some(match slot.take() {
            Some(prev) => Box::new(move |m, n| prev(m, n) && f(m, n)),
            None => f,
        });
    }

    /// `captures` declares what `f` binds; the sub-pattern it walks is not part
    /// of this graph, so the capture metadata has no other way to see it.
    /// See [`BindingWalkFn`](crate::matcher::BindingWalkFn).
    pub fn set_binding_walk(
        &mut self,
        out: PatValueRef,
        f: crate::matcher::BindingWalkFn,
        captures: crate::matcher::WalkCaptures,
    ) {
        let nd = self.core.kind_mut(out.node);
        nd.binding_walk = Some(f);
        nd.walk_captures = captures;
    }

    pub fn set_post_match(&mut self, out: PatValueRef, f: crate::matcher::PostMatchFn) {
        let slot = &mut self.core.kind_mut(out.node).post_match;
        *slot = Some(match slot.take() {
            Some(prev) => Box::new(move |m, n, ty, b| prev(m, n, ty, b) && f(m, n, ty, b)),
            None => f,
        });
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
    /// Single-rootedness is resolved here and reported at match time.
    ///
    /// # Panics
    /// On a cyclic staged graph (a builder bug).
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
