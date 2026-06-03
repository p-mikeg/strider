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

use crate::matcher::{MatcherBuilder, PatValueRef};
use crate::capture::Capture;
use crate::matcher::match_pat::MatchPat;
use crate::matcher::{KindSpec, NodePredicate, Pattern};

use super::{IndexedInputs, MemPat, SubCompiler};

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
    inputs: IndexedInputs,
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
}

/// The output produced by [`NodePat::lower`]: the anchor output (when the
/// anchor is not [`AnchorKind::None`]).
pub(crate) struct LowerResult {
    anchor_out: Option<PatValueRef>,
}

impl LowerResult {
    /// The anchor output, asserting it is a memory token. Used by the
    /// [`MemPat`] impls of the memory-rooted wrappers.
    #[allow(clippy::expect_used)]
    pub(crate) fn mem_value(self) -> PatValueRef {
        self.anchor_out.expect("memory-anchored NodePat has a memory output")
    }
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
        }
    }

    /// A value-rooted builder over `kind` whose value output lives at
    /// `slot` (sealed via `finish`; nests as a value operand).
    pub(crate) fn value(kind: KindSpec, slot: usize) -> Self {
        Self {
            kind,
            inputs: Vec::new(),
            node_predicate: None,
            capture: None,
            anchor: AnchorKind::Value(slot),
            output_width: None,
            input_widths: Vec::new(),
        }
    }

    /// Replace the node's kind spec (e.g. once `CallOther::user_op_id`
    /// narrows the variant to a `VariantWith` payload check).
    pub(crate) fn with_kind(mut self, kind: KindSpec) -> Self {
        self.kind = kind;
        self
    }

    /// Set the node's anchor output (overriding the constructor default).
    pub(crate) fn with_anchor(mut self, anchor: AnchorKind) -> Self {
        self.anchor = anchor;
        self
    }

    /// Declare a memory-token anchor output at `slot` (the chaining
    /// handle exposed by the memory-rooted wrappers via [`MemPat`]).
    pub(crate) fn with_mem_value(self, slot: usize) -> Self {
        self.with_anchor(AnchorKind::Memory(slot))
    }

    /// Declare a value anchor output at `slot` on a node-rooted builder
    /// (e.g. `Phi`: rooted on the node, captured/limited via its value
    /// output). Distinct from [`NodePat::value`], which also makes the
    /// builder a *value* root sealed on the output.
    pub(crate) fn with_anchor_value(self, slot: usize) -> Self {
        self.with_anchor(AnchorKind::Value(slot))
    }

    /// Wire a value sub-pattern into raw input `slot`. The one boxing
    /// site for value operands.
    pub(crate) fn input<P: MatchPat + 'static>(mut self, slot: usize, p: P) -> Self {
        self.inputs.push((slot, Box::new(move |b| p.compile(b))));
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
        self.inputs.push((slot, Box::new(move |b| p.compile_mem(b))));
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
    pub(crate) fn lower(self, b: &mut MatcherBuilder) -> LowerResult {
        let NodePat {
            kind,
            inputs,
            node_predicate,
            capture,
            anchor,
            output_width,
            input_widths,
        } = self;
        let node = b.node(kind);
        let anchor_out = match anchor {
            AnchorKind::Value(slot) => Some(b.value_output(node, slot)),
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
                Some(out) => b.capture_node(out, c),
                None => b.capture_node_for(node, c),
            }
        }
        LowerResult { anchor_out }
    }

    /// Seal the builder into a finished [`Pattern`]: on the value output
    /// for value roots, on the node vertex for node roots.
    pub(crate) fn build(self) -> Pattern {
        let mut b = MatcherBuilder::new();
        let _ = self.lower(&mut b);
        b.finish()
    }

    /// Lower and return the value anchor output (for the [`MatchPat`]
    /// impls of value-rooted wrappers that nest as a value operand).
    #[allow(clippy::expect_used)]
    pub(crate) fn compile_value(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.lower(b).anchor_out.expect("value-rooted NodePat has a value output")
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
pub(crate) fn variant_kind(discriminant: Discriminant<NodeKind>, check: Option<KindCheck>) -> KindSpec {
    match check {
        None => KindSpec::Variant(discriminant),
        Some(check) => KindSpec::VariantWith { discriminant, check },
    }
}
