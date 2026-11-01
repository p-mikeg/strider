//! Every pattern is an [`Expr`] tree, lowered once into the builder of the
//! side it is used on. A child is an `Expr`, and the slot it sits in is passed
//! down at lowering, so a node declares per slot which output it nests
//! through.
//!
//! `.ordered()` pins the operand order of the node holding the pair a pattern
//! was written with. It is a lowering flag: an [`Op`] pins itself, or with
//! `ordered_at` its operand (`int_ne` pins its `int_eq`); an alternation is
//! refused; [`Inner::Shared`] has no pair to pin.

use strider_ir::node::{NodeKind, ValueType};
use strider_ir::{IntBinaryOp, IntUnaryOp};

use crate::capture::Capture;
use crate::matcher::match_pat::MatchPat;
use crate::matcher::{
    InputSlot, KindSpec, MatcherBuilder, NodePredicate, OutputKindSpec, PatNodeRef, PatValueRef,
    Pattern, PostMatchFn,
};
use crate::node_builders::MemPat;
use crate::template::{TemplateBuilder, TmplValueRef};
use crate::typed::consts::{Const, ConstWith};

/// The flavour of consumer slot an operand is lowered into, passed down
/// through decorations and alternation arms to the output each one nests
/// through.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Slot {
    Value,
    /// A raw input, of whatever edge kind the node's signature puts there:
    /// lowered as a value, a node with outputs of several kinds nesting
    /// through any of them.
    Raw,
    Memory,
    /// A control predecessor: lowered as a value, its output retyped to
    /// `Control`.
    Control,
    /// A pattern's root: lowered as a value, its output also taking a
    /// valueless node's memory token.
    Root,
}

/// One pattern, lowered once into either side's builder.
#[doc(hidden)]
pub struct Expr(Inner);

/// A pattern as an [`Expr`]. The side it is valid on is a marker:
/// [`MatchPat`] and [`TemplatePat`].
#[doc(hidden)]
pub trait IntoExpr: Sized {
    fn into_expr(self) -> Expr;
}

/// The escape hatch for a front end that builds the tree itself. A part met
/// on a side it is not valid on refuses the pattern.
impl IntoExpr for Expr {
    fn into_expr(self) -> Expr {
        self
    }
}

impl MatchPat for Expr {}
impl MemPat for Expr {}

enum Inner {
    /// Lowered already.
    Taken,
    Leaf(Leaf),
    Op(Box<Op>),
    Node(Box<Node>),
    /// A constant, given its per-side form at lowering.
    Const(Const),
    Deco(Box<(Expr, Deco)>),
    /// Matches if an arm does, the arms tried against the same IR node:
    /// every matching arm enumerated, or with `first` only the first.
    Alt(Vec<Expr>, bool),
    /// The operands, each lowered once and pinned to one IR value, then
    /// consumed at every slot of `body` naming them: `float_eq(x, x)` would
    /// otherwise accept two different operands.
    Shared(Vec<Expr>, fn(&[PatValueRef]) -> Expr),
    /// An operand [`Inner::Shared`] lowered.
    At(PatValueRef),
    /// A constant computed at rewrite time.
    Built(Box<ConstWith>),
}

/// A node with no operands and one value output.
pub(crate) struct Leaf {
    pub(crate) kind: KindSpec,
    /// `Any` relaxes the output to the wildcard kind, so value-less kinds
    /// (`Region`, `MemPhi`, ...) match too.
    pub(crate) out: Option<OutputKindSpec>,
    pub(crate) width: Option<u32>,
    pub(crate) pred: Option<NodePredicate>,
    /// On the build side, a `var()`'s capture is the value it bound.
    pub(crate) capture: Option<Capture>,
}

impl Leaf {
    pub(crate) fn new(kind: KindSpec) -> Self {
        Self {
            kind,
            out: None,
            width: None,
            pred: None,
            capture: None,
        }
    }

    /// Any node, whatever it produces.
    pub(crate) fn any() -> Self {
        Self {
            out: Some(OutputKindSpec::Any),
            ..Self::new(KindSpec::Any)
        }
    }

    pub(crate) fn lower(self, b: &mut MatcherBuilder) -> PatValueRef {
        let node = b.node(self.kind);
        let out = b.value_output(node, 0);
        match self.out {
            Some(OutputKindSpec::Value(ty)) => b.set_value_ty(out, ty),
            Some(OutputKindSpec::Any) => b.set_output_any(out),
            _ => {}
        }
        if let Some(bits) = self.width {
            b.set_value_width(out, bits);
        }
        if let Some(f) = self.pred {
            b.set_node_predicate_at(node, f);
        }
        if let Some(c) = self.capture {
            b.capture_output(out, c);
        }
        out
    }

    /// A `var()`: any node, bound to its capture.
    fn is_var(&self) -> bool {
        let any = self.kind == KindSpec::Any && self.out == Some(OutputKindSpec::Any);
        any && self.capture.is_some() && self.width.is_none() && self.pred.is_none()
    }

    fn lower_template(self, b: &mut TemplateBuilder) -> TmplValueRef {
        if self.is_var() {
            return b.capture(self.capture.expect("a var has its capture"));
        }
        match (self.capture, self.kind, self.pred) {
            (None, KindSpec::Exact(kind), None) => {
                let out = b.leaf(kind);
                if let Some(OutputKindSpec::Value(ty)) = self.out {
                    b.set_value_ty(out, ty);
                }
                out
            }
            _ => b.refuse(MATCH_SIDE),
        }
    }
}

/// A value op: one node over one or two operands at input slots 0 and 1,
/// nesting through value output 0.
pub(crate) struct Op {
    kind: KindSpec,
    ty: Option<ValueType>,
    /// The operand `.ordered()` pins instead of this node.
    ordered_at: Option<usize>,
    lhs: Expr,
    rhs: Option<Expr>,
}

impl Op {
    fn lower(self, b: &mut MatcherBuilder, ordered: bool) -> PatValueRef {
        let mut ins = [None, None];
        for (i, e) in std::iter::once(self.lhs).chain(self.rhs).enumerate() {
            ins[i] = Some(e.lower_operand(b, Slot::Value, ordered && self.ordered_at == Some(i)));
        }
        let node = b.node(self.kind);
        for (i, o) in ins.into_iter().enumerate() {
            if let Some(o) = o {
                b.wire(node, InputSlot::At(i), o);
            }
        }
        let out = b.value_output(node, 0);
        if let Some(ty) = self.ty {
            b.set_value_ty(out, ty);
        }
        if ordered && self.ordered_at.is_none() {
            b.set_force_ordered(out);
        }
        out
    }

    fn lower_template(self, b: &mut TemplateBuilder) -> TmplValueRef {
        let KindSpec::Exact(kind) = self.kind else {
            return b.refuse(MATCH_SIDE);
        };
        let mut ins = [None, None];
        for (i, e) in std::iter::once(self.lhs).chain(self.rhs).enumerate() {
            ins[i] = Some(e.lower_template_nested(b));
        }
        let node = b.node(kind);
        for (i, o) in ins.into_iter().enumerate() {
            if let Some(o) = o {
                b.input(node, i, o);
            }
        }
        let out = b.value_output(node, 0);
        if let Some(ty) = self.ty {
            b.set_value_ty(out, ty);
        }
        out
    }
}

/// An annotation on the output the inner expression lowers to.
pub(crate) enum Deco {
    /// Binds the matched value; on the build side, a `var()`'s second
    /// capture names the binding it reuses.
    Capture(Capture),
    /// A node predicate, run before the inputs are descended into.
    Filter(NodePredicate),
    /// A post-match guard handed the output's type; refused where there is
    /// none.
    Guard(PostMatchFn),
    /// A post-match guard.
    PostMatch(PostMatchFn),
    Width(u32),
    Ty(ValueType),
    Ordered,
    /// Nests through the inner expression's value output in a memory slot
    /// too: `inputs_of_width` reads a node with value inputs and a value
    /// output.
    AsValue,
    /// Types the built value from the width of a bound LHS capture.
    TyOf(Capture),
}

const MATCH_SIDE: &str = "a match-only pattern (a wildcard, a predicate, a variant-agnostic \
     op, an alternation or a node builder) sits in a template";
const BUILD_SIDE: &str = "a build-only pattern (a constant computed at rewrite time, or a \
     `capture_typed`) sits in a match pattern";

impl Drop for Expr {
    fn drop(&mut self) {
        // A tower of boxed operands dropped in place unwinds a frame per
        // level.
        if matches!(
            self.0,
            Inner::Op(_) | Inner::Node(_) | Inner::Deco(_) | Inner::Alt(..) | Inner::Shared(..)
        ) {
            crate::node_builders::defer_drop(self.take());
        }
    }
}

impl From<Inner> for Expr {
    fn from(i: Inner) -> Self {
        Self(i)
    }
}

impl From<Leaf> for Expr {
    fn from(l: Leaf) -> Self {
        Self(Inner::Leaf(l))
    }
}

impl From<Node> for Expr {
    fn from(n: Node) -> Self {
        Self(Inner::Node(Box::new(n)))
    }
}

impl From<Const> for Expr {
    fn from(c: Const) -> Self {
        Self(Inner::Const(c))
    }
}

impl From<ConstWith> for Expr {
    fn from(c: ConstWith) -> Self {
        Self(Inner::Built(Box::new(c)))
    }
}

/// The build side of the escape hatch. An `Expr` is not a
/// [`TemplatePat`](crate::TemplatePat), so a Rust rewrite cannot take one as
/// its RHS; a match-only part refuses the template through its root.
impl Expr {
    #[doc(hidden)]
    pub fn into_template(self) -> crate::Template {
        let mut b = TemplateBuilder::new();
        self.lower_template(&mut b);
        b.finish()
    }

    /// [`capture_typed`](crate::capture_typed) over a tree.
    #[doc(hidden)]
    pub fn capture_typed(self, cap: Capture) -> Self {
        self.deco(Deco::TyOf(cap))
    }

    pub(crate) fn deco(self, d: Deco) -> Self {
        Self(Inner::Deco(Box::new((self, d))))
    }

    /// Runs `f` once the whole sub-pattern matches, whatever its root
    /// output; `false` fails the match.
    #[doc(hidden)]
    pub fn post_match(self, f: PostMatchFn) -> Self {
        self.deco(Deco::PostMatch(f))
    }

    /// Seals a node on its own anchor, as its builder's `build` does, and
    /// anything else as [`MatchPat::into_pattern`].
    #[doc(hidden)]
    pub fn build(mut self) -> Pattern {
        match self.take() {
            Inner::Node(n) => n.build(),
            inner => Self(inner).into_pattern(),
        }
    }

    /// Binds the root output to `c`; a leaf takes its first capture itself.
    pub(crate) fn capture(mut self, c: Capture) -> Self {
        match &mut self.0 {
            Inner::Leaf(l) if l.capture.is_none() => {
                l.capture = Some(c);
                self
            }
            _ => self.deco(Deco::Capture(c)),
        }
    }

    pub(crate) fn alt(arms: Vec<Expr>, first: bool) -> Self {
        Self(Inner::Alt(arms, first))
    }

    pub(crate) fn shared(operands: Vec<Expr>, body: fn(&[PatValueRef]) -> Expr) -> Self {
        Self(Inner::Shared(operands, body))
    }

    pub(crate) fn at(v: PatValueRef) -> Self {
        Self(Inner::At(v))
    }

    fn take(&mut self) -> Inner {
        std::mem::replace(&mut self.0, Inner::Taken)
    }

    /// `ordered` pins the operand order on the node holding the pair the
    /// pattern was written with. The slot's own retyping lands on the output
    /// the expression nests through, an alternation's arms each taking it on
    /// theirs.
    pub(crate) fn lower(
        mut self,
        b: &mut MatcherBuilder,
        slot: Slot,
        ordered: bool,
    ) -> PatValueRef {
        // Each arm is its own function: an unoptimised frame holds every
        // arm's locals, and this one is on the stack once per recursion step.
        let out = match self.take() {
            Inner::Leaf(l) => {
                let out = l.lower(b);
                pin(b, out, ordered)
            }
            Inner::Op(op) => op.lower(b, ordered),
            Inner::Const(c) => {
                let out = c.lower(b);
                pin(b, out, ordered)
            }
            Inner::Node(n) => n
                .lower(b, Some(slot), ordered)
                .expect("a nested node has an output"),
            Inner::Deco(d) => lower_deco(b, *d, slot, ordered),
            Inner::Alt(arms, first) => lower_alt(b, arms, first, slot, ordered),
            Inner::Shared(operands, body) => lower_shared(b, operands, body, slot),
            Inner::At(v) => v,
            Inner::Built(_) => b.refuse(BUILD_SIDE),
            Inner::Taken => unreachable!("an Expr lowers once"),
        };
        match slot {
            Slot::Control => b.set_output_control(out),
            Slot::Root => b.set_token_fallback(out),
            _ => {}
        }
        out
    }

    /// An operand at a `slot` consumer, one nesting level down: refused where
    /// it can never match there.
    fn lower_operand(self, b: &mut MatcherBuilder, slot: Slot, ordered: bool) -> PatValueRef {
        if let Some(why) = self.misfit(slot) {
            b.reject(why);
        }
        if self.is_flat() {
            return self.lower(b, slot, ordered);
        }
        if !b.core.enter_nesting(true) {
            return b.leaf(KindSpec::Any);
        }
        let out = self.lower(b, slot, ordered);
        b.core.leave_nesting(true);
        out
    }

    /// A leaf lowers without recursing, so it costs no nesting step.
    fn is_flat(&self) -> bool {
        matches!(self.0, Inner::Leaf(_) | Inner::Const(_) | Inner::At(_))
    }

    /// Why this operand can never match a `slot` consumer, looking through
    /// its decorations to the node it nests through.
    fn misfit(&self, slot: Slot) -> Option<String> {
        let mut e = self;
        loop {
            match &e.0 {
                Inner::Deco(d) => e = &d.0,
                Inner::Node(n) => return n.misfit(slot),
                Inner::Op(_) if slot == Slot::Memory => {
                    return Some(misfit("producing only a value", "memory"));
                }
                _ => return None,
            }
        }
    }

    pub(crate) fn lower_template(mut self, b: &mut TemplateBuilder) -> TmplValueRef {
        match self.take() {
            Inner::Leaf(l) => l.lower_template(b),
            Inner::Op(op) => op.lower_template(b),
            Inner::Const(c) => c.lower_template(b),
            Inner::Deco(d) => match *d {
                // Only a `var()` is a template's `.capture()`: over anything
                // else the discarded operands would vanish in silence.
                (e, Deco::Capture(c)) => match &e.0 {
                    Inner::Leaf(l) if l.is_var() => b.capture(c),
                    _ => b.refuse(MATCH_SIDE),
                },
                (e, Deco::TyOf(c)) => {
                    let out = e.lower_template_nested(b);
                    b.set_value_ty_of_binding(out, c);
                    out
                }
                _ => b.refuse(MATCH_SIDE),
            },
            Inner::Built(c) => c.lower(b),
            Inner::Node(_) | Inner::Alt(..) | Inner::Shared(..) | Inner::At(_) => {
                b.refuse(MATCH_SIDE)
            }
            Inner::Taken => unreachable!("an Expr lowers once"),
        }
    }

    /// One recursion step down, charged to the stack budget.
    fn lower_template_nested(self, b: &mut TemplateBuilder) -> TmplValueRef {
        if self.is_flat() {
            return self.lower_template(b);
        }
        if !b.enter_nesting() {
            return b.placeholder();
        }
        let out = self.lower_template(b);
        b.leave_nesting();
        out
    }
}

fn pin(b: &mut MatcherBuilder, out: PatValueRef, ordered: bool) -> PatValueRef {
    if ordered {
        b.set_force_ordered(out);
    }
    out
}

fn lower_alt(
    b: &mut MatcherBuilder,
    arms: Vec<Expr>,
    first: bool,
    slot: Slot,
    ordered: bool,
) -> PatValueRef {
    if !b.core.enter_nesting(true) {
        return b.leaf(KindSpec::Any);
    }
    // Loops, not iterator chains: an unoptimised adapter is a frame per call,
    // held across each arm's recursion.
    let mut refs = Vec::with_capacity(arms.len());
    for a in arms {
        refs.push(a.lower(b, slot, false));
    }
    b.core.leave_nesting(true);
    let out = if first {
        b.first_of(&refs)
    } else {
        b.one_of(&refs)
    };
    pin(b, out, ordered)
}

/// Each operand feeds several slots; the pin is what makes them agree on it.
fn lower_shared(
    b: &mut MatcherBuilder,
    operands: Vec<Expr>,
    body: fn(&[PatValueRef]) -> Expr,
    slot: Slot,
) -> PatValueRef {
    let mut at = Vec::with_capacity(operands.len());
    for e in operands {
        let v = e.lower_operand(b, Slot::Value, false);
        b.pin_shared_identity(v);
        at.push(v);
    }
    body(&at).lower(b, slot, false)
}

/// The output the decorated expression lowers to, annotated.
fn lower_deco(
    b: &mut MatcherBuilder,
    (e, d): (Expr, Deco),
    slot: Slot,
    ordered: bool,
) -> PatValueRef {
    let (slot, ordered) = match d {
        Deco::Ordered if slot == Slot::Memory => (slot, false),
        Deco::Ordered => (slot, true),
        Deco::AsValue if slot == Slot::Memory => (Slot::Value, ordered),
        _ => (slot, ordered),
    };
    if !b.core.enter_nesting(false) {
        return b.leaf(KindSpec::Any);
    }
    // A chain of decorations recurses here, one frame a link.
    let out = match e.0 {
        Inner::Deco(_) => {
            let mut e = e;
            let Inner::Deco(inner) = e.take() else {
                unreachable!("matched above")
            };
            lower_deco(b, *inner, slot, ordered)
        }
        _ => e.lower(b, slot, ordered),
    };
    b.core.leave_nesting(false);
    annotate(b, out, d, slot)
}

/// Applies `d` to `out`. A control slot retypes `out` first, so a typed guard
/// sees it untyped.
fn annotate(b: &mut MatcherBuilder, out: PatValueRef, d: Deco, slot: Slot) -> PatValueRef {
    // Kept out of `lower_deco`'s frame, which is on the stack once per link.
    if slot == Slot::Control {
        b.set_output_control(out);
    }
    match d {
        Deco::Capture(c) => b.capture_output(out, c),
        Deco::Filter(f) => b.set_node_predicate(out, f),
        Deco::Guard(f) => b.set_post_match_typed(out, f),
        Deco::PostMatch(f) => b.set_post_match(out, f),
        Deco::Width(bits) => b.set_value_width(out, bits),
        Deco::Ty(ty) => b.set_value_ty(out, ty),
        Deco::Ordered if slot == Slot::Memory => b.set_force_ordered(out),
        Deco::Ordered | Deco::AsValue => {}
        Deco::TyOf(_) => b.reject(BUILD_SIDE.to_string()),
    }
    out
}

/// The lowered shapes, each the canonical form the lifter emits.
impl Expr {
    /// One node of `kind` over one or two operands, nesting through value
    /// output 0, its type pinned to `ty` where given.
    pub(crate) fn op<const N: usize>(
        kind: KindSpec,
        operands: [Expr; N],
        ty: Option<ValueType>,
    ) -> Self {
        let mut operands = operands.into_iter();
        let lhs = operands.next().expect("an op has an operand");
        Self(Inner::Op(Box::new(Op {
            kind,
            ty,
            ordered_at: None,
            lhs,
            rhs: operands.next(),
        })))
    }

    pub(crate) fn exact<const N: usize>(kind: NodeKind, operands: [Expr; N]) -> Self {
        Self::op(KindSpec::Exact(kind), operands, None)
    }

    /// A comparison or boolean op: output `I1`.
    pub(crate) fn exact_i1<const N: usize>(kind: NodeKind, operands: [Expr; N]) -> Self {
        Self::op(KindSpec::Exact(kind), operands, Some(ValueType::I1))
    }

    /// `int_add(l, int_neg(r))`.
    pub(crate) fn int_sub(l: Expr, r: Expr) -> Self {
        let neg = Self::exact(NodeKind::IntUnaryOp(IntUnaryOp::Neg), [r]);
        Self::exact(NodeKind::IntBinaryOp(IntBinaryOp::Add), [l, neg])
    }

    /// `int_xor(cmp, 1)`, the lifter's `!=` / `<=`; `.ordered()` pins `cmp`,
    /// the node holding the pair the caller wrote.
    pub(crate) fn negated(cmp: Expr) -> Self {
        let xor = NodeKind::IntBinaryOp(IntBinaryOp::Xor);
        let mut e = Self::exact(xor, [cmp, Const::Bool(true).into()]);
        if let Inner::Op(op) = &mut e.0 {
            op.ordered_at = Some(0);
        }
        e
    }

    /// `int_xor(x, 1):I1`.
    pub(crate) fn bool_not(x: Expr) -> Self {
        let xor = NodeKind::IntBinaryOp(IntBinaryOp::Xor);
        Self::exact_i1(xor, [x, Const::Bool(true).into()])
    }
}

/// The output a node predicate and capture attach to; it also doubles as the
/// chaining handle for memory roots and the seal handle for value roots.
#[derive(Clone, Copy)]
enum AnchorKind {
    Value(usize),
    Memory(usize),
    Control(usize),
    /// No output vertex at all: captures attach to the node directly, and a
    /// slot that wires an edge gets a synthesized `Any` output.
    None,
}

/// The one constraint a sibling-output vertex carries.
#[derive(Clone, Copy)]
pub enum OutputAspect {
    Capture(Capture),
    Width(u32),
    Ty(ValueType),
}

/// One pattern node, its operands and its output constraints.
pub(crate) struct Node {
    kind: KindSpec,
    /// Sparse: raw input slot, the slot flavour, the operand.
    inputs: Vec<(InputSlot, Slot, Expr)>,
    node_predicates: Vec<NodePredicate>,
    captures: Vec<Capture>,
    anchor: AnchorKind,
    /// The value output a value slot nests through, where it is not the
    /// anchor: a `Call` is anchored on its memory token.
    value_anchor: Option<usize>,
    output_width: Option<u32>,
    /// `(slot, bits)`.
    input_widths: Vec<(usize, u32)>,
    /// Requires the matched value to be produced at exactly the anchor slot
    /// ([`crate::matcher::SlotMode::Pinned`]).
    pin_anchor_slot: bool,
    /// At a pinned slot, or `None` for `any_output()`.
    outputs: Vec<(Option<usize>, OutputAspect)>,
    /// Control-output vertices beyond the anchor, `(slot, capture)`: the two
    /// arms of an `If`. Declared ahead of `outputs`.
    control_outputs: Vec<(usize, Option<Capture>)>,
}

impl Node {
    pub(crate) fn new(kind: KindSpec) -> Self {
        Self {
            kind,
            inputs: Vec::new(),
            node_predicates: Vec::new(),
            captures: Vec::new(),
            anchor: AnchorKind::None,
            value_anchor: None,
            output_width: None,
            input_widths: Vec::new(),
            pin_anchor_slot: false,
            outputs: Vec::new(),
            control_outputs: Vec::new(),
        }
    }

    /// Nests as a value operand.
    pub(crate) fn value(kind: KindSpec, slot: usize) -> Self {
        Self::new(kind).with_anchor(AnchorKind::Value(slot))
    }

    /// Narrows the kind, e.g. `call_other()` to one `user_op_id`.
    pub(crate) fn with_kind(mut self, kind: KindSpec) -> Self {
        self.kind = kind;
        self
    }

    fn with_anchor(mut self, anchor: AnchorKind) -> Self {
        self.anchor = anchor;
        self
    }

    /// Anchors on the memory output at `slot`.
    pub(crate) fn with_mem_value(self, slot: usize) -> Self {
        self.with_anchor(AnchorKind::Memory(slot))
    }

    /// Anchors on the control output at `slot`, so the node nests as a
    /// control operand.
    pub(crate) fn with_control_value(self, slot: usize) -> Self {
        self.with_anchor(AnchorKind::Control(slot))
    }

    /// Nests in a value slot through the value output at `slot`, as in
    /// `int_add(x, call_other().name("f"))`.
    pub(crate) fn with_value_anchor(mut self, slot: usize) -> Self {
        self.value_anchor = Some(slot);
        self
    }

    /// No-op unless the anchor is a value output. See
    /// [`crate::matcher::SlotMode::Pinned`].
    pub(crate) fn pin_anchor_slot(mut self) -> Self {
        self.pin_anchor_slot = true;
        self
    }

    /// Wires `e`, lowered for a `slot` consumer, into `at`.
    pub(crate) fn wire(mut self, at: InputSlot, slot: Slot, e: Expr) -> Self {
        self.inputs.push((at, slot, e));
        self
    }

    /// Wires `p` into value input `slot`.
    pub(crate) fn input<P: MatchPat + 'static>(self, slot: usize, p: P) -> Self {
        self.wire(InputSlot::At(slot), Slot::Value, p.into_expr())
    }

    /// Wires `p` into raw input `slot`, whatever edge it carries.
    pub(crate) fn raw_input<P: MatchPat + 'static>(self, slot: usize, p: P) -> Self {
        self.wire(InputSlot::At(slot), Slot::Raw, p.into_expr())
    }

    /// Existential: matches *some* input of the node rather than a fixed slot.
    pub(crate) fn input_any<P: MatchPat + 'static>(self, p: P) -> Self {
        self.wire(InputSlot::Any, Slot::Raw, p.into_expr())
    }

    /// The operand's root output is a control edge.
    pub(crate) fn input_control<P: MatchPat + 'static>(self, slot: usize, p: P) -> Self {
        self.wire(InputSlot::At(slot), Slot::Control, p.into_expr())
    }

    /// The operand's memory-token output feeds the slot.
    pub(crate) fn input_mem<M: MemPat + 'static>(self, slot: usize, p: M) -> Self {
        self.wire(InputSlot::At(slot), Slot::Memory, p.into_expr())
    }

    /// The predicate short-circuits before child recursion. Composes with one
    /// already set, like
    /// [`MatcherBuilder::set_node_predicate_at`](crate::matcher::MatcherBuilder::set_node_predicate_at).
    pub(crate) fn with_node_predicate(mut self, f: NodePredicate) -> Self {
        self.node_predicates.push(f);
        self
    }

    /// The anchor output's width. A node-rooted pattern has no anchor, so it
    /// is a no-op there.
    pub(crate) fn with_output_width(mut self, bits: u32) -> Self {
        self.output_width = Some(bits);
        self
    }

    /// Constrains the operand at `slot` to that output width.
    pub(crate) fn with_input_width(mut self, slot: usize, bits: u32) -> Self {
        self.input_widths.push((slot, bits));
        self
    }

    /// Binds the anchor output where there is one, otherwise the node.
    /// Repeatable: every capture binds the same thing.
    pub(crate) fn capture(mut self, c: Capture) -> Self {
        self.captures.push(c);
        self
    }

    /// A *sibling* output of the anchor. A leaf: it constrains the output
    /// value itself and does not recurse into what that output feeds.
    pub(crate) fn output(mut self, slot: Option<usize>, aspect: OutputAspect) -> Self {
        self.outputs.push((slot, aspect));
        self
    }

    /// A control-output vertex at `slot`, sibling to the anchor.
    pub(crate) fn with_control_output(mut self, slot: usize) -> Self {
        self.control_outputs.push((slot, None));
        self
    }

    /// Binds the control-output vertex at `slot`, replacing any capture there.
    pub(crate) fn capture_control_output(mut self, slot: usize, c: Capture) -> Self {
        if let Some(out) = self.control_outputs.iter_mut().find(|(s, _)| *s == slot) {
            out.1 = Some(c);
        }
        self
    }

    /// The anchor a `slot` consumer nests through.
    fn anchor_for(&self, slot: Slot) -> AnchorKind {
        match (slot, self.value_anchor) {
            (Slot::Value | Slot::Raw | Slot::Control | Slot::Root, Some(at)) => {
                AnchorKind::Value(at)
            }
            _ => self.anchor,
        }
    }

    /// Why the node can never match a `slot` consumer: it nests through no
    /// output of the slot's kind.
    fn misfit(&self, slot: Slot) -> Option<String> {
        let outputless = self.kind.is_variant_in(&[
            NodeKind::Return,
            NodeKind::IndirectBranch,
            NodeKind::Unreachable,
        ]);
        let (what, name) = match (slot, self.anchor_for(slot)) {
            (Slot::Value, AnchorKind::Memory(_)) => ("nesting through a memory token", "value"),
            (Slot::Control, AnchorKind::Memory(_)) => ("nesting through a memory token", "control"),
            (Slot::Value, AnchorKind::Control(_)) => ("producing only a control edge", "value"),
            (Slot::Value, AnchorKind::None) => ("with no output to nest through", "value"),
            (Slot::Control, AnchorKind::None) if outputless => ("with no output at all", "control"),
            (Slot::Memory, AnchorKind::Value(_) | AnchorKind::Control(_) | AnchorKind::None) => {
                ("with no memory token", "memory")
            }
            _ => return None,
        };
        Some(misfit(what, name))
    }

    /// Seals the node, anchored on its own anchor.
    pub(crate) fn build(self) -> Pattern {
        let mut b = MatcherBuilder::new();
        Box::new(self).lower(&mut b, None, false);
        b.finish()
    }

    /// Lowers into `b` for a `slot` consumer, through the anchor it reads,
    /// an anchorless node through a synthesized `Any` output. With no slot,
    /// on its own anchor: `None` for an anchorless kind like `Return` / `If`.
    fn lower(
        mut self: Box<Self>,
        b: &mut MatcherBuilder,
        slot: Option<Slot>,
        ordered: bool,
    ) -> Option<PatValueRef> {
        // The fields stay in the box: this frame is on the stack once per
        // nesting level, and an unoptimised one holds every local it names.
        let anchor = slot.map_or(self.anchor, |s| self.anchor_for(s));
        let inputs = self.lower_inputs(b);
        let node = b.node(std::mem::replace(&mut self.kind, KindSpec::Any));
        for (at, o) in inputs {
            b.wire(node, at, o);
        }
        let anchor_out = match anchor {
            AnchorKind::Value(at) => {
                let out = b.value_output(node, at);
                if self.pin_anchor_slot {
                    b.pin_value_out_slot(out);
                }
                // A call: its control, memory or value edge.
                if slot == Some(Slot::Raw) && self.value_anchor.is_some() {
                    b.set_output_any(out);
                }
                Some(out)
            }
            AnchorKind::Memory(at) => Some(b.memory_output(node, at)),
            AnchorKind::Control(at) => Some(b.control_output(node, at)),
            AnchorKind::None => None,
        };
        if let Some(bits) = self.output_width
            && let Some(out) = anchor_out
        {
            b.set_value_width(out, bits);
        }
        self.finish(b, node, anchor_out);
        slot?;
        let out = anchor_out.unwrap_or_else(|| b.any_value_output(node));
        Some(pin(b, out, ordered))
    }

    fn lower_inputs(&mut self, b: &mut MatcherBuilder) -> Vec<(InputSlot, PatValueRef)> {
        let mut wired = Vec::with_capacity(self.inputs.len());
        for (at, slot, e) in std::mem::take(&mut self.inputs) {
            let o = e.lower_operand(b, slot, false);
            let width = self
                .input_widths
                .iter()
                .find(|(s, _)| at == InputSlot::At(*s));
            if let Some(&(_, bits)) = width {
                b.set_value_width(o, bits);
            }
            wired.push((at, o));
        }
        wired
    }

    /// The output constraints, predicates and captures, once the operands
    /// are wired.
    fn finish(
        self: Box<Self>,
        b: &mut MatcherBuilder,
        node: PatNodeRef,
        anchor: Option<PatValueRef>,
    ) {
        let Node {
            node_predicates,
            captures,
            outputs,
            control_outputs,
            ..
        } = *self;
        for (slot, c) in control_outputs {
            let out = b.control_output(node, slot);
            if let Some(c) = c {
                b.capture_output(out, c);
            }
        }
        lower_outputs(b, node, outputs);
        for f in node_predicates {
            b.set_node_predicate_at(node, f);
        }
        for c in captures {
            match anchor {
                Some(out) => b.capture_output(out, c),
                None => b.capture_node(node, c),
            }
        }
    }
}

fn misfit(what: &str, name: &str) -> String {
    format!("an operand {what} sits in a {name} slot, which it can never match")
}

/// Sibling-output vertices on `node`: at a pinned slot, or enumerated over
/// every slot for `any_output()`.
fn lower_outputs(
    b: &mut MatcherBuilder,
    node: PatNodeRef,
    outputs: Vec<(Option<usize>, OutputAspect)>,
) {
    for (slot, aspect) in outputs {
        let out = match slot {
            // The pin is what a single-vertex node (`switch`) needs: with no
            // second vertex to pick between, the root lookup takes the only
            // one and the slot would go unchecked.
            Some(slot) => {
                let out = b.value_output(node, slot);
                b.pin_value_out_slot(out);
                out
            }
            None => b.any_slot_value_output(node),
        };
        // Without a type pin, relax to the wildcard so a control or memory
        // sibling output can still bind. A width pin still narrows it to a
        // value of that width.
        match aspect {
            OutputAspect::Ty(ty) => b.set_value_ty(out, ty),
            OutputAspect::Capture(c) => {
                b.set_output_any(out);
                b.capture_output(out, c);
            }
            OutputAspect::Width(bits) => {
                b.set_output_any(out);
                b.set_value_width(out, bits);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! `input_any` on a `Call`, whose inputs include control and memory, pins
    //! the general model: candidate slots are EVERY input slot, with no
    //! value-kind filter, and the sub-pattern does the discriminating.

    use super::*;
    use crate::{Matcher, int_const};
    use strider_ir::node::{NodeKind, ValueType};
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
        let c = b.build_int_const(42u64, ValueType::int(64)).unwrap();
        b.write_variable(&arg, c).unwrap();
        let tgt = b.build_int_const(0xABCDu64, ValueType::int(64)).unwrap();
        b.build_call_cc(tgt, None).unwrap();
        b.build_return(None, &[]).unwrap();
        b.build().unwrap()
    }

    fn call_node() -> Node {
        Node::new(KindSpec::variant_of(&NodeKind::Call { cc: None })).with_mem_value(1)
    }

    /// Mirrors the raw shape the public `call()` builder lowers to.
    fn call_any_input<P: MatchPat + 'static>(p: P) -> Pattern {
        call_node().input_any(p).build()
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

    /// A raw slot is a slot: `usize::MAX` names no input of the `Call`, so it
    /// matches nothing rather than standing for `any_input`.
    #[test]
    fn a_raw_input_at_usize_max_names_no_slot() {
        let function = call_with_arg();
        let matcher = Matcher::new(&function);
        let raw = call_node().input(usize::MAX, int_const(42u128)).build();
        assert!(matcher.find_all(&raw).unwrap().is_empty());
    }

    /// The same through the raw builder's `input`.
    #[test]
    fn a_raw_builder_input_at_usize_max_names_no_slot() {
        let function = call_with_arg();
        let mut b = MatcherBuilder::new();
        let arg = MatchPat::compile(int_const(42u128), &mut b);
        let node = b.node(KindSpec::variant_of(&NodeKind::Call { cc: None }));
        b.input(node, usize::MAX, arg);
        b.memory_output(node, 1);
        let raw = b.finish();
        assert!(Matcher::new(&function).find_all(&raw).unwrap().is_empty());
    }

    /// A second predicate narrows the first, mirroring
    /// [`MatcherBuilder::set_node_predicate_at`], so a `.filter()` layered on a
    /// builder cannot delete that builder's core constraint.
    #[test]
    fn node_predicates_compose_rather_than_replace() {
        let function = call_with_arg();
        let matcher = Matcher::new(&function);
        let composed = Node::new(KindSpec::Exact(NodeKind::Return))
            .with_node_predicate(Box::new(|_m, _n| false))
            .with_node_predicate(Box::new(|_m, _n| true))
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
        let rooted = || Node::new(KindSpec::Exact(NodeKind::Return));
        assert_eq!(
            matcher.find_all(&rooted().build()).unwrap().len(),
            1,
            "the unguarded node-rooted pattern matches the Return"
        );
        let guarded = rooted()
            .with_node_predicate(Box::new(|_m, _n| false))
            .build();
        assert!(
            matcher.find_all(&guarded).unwrap().is_empty(),
            "a rejecting node predicate must still run on a node-rooted pattern"
        );
    }

    /// A control slot and a root reach an alternation's arms through
    /// `inputs_of_width`, which constrains the node it wraps.
    #[test]
    fn a_slot_reaches_the_arms_under_inputs_of_width() {
        use crate::node_builders::pattern_shape;
        use crate::{anything, call, inputs_of_width, one_of, store, var};
        let arms = |shape: String| -> Vec<String> {
            shape
                .lines()
                .filter(|l| l.contains("out v"))
                .map(str::to_owned)
                .collect()
        };
        let ctrl = call()
            .ctrl(inputs_of_width(8, one_of![call(), store()]))
            .build();
        let ctrl = arms(pattern_shape(&ctrl));
        assert!(
            ctrl[..2].iter().all(|l| l.contains(" Control ")),
            "{ctrl:?}"
        );
        let root = inputs_of_width(8, one_of![var(crate::Capture::new()), anything()]);
        let root = arms(pattern_shape(&root.into_pattern()));
        assert!(root[..2].iter().all(|l| l.ends_with("token=1")), "{root:?}");
    }

    /// `AnchorKind::Control` is the shape `entry()` and `region()` lower to.
    #[test]
    fn control_anchored_node_seals_and_roots() {
        let pat = Node::new(KindSpec::Exact(NodeKind::Entry))
            .with_control_value(0)
            .build();
        assert!(
            pat.root().is_ok(),
            "a control-anchored Node must seal into a rooted Pattern"
        );
    }
}
