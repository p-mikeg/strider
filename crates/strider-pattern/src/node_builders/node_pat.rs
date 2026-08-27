use std::mem::Discriminant;

use strider_ir::node::{NodeKind, ValueType};

use crate::capture::Capture;
use crate::matcher::match_pat::MatchPat;
use crate::matcher::{KindSpec, MatcherBuilder, NodePredicate, PatNodeRef, PatValueRef, Pattern};

use super::{MemPat, SubCompiler};

/// The anchor is the output a node predicate and capture attach to; it also
/// doubles as the chaining handle for memory roots and the seal handle for
/// value roots.
#[derive(Clone, Copy)]
pub(crate) enum AnchorKind {
    Value(usize),
    Memory(usize),
    Control(usize),
    /// No output vertex at all: captures attach to the node directly and no
    /// node predicate can be anchored.
    None,
}

type NodePredicateFactory = Box<dyn FnOnce() -> NodePredicate>;

/// Binds and/or kind-constrains the value produced at output `slot`, or at
/// some output when `slot` is `None`. Defaults to the wildcard kind
/// ([`OutputKindSpec::Any`](crate::matcher::OutputKindSpec::Any)); `ty`
/// narrows to an exact value type, `width` to a bit width.
#[derive(Clone, Copy)]
pub(crate) struct OutputSpec {
    slot: Option<usize>,
    capture: Option<Capture>,
    ty: Option<ValueType>,
    width: Option<u32>,
}

impl OutputSpec {
    fn at(slot: Option<usize>) -> Self {
        Self {
            slot,
            capture: None,
            ty: None,
            width: None,
        }
    }
}

pub(crate) struct NodePat {
    kind: KindSpec,
    /// Sparse: raw input slot to compiler.
    inputs: Vec<(usize, SubCompiler)>,
    node_predicate: Option<NodePredicateFactory>,
    capture: Option<Capture>,
    anchor: AnchorKind,
    output_width: Option<u32>,
    /// `(slot, bits)`.
    input_widths: Vec<(usize, u32)>,
    /// Requires the matched value to be produced at exactly the anchor slot
    /// (see [`crate::matcher::PatValue::match_slot`]).
    pin_anchor_slot: bool,
    outputs: Vec<OutputSpec>,
}

impl NodePat {
    pub(crate) fn node(kind: KindSpec) -> Self {
        Self {
            kind,
            inputs: Vec::new(),
            node_predicate: None,
            capture: None,
            anchor: AnchorKind::None,
            output_width: None,
            input_widths: Vec::new(),
            pin_anchor_slot: false,
            outputs: Vec::new(),
        }
    }

    /// Nests as a value operand.
    pub(crate) fn value(kind: KindSpec, slot: usize) -> Self {
        let mut s = Self::node(kind);
        s.anchor = AnchorKind::Value(slot);
        s
    }

    /// Narrows the kind, e.g. to a `VariantWith` payload check.
    pub(crate) fn with_kind(mut self, kind: KindSpec) -> Self {
        self.kind = kind;
        self
    }

    /// Anchors on the memory output at `slot`.
    pub(crate) fn with_mem_value(mut self, slot: usize) -> Self {
        self.anchor = AnchorKind::Memory(slot);
        self
    }

    /// Anchors on the control output at `slot`, so the builder can nest as a
    /// control operand.
    pub(crate) fn with_control_value(mut self, slot: usize) -> Self {
        self.anchor = AnchorKind::Control(slot);
        self
    }

    /// Anchors on the value output at `slot`, so a normally memory-rooted node
    /// can nest as a value operand, as in `int_add(x, call_other().name("f"))`.
    pub(crate) fn with_value_anchor(mut self, slot: usize) -> Self {
        self.anchor = AnchorKind::Value(slot);
        self
    }

    /// No-op unless the anchor is [`AnchorKind::Value`]. See
    /// [`crate::matcher::PatValue::match_slot`].
    pub(crate) fn pin_anchor_slot(mut self) -> Self {
        self.pin_anchor_slot = true;
        self
    }

    /// Wires `p` into raw input `slot`.
    pub(crate) fn input<P: MatchPat + 'static>(mut self, slot: usize, p: P) -> Self {
        self.inputs.push((slot, Box::new(move |b| p.compile(b))));
        self
    }

    /// Existential: matches *some* input of the node rather than a fixed slot.
    pub(crate) fn input_any<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.inputs.push((
            crate::matcher::ANY_INPUT_SLOT,
            Box::new(move |b| p.compile(b)),
        ));
        self
    }

    /// Relaxes the sub-pattern's root output to a control edge.
    pub(crate) fn input_control<P: MatchPat + 'static>(mut self, slot: usize, p: P) -> Self {
        self.inputs.push((slot, control_compiler(p)));
        self
    }

    /// The sub-pattern's memory-token output feeds the slot.
    pub(crate) fn input_mem<M: MemPat + 'static>(mut self, slot: usize, p: M) -> Self {
        self.inputs
            .push((slot, Box::new(move |b| p.compile_mem(b))));
        self
    }

    /// The predicate short-circuits before child recursion. Composes with one
    /// already staged, like
    /// [`MatcherBuilder::set_node_predicate_at`](crate::matcher::MatcherBuilder::set_node_predicate_at).
    pub(crate) fn with_node_predicate<F>(mut self, f: F) -> Self
    where
        F: FnOnce() -> NodePredicate + 'static,
    {
        self.node_predicate = Some(match self.node_predicate.take() {
            Some(prev) => Box::new(move || {
                let (prev, next) = (prev(), f());
                Box::new(move |m, n| prev(m, n) && next(m, n))
            }),
            None => Box::new(f),
        });
        self
    }

    /// No-op unless the anchor is [`AnchorKind::Value`].
    pub(crate) fn with_output_width(mut self, bits: u32) -> Self {
        self.output_width = Some(bits);
        self
    }

    /// Constrains the sub-pattern at `slot` to that output width.
    pub(crate) fn with_input_width(mut self, slot: usize, bits: u32) -> Self {
        self.input_widths.push((slot, bits));
        self
    }

    /// Binds the anchor output where there is one, otherwise the node.
    pub(crate) fn capture(mut self, c: Capture) -> Self {
        self.capture = Some(c);
        self
    }

    /// Binds a *sibling* output of the anchor. A leaf: it captures the output
    /// value itself and does not recurse into what that output feeds.
    pub(crate) fn capture_output(mut self, slot: Option<usize>, c: Capture) -> Self {
        self.outputs.push(OutputSpec {
            capture: Some(c),
            ..OutputSpec::at(slot)
        });
        self
    }

    /// Implies a value output of that width.
    pub(crate) fn output_width(mut self, slot: Option<usize>, bits: u32) -> Self {
        self.outputs.push(OutputSpec {
            width: Some(bits),
            ..OutputSpec::at(slot)
        });
        self
    }

    pub(crate) fn output_ty(mut self, slot: Option<usize>, ty: ValueType) -> Self {
        self.outputs.push(OutputSpec {
            ty: Some(ty),
            ..OutputSpec::at(slot)
        });
        self
    }

    /// Lowers into `b`, returning the staged node and its anchor output (the
    /// value/memory/control output this pattern nests through, or `None` for a
    /// node-rooted kind like `Return` / `If`).
    pub(crate) fn lower(self, b: &mut MatcherBuilder) -> (PatNodeRef, Option<PatValueRef>) {
        let NodePat {
            kind,
            inputs,
            node_predicate,
            capture,
            anchor,
            output_width,
            input_widths,
            pin_anchor_slot,
            outputs,
        } = self;
        let node = b.node(kind);
        let anchor_out = match anchor {
            AnchorKind::Value(slot) => {
                let out = b.value_output(node, slot);
                if pin_anchor_slot {
                    b.set_value_out_slot(out, slot);
                }
                Some(out)
            }
            AnchorKind::Memory(slot) => Some(b.memory_output(node, slot)),
            AnchorKind::Control(slot) => Some(b.control_output(node, slot)),
            AnchorKind::None => None,
        };
        if let Some(bits) = output_width
            && let Some(out) = anchor_out
        {
            b.set_value_width(out, bits);
        }
        for (slot, compile) in inputs {
            let o = compile(b);
            if let Some((_, bits)) = input_widths.iter().find(|(s, _)| *s == slot) {
                b.set_value_width(o, *bits);
            }
            b.input(node, slot, o);
        }
        // Sibling-output vertices: at a pinned slot, or enumerated over every
        // slot for `any_output()`.
        for spec in outputs {
            let out = match spec.slot {
                // The pin is what a single-vertex node (`switch`) needs: with
                // no second vertex to pick between, the root lookup takes the
                // only one and the slot would go unchecked.
                Some(slot) => {
                    let out = b.value_output(node, slot);
                    b.set_value_out_slot(out, slot);
                    out
                }
                None => b.any_slot_value_output(node),
            };
            match (spec.ty, spec.width) {
                (Some(ty), _) => b.set_value_ty(out, ty),
                // Without a type pin, relax to the wildcard so a control or
                // memory sibling output can still bind. A width pin below
                // still narrows it to a value of that width.
                (None, _) => b.set_output_any(out),
            }
            if let Some(bits) = spec.width {
                b.set_value_width(out, bits);
            }
            if let Some(c) = spec.capture {
                b.capture_output(out, c);
            }
        }
        if let Some(factory) = node_predicate {
            b.set_node_predicate_at(node, factory());
        }
        if let Some(c) = capture {
            match anchor_out {
                Some(out) => b.capture_output(out, c),
                None => b.capture_node(node, c),
            }
        }
        (node, anchor_out)
    }

    pub(crate) fn build(self) -> Pattern {
        let mut b = MatcherBuilder::new();
        self.lower(&mut b);
        b.finish()
    }

    /// For the [`MatchPat`] impls of anchored wrappers nesting as a value or
    /// memory operand. Those always carry an anchor by construction; only a
    /// node-rooted builder, which never nests, could yield `None`.
    pub(crate) fn compile_anchored(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.lower(b)
            .1
            .expect("anchored NodePat has an anchor output")
    }

    /// Compiles a node-rooted kind (`Return` / `Switch` / ...) for a slot that
    /// wires an edge, an alternation arm or an existential input. There is no
    /// anchor output, so synthesize an `Any` one: it binds against any edge and
    /// the match stays node-rooted.
    pub(crate) fn compile_alt_arm(self, b: &mut MatcherBuilder) -> PatValueRef {
        let (node, anchor) = self.lower(b);
        anchor.unwrap_or_else(|| b.any_value_output(node))
    }
}

/// For control-predecessor slots (`ctrl`), where the producer output is
/// `Control` rather than a value.
pub(crate) fn control_compiler<P: MatchPat + 'static>(p: P) -> SubCompiler {
    Box::new(move |b| {
        let o = p.compile(b);
        b.set_output_control(o);
        o
    })
}

/// Narrows a [`KindSpec::VariantWith`] beyond its discriminant: `CallOther`
/// `user_op_id`, `Load` / `Store` `space`.
pub(crate) type KindCheck = Box<dyn Fn(&NodeKind) -> bool>;

/// Pins a discriminant, plus the exact payload when `check` is `Some`.
pub(crate) fn variant_kind(
    discriminant: Discriminant<NodeKind>,
    check: Option<KindCheck>,
) -> KindSpec {
    match check {
        None => KindSpec::Variant(discriminant),
        Some(check) => KindSpec::VariantWith {
            discriminant,
            check,
        },
    }
}

#[cfg(test)]
mod tests {
    //! `input_any` on kinds with control/memory inputs (`Call`, `MemPhi`),
    //! driven through the raw [`NodePat::input_any`] path since no public
    //! wrapper exposes `any_input` on them. They pin the general model:
    //! candidate slots are EVERY input slot, with no value-kind filter, and
    //! the sub-pattern does the discriminating.

    use super::*;
    use crate::{Matcher, int_const};
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IRViewer};
    use strider_ir_test_utils::RegisterSet;

    /// Target `IntConst(0xABCD)` at fixed-prefix slot 2, arg0 `IntConst(42)`
    /// in the variadic tail at slot 4.
    fn call_with_arg() -> strider_ir::Function {
        let arg = strider_ir_test_utils::reg_vn(0, 8);
        let mut b = RegisterSet::new()
            .tracked(arg)
            .arg(arg)
            .build_fn_single_region()
            .unwrap();
        let c = b.build_int_const(42u64, ValueType::I64).unwrap();
        b.write_variable(&arg, c).unwrap();
        let tgt = b.build_int_const(0xABCDu64, ValueType::I64).unwrap();
        b.build_call_cc(tgt, None).unwrap();
        b.build_return(None, &[]).unwrap();
        b.build().unwrap()
    }

    /// Mirrors the raw shape the public `call()` builder lowers to.
    fn call_any_input<P: MatchPat + 'static>(p: P) -> Pattern {
        NodePat::node(KindSpec::Exact(NodeKind::Call))
            .with_mem_value(1)
            .input_any(p)
            .build()
    }

    /// A typed value sub matches whichever VALUE input carries that value; a
    /// wildcard reaches any input, including the control and memory ones a
    /// typed sub can never bind.
    #[test]
    fn call_any_input_general_model() {
        let function = call_with_arg();
        let matcher = Matcher::new(&function);

        // arg0 is IntConst(42).
        assert_eq!(
            matcher
                .find_all(&call_any_input(int_const(42u128)))
                .unwrap()
                .len(),
            1,
            "typed any_input binds the matching arg",
        );

        // The call target is an ordinary value input, so it is offered too.
        assert_eq!(
            matcher
                .find_all(&call_any_input(int_const(0xABCDu128)))
                .unwrap()
                .len(),
            1,
            "typed any_input reaches the call target under the general model",
        );

        // A typed sub can never reach control or memory.
        assert_eq!(
            matcher
                .find_all(&call_any_input(int_const(0x9999u128)))
                .unwrap()
                .len(),
            0,
            "typed any_input matching no value input binds nothing",
        );

        // The count is pinned to lock the candidate set. `Call` has no
        // PhiToken slot.
        let c = crate::Capture::new();
        let hits = matcher.find_all(&call_any_input(crate::var(c))).unwrap();
        assert_eq!(
            hits.len(),
            5,
            "wildcard any_input reaches every input (ctrl, mem, target, sp, arg0)",
        );
        let bound_kinds: Vec<_> = hits
            .iter()
            .map(|h| function.value_kind(h.value(c).unwrap()))
            .collect();
        assert!(
            bound_kinds
                .iter()
                .any(|k| matches!(k, strider_ir::node::ValueKind::Control)),
            "wildcard any_input binds the control input",
        );
        assert!(
            bound_kinds
                .iter()
                .any(|k| matches!(k, strider_ir::node::ValueKind::Memory)),
            "wildcard any_input binds the memory input",
        );
    }

    /// Two real memory predecessors, a store on each side of a joined
    /// if/else, so the variadic tail past the one-slot `[PhiToken]` prefix is
    /// `Memory`.
    fn mem_phi_with_two_stores() -> strider_ir::Function {
        let var_vn = strider_ir_test_utils::reg_vn(0x10, 8);
        let mut b = RegisterSet::new().tracked(var_vn).build_fn().unwrap();

        let entry = b.create_region_all().unwrap();
        let region_t = b.create_region_all().unwrap();
        let region_f = b.create_region_all().unwrap();
        let join = b.create_region_all().unwrap();

        b.set_entry_region_all(entry).unwrap();
        b.set_region(entry);
        b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
        let cond = b.build_boolean_const(true);
        b.build_if(cond, region_t, region_f).unwrap();

        for (region, val) in [(region_t, 1u64), (region_f, 2u64)] {
            b.set_region(region);
            let addr = b.build_int_const(0x40u64, ValueType::I64).unwrap();
            let data = b.build_int_const(val, ValueType::I64).unwrap();
            b.build_store(addr, data, rsleigh::VnSpace::RAM).unwrap();
            b.build_branch(join).unwrap();
        }

        b.set_region(join);
        let addr = b.build_int_const(0x48u64, ValueType::I64).unwrap();
        let loaded = b
            .build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)
            .unwrap();
        b.build_return(Some(loaded), &[]).unwrap();
        b.set_lift_addr(None);
        b.build().unwrap()
    }

    fn mem_phi_any_input<P: MatchPat + 'static>(p: P) -> Pattern {
        NodePat::node(KindSpec::variant_of(&NodeKind::MemPhi))
            .with_mem_value(0)
            .input_any(p)
            .build()
    }

    /// A `MemPhi`'s memory predecessors are inputs like any other, so a
    /// wildcard reaches both of them and the slot-0 `PhiToken`. A typed value
    /// sub binds none of them: it can never bind a `Memory` or `PhiToken`
    /// edge.
    #[test]
    fn mem_phi_any_input_binds_a_memory_predecessor() {
        let function = mem_phi_with_two_stores();
        let matcher = Matcher::new(&function);

        // Both stores' memory outputs and the `PhiToken` plumbing slot.
        let c = crate::Capture::new();
        let hits = matcher.find_all(&mem_phi_any_input(crate::var(c))).unwrap();
        let kinds: Vec<_> = hits
            .iter()
            .map(|hit| function.value_kind(hit.value(c).unwrap()))
            .collect();
        assert!(
            kinds
                .iter()
                .any(|k| matches!(k, strider_ir::node::ValueKind::Memory)),
            "wildcard any_input must bind a MemPhi memory predecessor, kinds: {kinds:?}",
        );
        assert!(
            kinds
                .iter()
                .any(|k| matches!(k, strider_ir::node::ValueKind::PhiToken)),
            "wildcard any_input can now also reach the PhiToken slot, kinds: {kinds:?}",
        );

        assert_eq!(
            matcher
                .find_all(&mem_phi_any_input(int_const(1u128)))
                .unwrap()
                .len(),
            0,
            "a typed value sub must not bind a Memory or PhiToken predecessor",
        );
    }

    /// A second predicate narrows the first, mirroring
    /// [`MatcherBuilder::set_node_predicate_at`], so a `.filter()` layered on a
    /// builder cannot delete that builder's core constraint.
    #[test]
    fn node_predicates_compose_rather_than_replace() {
        let function = call_with_arg();
        let matcher = Matcher::new(&function);
        let composed = NodePat::node(KindSpec::Exact(NodeKind::Return))
            .with_node_predicate(|| Box::new(|_m, _n| false))
            .with_node_predicate(|| Box::new(|_m, _n| true))
            .build();
        assert!(
            matcher.find_all(&composed).unwrap().is_empty(),
            "the first predicate must survive the second"
        );
    }

    /// A node-rooted kind has no anchor output, which must not lose the
    /// predicate.
    #[test]
    fn node_rooted_pattern_keeps_its_node_predicate() {
        let function = call_with_arg();
        let matcher = Matcher::new(&function);
        let rooted = || NodePat::node(KindSpec::Exact(NodeKind::Return));
        assert_eq!(
            matcher.find_all(&rooted().build()).unwrap().len(),
            1,
            "the unguarded node-rooted pattern matches the Return"
        );
        let guarded = rooted()
            .with_node_predicate(|| Box::new(|_m, _n| false))
            .build();
        assert!(
            matcher.find_all(&guarded).unwrap().is_empty(),
            "a rejecting node predicate must still run on a node-rooted pattern"
        );
    }

    /// `AnchorKind::Control` is the shape `entry()` and `region()` lower to.
    #[test]
    fn control_anchored_node_pat_seals_and_roots() {
        let pat = NodePat::node(KindSpec::Exact(NodeKind::Entry))
            .with_control_value(0)
            .build();
        assert!(
            pat.root().is_ok(),
            "a control-anchored NodePat must seal into a rooted Pattern"
        );
    }
}
