//! The construction verbs shared by [`MatcherBuilder`] and [`TemplateBuilder`],
//! so a fixed-op typed struct declares its node shape once instead of in twin
//! `MatchPat` / `TemplatePat` bodies.
//!
//! The match/template split still holds: [`CompileInto`] is blanket-implemented
//! for `MatchPat` over the match builder and `TemplatePat` over the template
//! builder, so an operand only lowers into a builder its own side admits. That
//! is what keeps the compile-time wildcard-in-RHS guard.

use strider_ir::IntBinaryOp;
use strider_ir::node::ValueType;

use crate::matcher::match_pat::MatchPat;
use crate::matcher::{KindSpec, MatcherBuilder, PatNodeRef, PatValueRef};
use crate::template::template_pat::TemplatePat;
use crate::template::{TemplateBuilder, TmplNodeRef, TmplValueRef};

pub trait BuilderLike {
    type OutRef: Copy;
    type NodeRef: Copy;

    /// One value input, one value output.
    fn unary(&mut self, kind: KindSpec, inner: Self::OutRef) -> Self::OutRef;
    fn binary(&mut self, op: IntBinaryOp, l: Self::OutRef, r: Self::OutRef) -> Self::OutRef;
    /// A bare node with no inputs/outputs yet.
    fn node(&mut self, kind: KindSpec) -> Self::NodeRef;
    fn input(&mut self, node: Self::NodeRef, slot: usize, prod: Self::OutRef);
    fn value_output(&mut self, node: Self::NodeRef, slot: usize) -> Self::OutRef;
    fn set_value_ty(&mut self, out: Self::OutRef, ty: ValueType);
    /// No inputs, one value output.
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

/// Blanket-implemented for every [`MatchPat`] over [`MatcherBuilder`] and every
/// [`TemplatePat`] over [`TemplateBuilder`], so each side's operand
/// admissibility carries through to the generic `compile_into` bodies.
pub trait CompileInto<B: BuilderLike> {
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

// Free functions rather than a generic `CompileInto` impl per struct: the
// structs already carry direct `MatchPat` / `TemplatePat` impls, and a blanket
// `CompileInto` over those traits would collide with them.

pub(crate) fn compile_unary_kind<B, I>(b: &mut B, kind: KindSpec, inner: I) -> B::OutRef
where
    B: BuilderLike,
    I: CompileInto<B>,
{
    let i = inner.compile_into(b);
    b.unary(kind, i)
}

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

/// Two-input, one value output, optionally pinned to `out_ty`. Covers the
/// int/float comparisons (`I1`-pinned) and float-binary (un-pinned).
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

/// `binary` pinned to `I1`: the boolean-binary shape.
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
