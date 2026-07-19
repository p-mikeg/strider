//! [`NodePat`] — the shared lowering core behind the slot-convention
//! control / memory / phi builders.
//!
//! Every variadic node-family builder (`Call` / `CallOther` / `Return` /
//! `Load` / `Store` / `Phi` / `MemPhi`) lowers to the same
//! shape: pick a kind, declare an anchor output (a value output, a
//! memory-token output, or none), wire sparse positional sub-patterns
//! into input slots, optionally pin a node predicate + capture, then
//! seal the built graph (`finish`).
//! [`NodePat`] holds that machinery once; the public
//! builders are thin slot-convention wrappers that translate caller
//! verbs into [`NodePat::input`] / [`NodePat::input_control`] /
//! [`NodePat::input_mem`] calls at the right raw slot.
//!
//! The lone genuine per-builder difference is the slot convention (which
//! raw input slot each verb addresses) and the anchor/root choice, both
//! expressed declaratively here via [`AnchorKind`].

use std::mem::Discriminant;

use strider_ir::node::NodeKind;

use crate::capture::Capture;
use crate::matcher::match_pat::MatchPat;
use crate::matcher::{KindSpec, MatcherBuilder, NodePredicate, PatValueRef, Pattern};

use super::{MemPat, SubCompiler};

/// How the lowered node's anchor output is modelled. The anchor is the
/// output a node-limit and capture attach to; it also doubles as the
/// chaining handle for memory roots and the seal handle for value roots.
#[derive(Clone, Copy)]
pub(crate) enum AnchorKind {
    /// A value output at the given slot (`Load` / `Phi`).
    Value(usize),
    /// A memory-token output at the given slot (`Call` / `CallOther` /
    /// `Store` / `MemPhi`).
    Memory(usize),
    /// No output vertex at all (`Return`): captures attach to the node
    /// directly and no node-limit can be anchored.
    None,
}

/// A node-predicate factory: given the matched node's anchor, install a
/// predicate. Boxed once and run inside [`NodePat::lower`] so the predicate
/// can close over filter state captured at builder-construction time.
type NodePredicateFactory = Box<dyn FnOnce() -> NodePredicate>;

/// Shared lowering core for the slot-convention node-family builders.
pub(crate) struct NodePat {
    kind: KindSpec,
    /// Sparse indexed sub-pattern constraints (raw input slot → compiler).
    inputs: Vec<(usize, SubCompiler)>,
    node_predicate: Option<NodePredicateFactory>,
    capture: Option<Capture>,
    anchor: AnchorKind,
    /// Width to pin on the anchor value output (declarative; checked by
    /// the matcher's `output_ok`). `None` leaves the width unconstrained.
    output_width: Option<u32>,
    /// Declarative width constraints on the producer output of specific
    /// input slots (`(slot, bits)`); applied during `lower` to the
    /// compiled sub-pattern's output before it is wired into the slot.
    input_widths: Vec<(usize, u32)>,
    /// When `true` and the anchor is a value output, enforce that the matched
    /// value is produced at exactly the anchor slot (see
    /// [`PatValue::match_slot`]). Set by `.res()` on Call/CallOther to pin the
    /// declared result output and exclude clobbers.
    pin_anchor_slot: bool,
}

impl NodePat {
    /// A node-rooted builder over `kind` with no anchor output.
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
        }
    }

    /// A value-rooted builder over `kind` whose value output lives at
    /// `slot` (sealed via `finish`; nests as a value operand).
    pub(crate) fn value(kind: KindSpec, slot: usize) -> Self {
        let mut s = Self::node(kind);
        s.anchor = AnchorKind::Value(slot);
        s
    }

    /// Replace the node's kind spec (e.g. once `CallOther::user_op_id`
    /// narrows the variant to a `VariantWith` payload check).
    pub(crate) fn with_kind(mut self, kind: KindSpec) -> Self {
        self.kind = kind;
        self
    }

    /// Declare a memory-token anchor output at `slot` (the chaining
    /// handle exposed by the memory-rooted wrappers via [`MemPat`]).
    pub(crate) fn with_mem_value(mut self, slot: usize) -> Self {
        self.anchor = AnchorKind::Memory(slot);
        self
    }

    /// Re-anchor onto a value output at `slot`. Used to nest a normally
    /// memory-rooted node (`Call` / `CallOther`, whose value outputs start at
    /// slot 2 after ctrl/mem) as a **value** operand of another node, e.g.
    /// `add(x, call_other().name("f"))`.
    pub(crate) fn with_value_anchor(mut self, slot: usize) -> Self {
        self.anchor = AnchorKind::Value(slot);
        self
    }

    /// Enforce the anchor value output's slot at match time (see
    /// [`crate::matcher::PatValue::match_slot`]). Only takes effect when the
    /// anchor is [`AnchorKind::Value`]. Set by `.res()` on Call/CallOther.
    pub(crate) fn pin_anchor_slot(mut self) -> Self {
        self.pin_anchor_slot = true;
        self
    }

    /// Wire a value sub-pattern into raw input `slot`. The one boxing
    /// site for value operands.
    pub(crate) fn input<P: MatchPat + 'static>(mut self, slot: usize, p: P) -> Self {
        self.inputs.push((slot, Box::new(move |b| p.compile(b))));
        self
    }

    /// Wire an **existential** value sub-pattern: it matches *some* value
    /// input of the node rather than a fixed slot (the `any_input` verb).
    /// Recorded at the [`ANY_INPUT_SLOT`] sentinel slot; the matcher routes
    /// it through its existential search.
    pub(crate) fn input_any<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.inputs.push((
            crate::matcher::ANY_INPUT_SLOT,
            Box::new(move |b| p.compile(b)),
        ));
        self
    }

    /// Wire a control-predecessor sub-pattern into raw input `slot`
    /// (relaxing its root output to a control edge).
    pub(crate) fn input_control<P: MatchPat + 'static>(mut self, slot: usize, p: P) -> Self {
        self.inputs.push((slot, control_compiler(p)));
        self
    }

    /// Wire a memory-producing sub-pattern into raw input `slot` (its
    /// memory-token output feeds the slot).
    pub(crate) fn input_mem<M: MemPat + 'static>(mut self, slot: usize, p: M) -> Self {
        self.inputs
            .push((slot, Box::new(move |b| p.compile_mem(b))));
        self
    }

    /// Install a node predicate on the anchor (short-circuits before
    /// child recursion). The factory is invoked once during `lower`.
    pub(crate) fn with_node_predicate<F>(mut self, f: F) -> Self
    where
        F: FnOnce() -> NodePredicate + 'static,
    {
        self.node_predicate = Some(Box::new(f));
        self
    }

    /// Pin the anchor value output's width to `bits` (declarative;
    /// reuses the matcher's output-vertex width check). The anchor must
    /// be a value output ([`AnchorKind::Value`]) for this to take effect.
    pub(crate) fn with_output_width(mut self, bits: u32) -> Self {
        self.output_width = Some(bits);
        self
    }

    /// Pin the width of the producer output wired into raw input `slot`
    /// to `bits` (declarative). Applied during `lower` to the compiled
    /// sub-pattern's output, so the matcher's `output_ok` checks it when
    /// the input is consumed.
    pub(crate) fn with_input_width(mut self, slot: usize, bits: u32) -> Self {
        self.input_widths.push((slot, bits));
        self
    }

    /// Bind the matched node (or its anchor output) to `c`.
    pub(crate) fn capture(mut self, c: Capture) -> Self {
        self.capture = Some(c);
        self
    }

    /// Lower the node onto `b`: create the node, declare its anchor
    /// output, wire every indexed input, then apply the node-limit and
    /// capture. Shared by [`build`](Self::build) and the wrappers'
    /// [`MemPat`] / [`MatchPat`] impls.
    pub(crate) fn lower(self, b: &mut MatcherBuilder) -> Option<PatValueRef> {
        let NodePat {
            kind,
            inputs,
            node_predicate,
            capture,
            anchor,
            output_width,
            input_widths,
            pin_anchor_slot,
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
        if let Some(factory) = node_predicate
            && let Some(out) = anchor_out
        {
            b.set_node_predicate(out, factory());
        }
        if let Some(c) = capture {
            match anchor_out {
                Some(out) => b.capture_output(out, c),
                None => b.capture_node(node, c),
            }
        }
        anchor_out
    }

    /// Seal the builder into a finished [`Pattern`]: on the value output
    /// for value roots, on the node vertex for node roots.
    pub(crate) fn build(self) -> Pattern {
        let mut b = MatcherBuilder::new();
        let _ = self.lower(&mut b);
        b.finish()
    }

    /// Lower and return the anchor output (for the [`MatchPat`] /
    /// [`MemPat`] impls of anchored wrappers that nest as a value or
    /// memory operand). The anchor is `Some(_)` by construction for these
    /// wrappers ([`AnchorKind::Value`] / [`AnchorKind::Memory`]); only a
    /// node-rooted ([`AnchorKind::None`]) builder, which never nests,
    /// would yield `None`.
    #[allow(clippy::expect_used)]
    pub(crate) fn compile_anchored(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.lower(b)
            .expect("anchored NodePat has an anchor output")
    }
}

/// A [`SubCompiler`] that compiles a value-pattern then relaxes its root
/// output to match a control edge. Used for control-predecessor slots
/// (`ctrl` / `preceded_by`) where the producer's output is `Control`,
/// not a value.
fn control_compiler<P: MatchPat + 'static>(p: P) -> SubCompiler {
    Box::new(move |b| {
        let o = p.compile(b);
        b.set_output_control(o);
        o
    })
}

/// An exact-payload predicate narrowing a [`KindSpec::VariantWith`]
/// beyond its discriminant (e.g. `CallOther` `user_op_id`, `Load` /
/// `Store` `space`).
pub(crate) type KindCheck = Box<dyn Fn(&NodeKind) -> bool>;

/// Build a `KindSpec` that pins a node kind's discriminant and, when
/// `check` is `Some`, additionally narrows the exact payload via
/// [`KindSpec::VariantWith`]. Shared by `CallOther` (`user_op_id`) and
/// `Load` / `Store` (`space`).
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
    //! Engine-level `input_any` (`ANY_INPUT_SLOT`) tests on kinds with
    //! control/memory inputs (`Call`, `MemPhi`) — exercised through the raw
    //! [`NodePat::input_any`] path because no public wrapper exposes `any_input`
    //! on them yet. They pin the general `any_input` model: candidate slots are
    //! every input except the `PhiToken`, and the sub-pattern discriminates.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::{Matcher, int_const};
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IRViewer};
    use strider_ir_test_utils::RegisterSet;

    /// A `Call` whose **target** is `IntConst(0xABCD)` (fixed-prefix slot 2)
    /// and whose **arg0** is `IntConst(42)` (variadic tail, slot 4).
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

    /// A node-rooted `Call` pattern with a single existential `any_input(p)`,
    /// mirroring the raw shape the public `call()` builder lowers to.
    fn call_any_input<P: MatchPat + 'static>(p: P) -> Pattern {
        NodePat::node(KindSpec::Exact(NodeKind::Call))
            .with_mem_value(1)
            .input_any(p)
            .build()
    }

    /// `any_input` under the GENERAL model: the candidate slots are every input
    /// EXCEPT the `PhiToken` plumbing slot (a `Call` has none). A typed value
    /// sub matches whichever VALUE input carries that value; a wildcard reaches
    /// ANY input, including the control/memory ones a typed sub can never bind.
    #[test]
    fn call_any_input_general_model() {
        let function = call_with_arg();
        let matcher = Matcher::new(&function);

        // A typed sub binds the arg carrying its value (arg0 = IntConst(42)).
        assert_eq!(
            matcher
                .find_all(&call_any_input(int_const(42u128)))
                .unwrap()
                .len(),
            1,
            "typed any_input binds the matching arg",
        );

        // The call TARGET is an ordinary value input; under the general model it
        // is offered (the tail-only engine hid it), so a typed sub of the
        // target's value now binds it.
        assert_eq!(
            matcher
                .find_all(&call_any_input(int_const(0xABCDu128)))
                .unwrap()
                .len(),
            1,
            "typed any_input reaches the call target under the general model",
        );

        // Sub-pattern discrimination: a typed VALUE sub matching no value input
        // binds nothing — control/memory are never reachable by a typed sub.
        assert_eq!(
            matcher
                .find_all(&call_any_input(int_const(0x9999u128)))
                .unwrap()
                .len(),
            0,
            "typed any_input matching no value input binds nothing",
        );

        // A WILDCARD reaches every non-`PhiToken` input — the control and memory
        // edges included, which is the general model's defining behavior. The
        // exact count is pinned to lock the candidate set (ctrl, mem, target,
        // sp, arg0).
        let c = crate::Capture::new();
        let hits = matcher.find_all(&call_any_input(crate::var(c))).unwrap();
        assert_eq!(
            hits.len(),
            5,
            "wildcard any_input reaches every non-PhiToken input (ctrl, mem, target, sp, arg0)",
        );
        // Among the wildcard's bindings, a control and a memory input DO appear
        // — proving those slots are reachable only by a wildcard, not the typed
        // subs above.
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

    /// A `MemPhi` with two REAL memory predecessors (a store on each side of
    /// an if/else, joined) — its variadic tail (past the fixed `[PhiToken]`
    /// prefix, len 1) is `Memory`, not `PhiToken`. Exercised through the raw
    /// [`NodePat::input_any`] path because the public `mem_phi()` builder
    /// doesn't expose `any_input` (there is no value sub-pattern a `Memory`
    /// tail slot could ever legitimately bind).
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

    /// A node-rooted `MemPhi` pattern with a single existential
    /// `any_input(p)`, mirroring the raw shape `call_any_input` uses above.
    fn mem_phi_any_input<P: MatchPat + 'static>(p: P) -> Pattern {
        NodePat::node(KindSpec::variant_of(&NodeKind::MemPhi))
            .with_mem_value(0)
            .input_any(p)
            .build()
    }

    /// Under the GENERAL model a `MemPhi`'s memory predecessors ARE offered to
    /// `any_input` — they are inputs, not the `PhiToken`. A wildcard binds them
    /// (one per memory predecessor), while a typed value sub binds none of them
    /// (sub-pattern discrimination: a typed sub can never bind a `Memory` edge).
    #[test]
    fn mem_phi_any_input_binds_a_memory_predecessor() {
        let function = mem_phi_with_two_stores();
        let matcher = Matcher::new(&function);

        // A wildcard reaches the memory predecessors (the if/else stores'
        // memory outputs) — under the general model they ARE offered. Every
        // binding is a Memory value (never the excluded slot-0 `PhiToken`).
        let c = crate::Capture::new();
        let hits = matcher.find_all(&mem_phi_any_input(crate::var(c))).unwrap();
        assert!(
            !hits.is_empty(),
            "wildcard any_input must bind a MemPhi memory predecessor",
        );
        for hit in &hits {
            assert!(
                matches!(
                    function.value_kind(hit.value(c).unwrap()),
                    strider_ir::node::ValueKind::Memory
                ),
                "each wildcard binding is a Memory predecessor, never the PhiToken",
            );
        }

        // Sub-pattern discrimination: a typed VALUE sub can never bind a Memory
        // edge, so it matches nothing.
        assert_eq!(
            matcher
                .find_all(&mem_phi_any_input(int_const(1u128)))
                .unwrap()
                .len(),
            0,
            "a typed value sub must not bind a Memory predecessor",
        );
    }
}
