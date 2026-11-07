use strider_ir::IntBinaryOp;
use strider_ir::node::{NodeKind, ValueType};

use crate::build_graph::BuildGraph;
use crate::matcher::{
    KindSpec, OUTPUTLESS, OutputKindSpec, PatNode, PatValue, Pattern, Role, SlotMode,
};
use strider_graph::{NodeId, ValueId};

/// A pattern node's output. Meaningful only in the builder that produced it;
/// an IR value is not one:
///
/// ```compile_fail,E0308
/// use strider_pattern::matcher::{KindSpec, MatcherBuilder};
/// let mut b = MatcherBuilder::new();
/// let n = b.node(KindSpec::Any);
/// b.input(n, 0, strider_ir::node::ValueId::from_u32(0));
/// ```
#[derive(Clone, Copy)]
pub struct PatValueRef(ValueId);

/// A pattern node. Meaningful only in the builder that produced it; an IR
/// node is not one:
///
/// ```compile_fail,E0308
/// use strider_pattern::matcher::{KindSpec, MatcherBuilder};
/// let mut b = MatcherBuilder::new();
/// let x = b.leaf(KindSpec::Any);
/// b.input(strider_ir::node::NodeId::from_u32(1000), 0, x);
/// ```
#[derive(Clone, Copy)]
pub struct PatNodeRef(NodeId);

/// Low-level pattern-graph builder. The root is derived at [`Self::finish`]; an input must come
/// from a node built before its consumer, or the wiring is refused.
#[derive(Default)]
pub struct MatcherBuilder {
    pub(crate) core: BuildGraph<PatNode, PatValue>,
}

impl MatcherBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `why` and lowers a wildcard in place of what was refused.
    pub(crate) fn refuse(&mut self, why: &str) -> PatValueRef {
        self.reject(why.to_string());
        self.leaf(KindSpec::Any)
    }

    /// Records a build-time refusal; queries on the sealed pattern fail with the first recorded.
    pub fn reject(&mut self, why: String) {
        self.core.reject(why);
    }

    /// One value output at slot `0`, no inputs.
    pub fn leaf(&mut self, kind: KindSpec) -> PatValueRef {
        let n = self.add_node(PatNode::from_kind(kind));
        self.value_output(n, 0)
    }

    /// Consumes `l` at slot `0` and `r` at slot `1`, one value output at slot `0`.
    pub fn binary(&mut self, op: IntBinaryOp, l: PatValueRef, r: PatValueRef) -> PatValueRef {
        let n = self.add_node(PatNode::exact(NodeKind::IntBinaryOp(op)));
        self.input(n, 0, l);
        self.input(n, 1, r);
        self.value_output(n, 0)
    }

    /// No inputs or outputs yet.
    pub fn node(&mut self, kind: KindSpec) -> PatNodeRef {
        self.add_node(PatNode::from_kind(kind))
    }

    /// A union: every arm is tried against the same IR node. Empty `alts` matches nothing. The
    /// output is the wildcard kind, so it fits any slot. See [`crate::OneOf::new`] for cost.
    pub fn one_of(&mut self, alts: &[PatValueRef]) -> PatValueRef {
        self.alternation(alts, Role::OneOf)
    }

    /// Like [`Self::one_of`] but an ordered choice: see [`crate::OneOf::first`]. Each anchored
    /// root output settles on its own.
    pub fn first_of(&mut self, alts: &[PatValueRef]) -> PatValueRef {
        self.alternation(alts, Role::FirstOf)
    }

    fn alternation(&mut self, alts: &[PatValueRef], role: Role) -> PatValueRef {
        let mut alt_node = PatNode::from_kind(KindSpec::Any);
        alt_node.role = role;
        let n = self.add_node(alt_node);
        for (slot, alt) in alts.iter().enumerate() {
            self.input(n, slot, *alt);
        }
        self.any_value_output(n)
    }

    pub fn input(&mut self, node: PatNodeRef, slot: usize, prod: PatValueRef) {
        self.wire(node, crate::matcher::InputSlot::At(slot), prod);
    }

    /// Existential: `prod` matches some input of the node, not a fixed one.
    pub fn input_any(&mut self, node: PatNodeRef, prod: PatValueRef) {
        self.wire(node, crate::matcher::InputSlot::Any, prod);
    }

    pub(crate) fn wire(
        &mut self,
        node: PatNodeRef,
        slot: crate::matcher::InputSlot,
        prod: PatValueRef,
    ) {
        if self.core.add_input(node.0, prod.0, false) {
            self.core.graph.node_kind_mut(node.0).input_slots.push(slot);
        }
    }

    /// Wiring is tree-like: each slot consuming `node`, through one output vertex or
    /// several, may match a different IR node. [`Self::capture_node`] ties them to one.
    pub fn value_output(&mut self, node: PatNodeRef, slot: usize) -> PatValueRef {
        self.add_output(node, PatValue::value(slot))
    }

    /// A slot-0 output of the wildcard kind.
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

    pub fn set_output_control(&mut self, out: PatValueRef) {
        self.out_of(out).kind = OutputKindSpec::Control;
    }

    /// Relaxes `out` to the wildcard kind, so `anything()` / `var()` match
    /// value-less kinds (`Region`, `MemPhi`, ...) too.
    pub fn set_output_any(&mut self, out: PatValueRef) {
        self.out_of(out).kind = OutputKindSpec::Any;
    }

    /// `any_output()`: an output vertex enumerating the node's outputs, a capture binding each in
    /// turn. Unlike `any_input`, sibling vertices may bind the same output.
    pub fn any_slot_value_output(&mut self, node: PatNodeRef) -> PatValueRef {
        let out = self.any_value_output(node);
        self.out_of(out).slot_mode = SlotMode::Enumerated;
        out
    }

    /// Widens an `AnyValue` output to also accept a valueless node's memory token.
    pub fn set_token_fallback(&mut self, out: PatValueRef) {
        self.out_of(out).token_fallback = true;
    }

    pub fn set_value_width(&mut self, out: PatValueRef, bits: u32) {
        self.out_of(out).width = Some(bits);
    }

    /// Pins `out` to its own slot, replacing an [`Self::any_slot_value_output`] enumeration.
    pub fn pin_value_out_slot(&mut self, out: PatValueRef) {
        self.out_of(out).slot_mode = SlotMode::Pinned;
    }

    /// Binds the matched output's value (`Binding::Value`). Additive: `var(x).capture(y)` binds
    /// both.
    pub fn capture_output(&mut self, out: PatValueRef, c: crate::capture::Capture) {
        self.out_of(out).captures.push(c);
    }

    /// Forces every slot consuming `out` to bind the same IR value, which wiring alone does not.
    /// Another output vertex of the same node is not tied to it.
    /// See [`PatValue::identity`](crate::matcher::PatValue).
    pub(crate) fn pin_shared_identity(&mut self, out: PatValueRef) {
        self.out_of(out).identity = Some(crate::capture::Capture::internal());
    }

    /// [`Self::capture_output`], except that an output-less producer (`Return`,
    /// `IndirectBranch`, `Unreachable`), also as an alternation arm, binds its node.
    pub(crate) fn capture(&mut self, out: PatValueRef, c: crate::capture::Capture) {
        let graph = &self.core.graph;
        let producer = graph.producer(out.0);
        let node = graph.node_kind(producer);
        if matches!(node.role, Role::OneOf | Role::FirstOf) {
            let arms: Vec<ValueId> = graph.node_inputs(producer).into_iter().collect();
            if arms.iter().any(|&arm| self.has_output_less_arm(arm)) {
                for arm in arms {
                    self.capture(PatValueRef(arm), c);
                }
                return;
            }
        } else if self.has_output_less_arm(out.0) {
            return self.capture_node(PatNodeRef(producer), c);
        }
        self.capture_output(out, c);
    }

    /// Whether `out`'s producer has no outputs, or is an alternation with such an arm.
    fn has_output_less_arm(&self, out: ValueId) -> bool {
        let graph = &self.core.graph;
        let producer = graph.producer(out);
        let node = graph.node_kind(producer);
        if matches!(node.role, Role::OneOf | Role::FirstOf) {
            return graph
                .node_inputs(producer)
                .into_iter()
                .any(|arm| self.has_output_less_arm(arm));
        }
        node.kind.is_variant_in(&OUTPUTLESS)
    }

    /// Binds the matched node (`Binding::Node`). Additive, like [`Self::capture_output`].
    pub fn capture_node(&mut self, node: PatNodeRef, c: crate::capture::Capture) {
        self.core.graph.node_kind_mut(node.0).captures.push(c);
    }

    /// Adds a node predicate to `out`'s producer, alongside any already there.
    pub fn set_node_predicate(&mut self, out: PatValueRef, f: crate::matcher::NodePredicate) {
        let node = PatNodeRef(self.core.graph.producer(out.0));
        self.set_node_predicate_at(node, f);
    }

    /// Node-keyed form of [`Self::set_node_predicate`].
    pub fn set_node_predicate_at(&mut self, node: PatNodeRef, f: crate::matcher::NodePredicate) {
        self.core
            .graph
            .node_kind_mut(node.0)
            .node_predicates
            .push(f);
    }

    /// [`Self::set_post_match`] for a guard needing the output's `ValueType` (`when_match`).
    /// Refused where `out` carries no value.
    pub(crate) fn set_post_match_typed(
        &mut self,
        out: PatValueRef,
        f: crate::matcher::PostMatchFn,
    ) {
        if self.valueless(out.0) {
            self.reject(
                "`when_match` is typed and this output carries no value, so the guard \
                 could never run; at a root, `Pattern::with_root_post_match` is handed \
                 the missing type"
                    .into(),
            );
        }
        self.set_post_match(out, f);
    }

    /// Whether `out` never carries a value: a valueless kind or output, or an
    /// alternation every arm of which is.
    fn valueless(&self, out: ValueId) -> bool {
        let graph = &self.core.graph;
        let producer = graph.producer(out);
        let node = graph.node_kind(producer);
        if matches!(node.role, Role::OneOf | Role::FirstOf) {
            let arms: Vec<ValueId> = graph.node_inputs(producer).into_iter().collect();
            return !arms.is_empty() && arms.into_iter().all(|arm| self.valueless(arm));
        }
        // Valueless kinds whose anchor output's kind does not already say so.
        node.kind.is_variant_in(&[
            NodeKind::If,
            NodeKind::Switch(strider_ir::node::SwitchTableId::from_u32(0)),
        ]) || node.kind.is_variant_in(&OUTPUTLESS)
            || matches!(
                graph.value_kind_ref(out).kind,
                OutputKindSpec::Control | OutputKindSpec::Memory
            )
    }

    pub fn set_post_match(&mut self, out: PatValueRef, f: crate::matcher::PostMatchFn) {
        self.kind_of(out).post_matches.push(f);
    }

    /// Disables commutative operand reordering on `out`'s producer. Refused on an alternation.
    pub fn set_force_ordered(&mut self, out: PatValueRef) {
        match &mut self.kind_of(out).role {
            Role::Operands { ordered } => *ordered = true,
            Role::OneOf | Role::FirstOf => self.reject(
                "`ordered` on an alternation does nothing: `one_of` / `first_of` arms are \
                 alternatives, not operands; put `.ordered()` on the arm whose operands \
                 must not swap"
                    .into(),
            ),
        }
    }

    /// Seals the pattern; a refusal or a rootless or multi-sink graph surfaces through
    /// [`Pattern::root`].
    pub fn finish(self) -> Pattern {
        Pattern::from_graph(self.core.graph, self.core.refusal)
    }

    fn add_node(&mut self, kind: PatNode) -> PatNodeRef {
        PatNodeRef(self.core.add_node(kind))
    }

    fn add_output(&mut self, node: PatNodeRef, out: PatValue) -> PatValueRef {
        PatValueRef(self.core.add_output(node.0, out))
    }

    fn out_of(&mut self, out: PatValueRef) -> &mut PatValue {
        self.core.graph.value_kind_mut(out.0)
    }

    fn kind_of(&mut self, out: PatValueRef) -> &mut PatNode {
        let node = self.core.graph.producer(out.0);
        self.core.graph.node_kind_mut(node)
    }
}

#[cfg(test)]
mod tests {
    use super::MatcherBuilder;

    /// One lowering level whose frame alone is 16 KiB, so the stack budget
    /// trips well inside the level cap.
    #[inline(never)]
    fn descend(b: &mut MatcherBuilder, levels: &mut usize) {
        let pad = std::hint::black_box([0u8; 16 * 1024]);
        if b.core.enter_nesting(true) {
            *levels += 1;
            descend(b, levels);
            b.core.leave_nesting(true);
        }
        std::hint::black_box(&pad);
    }

    #[test]
    fn a_fat_frame_per_level_trips_the_stack_budget_inside_the_level_cap() {
        let (levels, refusal) = std::thread::Builder::new()
            .stack_size(64 << 20)
            .spawn(|| {
                let mut b = MatcherBuilder::new();
                let mut levels = 0;
                descend(&mut b, &mut levels);
                (levels, b.finish().root().unwrap_err().to_string())
            })
            .expect("spawn")
            .join()
            .expect("join");
        assert!(
            levels < crate::matcher::graph::MAX_PATTERN_NODES,
            "{levels} levels"
        );
        assert!(
            refusal.contains("of a 1024 KiB lowering budget"),
            "{refusal}"
        );
    }
}
