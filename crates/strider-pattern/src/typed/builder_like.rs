//! The shared verb surface between the match- and build-side builders.
//!
//! [`MatcherBuilder`] and
//! [`TemplateBuilder`] expose an
//! identical subset of construction verbs for value ops (`leaf` / `unary`
//! / `binary` / `node` / `input` / `value_output` / `set_value_ty`),
//! differing only in their output/node handle types. [`BuilderLike`]
//! abstracts that subset so a fixed-op typed struct can declare its node
//! shape exactly once, in a single [`CompileInto::compile_into`] body
//! generic over the builder, instead of byte-for-byte duplicated
//! `MatchPat` / `TemplatePat` impls.
//!
//! The match/template split (and with it the compile-time
//! wildcard-in-RHS guard) is preserved entirely: [`CompileInto`] is
//! blanket-implemented for `MatchPat` over the match builder and for
//! `TemplatePat` over the template builder, and the operand bounds of
//! each fixed-op `compile_into` are stated against [`CompileInto`], so an
//! operand only lowers into a builder its own side admits. The split
//! factory free functions (`add` bounded `MatchPat`, `template::add`
//! bounded `TemplatePat`) are untouched.

use strider_ir::IntBinaryOp;
use strider_ir::node::ValueType;

use crate::matcher::KindSpec;
use crate::matcher::match_pat::MatchPat;
use crate::matcher::{MatcherBuilder, PatNodeRef, PatValueRef};
use crate::template::template_pat::TemplatePat;
use crate::template::{TemplateBuilder, TmplNodeRef, TmplValueRef};

/// The value-op construction verbs shared by both imperative builders.
///
/// Implemented for [`MatcherBuilder`] (match side) and
/// [`TemplateBuilder`] (build side). A [`CompileInto`] body wires its
/// node shape through these verbs and so lowers correctly into either.
pub trait BuilderLike {
    /// Handle to an output vertex this builder produced.
    type OutRef: Copy;
    /// Handle to a bare node vertex this builder produced.
    type NodeRef: Copy;

    /// A unary node of `kind` consuming `inner`, with one value output.
    fn unary(&mut self, kind: KindSpec, inner: Self::OutRef) -> Self::OutRef;
    /// A binary [`IntBinaryOp`] node consuming `l` / `r`, with one value
    /// output.
    fn binary(&mut self, op: IntBinaryOp, l: Self::OutRef, r: Self::OutRef) -> Self::OutRef;
    /// A bare node of `kind` with no inputs/outputs yet.
    fn node(&mut self, kind: KindSpec) -> Self::NodeRef;
    /// Wire `prod` into `node`'s input `slot`.
    fn input(&mut self, node: Self::NodeRef, slot: usize, prod: Self::OutRef);
    /// Add a value output at `slot` to `node`.
    fn value_output(&mut self, node: Self::NodeRef, slot: usize) -> Self::OutRef;
    /// Pin `out`'s value output to an exact type.
    fn set_value_ty(&mut self, out: Self::OutRef, ty: ValueType);
    /// A leaf node of `kind` (no inputs) with a single value output.
    fn leaf(&mut self, kind: KindSpec) -> Self::OutRef;
}

impl BuilderLike for MatcherBuilder {
    type OutRef = PatValueRef;
    type NodeRef = PatNodeRef;

    fn unary(&mut self, kind: KindSpec, inner: Self::OutRef) -> Self::OutRef {
        MatcherBuilder::unary(self, kind, inner)
    }
    fn binary(&mut self, op: IntBinaryOp, l: Self::OutRef, r: Self::OutRef) -> Self::OutRef {
        MatcherBuilder::binary(self, op, l, r)
    }
    fn node(&mut self, kind: KindSpec) -> Self::NodeRef {
        MatcherBuilder::node(self, kind)
    }
    fn input(&mut self, node: Self::NodeRef, slot: usize, prod: Self::OutRef) {
        MatcherBuilder::input(self, node, slot, prod);
    }
    fn value_output(&mut self, node: Self::NodeRef, slot: usize) -> Self::OutRef {
        MatcherBuilder::value_output(self, node, slot)
    }
    fn set_value_ty(&mut self, out: Self::OutRef, ty: ValueType) {
        MatcherBuilder::set_value_ty(self, out, ty);
    }
    fn leaf(&mut self, kind: KindSpec) -> Self::OutRef {
        MatcherBuilder::leaf(self, kind)
    }
}

impl BuilderLike for TemplateBuilder {
    type OutRef = TmplValueRef;
    type NodeRef = TmplNodeRef;

    fn unary(&mut self, kind: KindSpec, inner: Self::OutRef) -> Self::OutRef {
        TemplateBuilder::unary(self, kind, inner)
    }
    fn binary(&mut self, op: IntBinaryOp, l: Self::OutRef, r: Self::OutRef) -> Self::OutRef {
        TemplateBuilder::binary(self, op, l, r)
    }
    fn node(&mut self, kind: KindSpec) -> Self::NodeRef {
        TemplateBuilder::node(self, kind)
    }
    fn input(&mut self, node: Self::NodeRef, slot: usize, prod: Self::OutRef) {
        TemplateBuilder::input(self, node, slot, prod);
    }
    fn value_output(&mut self, node: Self::NodeRef, slot: usize) -> Self::OutRef {
        TemplateBuilder::value_output(self, node, slot)
    }
    fn set_value_ty(&mut self, out: Self::OutRef, ty: ValueType) {
        TemplateBuilder::set_value_ty(self, out, ty);
    }
    fn leaf(&mut self, kind: KindSpec) -> Self::OutRef {
        TemplateBuilder::leaf(self, kind)
    }
}

/// Lower a typed operand into a builder `B`, returning its value-output
/// handle.
///
/// Blanket-implemented for every [`MatchPat`] over [`MatcherBuilder`] and
/// every [`TemplatePat`] over [`TemplateBuilder`], so the two sides'
/// operand admissibility (and the wildcard-in-RHS compile guard) carry
/// straight through to the generic `compile_into` bodies.
pub trait CompileInto<B: BuilderLike> {
    /// Lower `self` into `b`, returning the value-output handle of its
    /// root node.
    fn compile_into(self, b: &mut B) -> B::OutRef;
}

impl<P: MatchPat> CompileInto<MatcherBuilder> for P {
    fn compile_into(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.compile(b)
    }
}

impl<P: TemplatePat> CompileInto<TemplateBuilder> for P {
    fn compile_into(self, b: &mut TemplateBuilder) -> TmplValueRef {
        self.compile(b)
    }
}

// ── Shared fixed-op lowerings ─────────────────────────────────────────
//
// One generic body per fixed-op node shape, wired through the
// [`BuilderLike`] verbs and operands bounded by [`CompileInto<B>`]. The
// `MatchPat` / `TemplatePat` impls of each fixed-op struct are thin
// forwarders to these (the operand bound `L: MatchPat` supplies `L:
// CompileInto<MatcherBuilder>` via the blanket impls above, and likewise
// for the template side), so each node shape is declared exactly once.
// These are kept as free functions rather than a single generic
// `CompileInto` impl per struct because the structs already carry direct
// `MatchPat` / `TemplatePat` impls, and a blanket `CompileInto` over
// those traits would collide with such per-struct impls.

/// `unary(kind, inner)` — the cast / fixed-kind-unary shape.
pub(crate) fn compile_unary_kind<B, I>(b: &mut B, kind: KindSpec, inner: I) -> B::OutRef
where
    B: BuilderLike,
    I: CompileInto<B>,
{
    let i = inner.compile_into(b);
    b.unary(kind, i)
}

/// `binary(op, l, r)` — the plain integer-binary shape.
pub(crate) fn compile_int_binary<B, L, R>(b: &mut B, op: IntBinaryOp, l: L, r: R) -> B::OutRef
where
    B: BuilderLike,
    L: CompileInto<B>,
    R: CompileInto<B>,
{
    let l = l.compile_into(b);
    let r = r.compile_into(b);
    b.binary(op, l, r)
}

/// A two-input node of `kind` with one value output, optionally pinned to
/// `out_ty`. Covers the int/float comparison (`I1`-pinned) and float-
/// binary (un-pinned) shapes.
pub(crate) fn compile_two_input<B, L, R>(
    b: &mut B,
    kind: KindSpec,
    l: L,
    r: R,
    out_ty: Option<ValueType>,
) -> B::OutRef
where
    B: BuilderLike,
    L: CompileInto<B>,
    R: CompileInto<B>,
{
    let n = b.node(kind);
    let l = l.compile_into(b);
    let r = r.compile_into(b);
    b.input(n, 0, l);
    b.input(n, 1, r);
    let out = b.value_output(n, 0);
    if let Some(ty) = out_ty {
        b.set_value_ty(out, ty);
    }
    out
}

/// `binary(op, l, r)` pinned to `I1` — the boolean-binary shape.
pub(crate) fn compile_bool_binary<B, L, R>(b: &mut B, op: IntBinaryOp, l: L, r: R) -> B::OutRef
where
    B: BuilderLike,
    L: CompileInto<B>,
    R: CompileInto<B>,
{
    let l = l.compile_into(b);
    let r = r.compile_into(b);
    let out = b.binary(op, l, r);
    b.set_value_ty(out, ValueType::I1);
    out
}
