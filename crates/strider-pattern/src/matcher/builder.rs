use strider_ir::IntBinaryOp;
use strider_ir::node::{NodeKind, ValueType};

use crate::build_graph::BuildGraph;
use crate::matcher::{KindSpec, OutputKindSpec, PatNode, PatValue, Pattern, Role, SlotMode};
use strider_graph::{NodeId, ValueId};

/// A node's output, scoped to the builder that produced it. An IR value is
/// not one:
///
/// ```compile_fail,E0308
/// use strider_pattern::matcher::{KindSpec, MatcherBuilder};
/// let mut b = MatcherBuilder::new();
/// let n = b.node(KindSpec::Any);
/// b.input(n, 0, strider_ir::node::ValueId::from_u32(0));
/// ```
#[derive(Clone, Copy)]
pub struct PatValueRef(ValueId);

/// A node, scoped to the builder that produced it. An IR node is not one:
///
/// ```compile_fail,E0308
/// use strider_pattern::matcher::{KindSpec, MatcherBuilder};
/// let mut b = MatcherBuilder::new();
/// let x = b.leaf(KindSpec::Any);
/// b.input(strider_ir::node::NodeId::from_u32(1000), 0, x);
/// ```
#[derive(Clone, Copy)]
pub struct PatNodeRef(NodeId);

/// The match root is derived structurally, so the seal takes no root handle.
/// An input is wired from a node built before its consumer; any other wiring
/// is refused.
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

    /// Records a build-time refusal; every query on the sealed pattern then
    /// fails with the first one recorded.
    pub fn reject(&mut self, why: String) {
        self.core.reject(why);
    }

    /// One value output at slot `0`, no inputs.
    pub fn leaf(&mut self, kind: KindSpec) -> PatValueRef {
        let n = self.add_node(PatNode::from_kind(kind));
        self.value_output(n, 0)
    }

    /// Consumes `l` at slot `0` and `r` at slot `1`, one value output at
    /// slot `0`.
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

    /// Matches if any alternative matches the node. The alternatives wire as
    /// this node's inputs, but the matcher tries each against the *same* IR
    /// node rather than as operands, enumerating every arm that matches (a
    /// union). Empty `alts` matches nothing. `find_all` dedups on the binding
    /// signature, so arms that match one node identically are one row.
    ///
    /// The alternation output is the wildcard kind, so it nests in a
    /// value, control, or memory slot alike; the arms discriminate.
    ///
    /// Enumeration is multiplicative under
    /// [`find_all`](crate::Matcher::find_all); see [`crate::OneOf::new`].
    pub fn one_of(&mut self, alts: &[PatValueRef]) -> PatValueRef {
        self.alternation(alts, Role::OneOf)
    }

    /// Like [`Self::one_of`] but cuts to the first arm that yields a match
    /// instead of enumerating every match: an ordered choice, not a union.
    /// An arm rejected by a guard above the alternation produces no match, so
    /// the choice falls through to the next arm.
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

    /// Existential: `prod` matches *some* input of the node rather than a
    /// fixed one; the wiring records `InputSlot::Any` for that operand.
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

    pub fn value_output(&mut self, node: PatNodeRef, slot: usize) -> PatValueRef {
        self.add_output(node, PatValue::value(slot))
    }

    /// A slot-0 value output relaxed to the wildcard kind: the output an
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

    pub fn set_output_control(&mut self, out: PatValueRef) {
        self.out_of(out).kind = OutputKindSpec::Control;
    }

    /// Relaxes `out` to the wildcard kind, so `anything()` / `var()` match
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
        self.out_of(out).slot_mode = SlotMode::Enumerated;
        out
    }

    /// Widens an `AnyValue` output to also accept a valueless node's memory
    /// token.
    pub fn set_token_fallback(&mut self, out: PatValueRef) {
        self.out_of(out).token_fallback = true;
    }

    pub fn set_value_width(&mut self, out: PatValueRef, bits: u32) {
        self.out_of(out).width = Some(bits);
    }

    /// Pins `out` to its own slot: the matched output must be produced there.
    /// On an [`Self::any_slot_value_output`] vertex the pin replaces the
    /// enumeration.
    pub fn pin_value_out_slot(&mut self, out: PatValueRef) {
        self.out_of(out).slot_mode = SlotMode::Pinned;
    }

    /// Binds the matched output's value (`Binding::Value`), e.g.
    /// `int_add(var(x), ..)` captures `x`'s value, not its node. Additive:
    /// `var(x).capture(y)` binds both.
    pub fn capture_output(&mut self, out: PatValueRef, c: crate::capture::Capture) {
        self.out_of(out).captures.push(c);
    }

    /// Forces every slot consuming `out` to bind the SAME IR value. Wiring one
    /// vertex into two slots does not: the matcher visits them independently.
    /// See [`PatValue::identity`](crate::matcher::PatValue).
    pub(crate) fn pin_shared_identity(&mut self, out: PatValueRef) {
        self.out_of(out).identity = Some(crate::capture::Capture::internal());
    }

    /// Binds the matched node (`Binding::Node`), for zero-value-output roots
    /// like `Return` / `If`. Additive, like [`Self::capture_output`].
    pub fn capture_node(&mut self, node: PatNodeRef, c: crate::capture::Capture) {
        self.core.graph.node_kind_mut(node.0).captures.push(c);
    }

    /// Composes with any predicate already on `out`'s producer rather than
    /// replacing it: a builder installs its own core constraint through this
    /// slot (`stack_only`, `int_const(v)`, `phi_for(vn)`, ...), and a `.filter()`
    /// layered on top must narrow that, never delete it. Both are node-only, so
    /// evaluation order is immaterial.
    pub fn set_node_predicate(&mut self, out: PatValueRef, f: crate::matcher::NodePredicate) {
        let node = PatNodeRef(self.core.graph.producer(out.0));
        self.set_node_predicate_at(node, f);
    }

    /// Node-keyed form of [`Self::set_node_predicate`], for a node-rooted
    /// pattern with no anchor output to address it through.
    pub fn set_node_predicate_at(&mut self, node: PatNodeRef, f: crate::matcher::NodePredicate) {
        self.core
            .graph
            .node_kind_mut(node.0)
            .node_predicates
            .push(f);
    }

    /// [`Self::set_post_match`] for a guard that needs the matched output's
    /// `ValueType` (`when_match`). A control / memory / phi-token vertex or a
    /// kind with no value output can never supply one, so the guard would
    /// reject every match whatever it returns; refuse instead.
    pub(crate) fn set_post_match_typed(
        &mut self,
        out: PatValueRef,
        f: crate::matcher::PostMatchFn,
    ) {
        // The value-less kinds whose anchor output does not already say so. A
        // `Store` or `Region` is value-less too; its memory or control anchor
        // is what the output check catches.
        let untyped_kind = self.kind_of(out).kind.is_variant_in(&[
            NodeKind::If,
            NodeKind::Switch(strider_ir::node::SwitchTableId::from_u32(0)),
            NodeKind::Return,
            NodeKind::IndirectBranch,
            NodeKind::Unreachable,
        ]);
        let untyped_output = matches!(
            self.out_of(out).kind,
            OutputKindSpec::Control | OutputKindSpec::Memory
        );
        if untyped_kind || untyped_output {
            self.reject(
                "`when_match` is typed and this root produces no value output, so the \
                 guard could never run; use `Pattern::with_root_post_match`, which is \
                 handed the missing type"
                    .into(),
            );
        }
        self.set_post_match(out, f);
    }

    pub fn set_post_match(&mut self, out: PatValueRef, f: crate::matcher::PostMatchFn) {
        self.kind_of(out).post_matches.push(f);
    }

    /// Disables commutative operand reordering on `out`'s producer. Refused on
    /// an alternation: its inputs are alternatives, not operands, so nothing
    /// reads the flag.
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

    /// Single-rootedness is resolved here and reported at match time.
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
