//! Constant-literal typed builders.
//!
//! `int_const` / `signed_int_const` / `bool_const` / `float_const`
//! match a literal value; the `any_*` family matches any constant of a
//! kind; `int_const_any_of` matches one of a value set;
//! `int_const_all_ones` matches the width-relative all-ones mask.

use std::collections::HashSet;

use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir::wide_const::WideConstStorage;

use crate::builder::{MatcherBuilder, PatOutRef};
use crate::match_pat::MatchPat;
use crate::pattern::KindSpec;
use crate::template::{TemplateBuilder, TemplateKind, TemplateTy, TmplOutRef};
use crate::template_pat::TemplatePat;

/// Match the integer constant `v` (width-aware: masks `v` and the stored
/// payload to the matched node's output width before comparing).
pub struct IntConst {
    v: u128,
}

impl MatchPat for IntConst {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let exemplar = NodeKind::IntConst(0);
        let v = self.v;
        let o = b.leaf(KindSpec::Variant(std::mem::discriminant(&exemplar)));
        b.set_node_limit(
            o,
            Box::new(move |m, node, ty| {
                let NodeKind::IntConst(stored) = *m.function().node_kind(node) else {
                    return false;
                };
                let mask = ty.bit_mask_u128();
                (stored & mask) == (v & mask)
            }),
        );
        o
    }
}

impl crate::template_pat::TemplatePat for IntConst {
    fn compile(self, b: &mut TemplateBuilder) -> TmplOutRef {
        // Build a concrete `IntConst(v)`; its output type inherits the
        // rewrite root.
        b.leaf(KindSpec::Exact(NodeKind::IntConst(self.v)))
    }
}

/// Match the integer constant `v` (any width).
#[must_use]
pub fn int_const(v: impl Into<u128>) -> IntConst {
    IntConst { v: v.into() }
}

/// Match a signed integer constant `v`, recognising exact, sign-extended,
/// and zero-extended-narrow encodings across widths.
pub struct SignedIntConst {
    v: i64,
}

impl MatchPat for SignedIntConst {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let exemplar = NodeKind::IntConst(0);
        let v_unsigned: u128 = i128::from(self.v) as u128;
        let o = b.leaf(KindSpec::Variant(std::mem::discriminant(&exemplar)));
        b.set_node_limit(
            o,
            Box::new(move |m, node, _ty| {
                let f = m.function();
                let NodeKind::IntConst(stored) = *f.node_kind(node) else {
                    return false;
                };
                // Match-time output type is the matched node's own first
                // value output (not the consumer-expected `_ty` width that
                // a cast walk-through would pass).
                let Some(out_ty) = f
                    .node_outputs(node)
                    .iter()
                    .find_map(|&out| f.output_kind(out).as_value())
                else {
                    return false;
                };
                let output_width = out_ty.bit_width();
                if output_width == 0 {
                    return false;
                }
                let output_mask = out_ty.bit_mask_u128();
                for &w in &[8usize, 16, 32, 64, 128] {
                    if w > output_width {
                        break;
                    }
                    let w_mask: u128 = if w >= 128 { u128::MAX } else { (1u128 << w) - 1 };
                    let low = stored & w_mask;
                    let v_low = v_unsigned & w_mask;
                    if low != v_low {
                        continue;
                    }
                    let above_w_mask = output_mask & !w_mask;
                    let above = stored & above_w_mask;
                    if above == 0 {
                        return true; // zero-extended form
                    }
                    let sign_bit_w = if w >= 128 { 0 } else { 1u128 << (w - 1) };
                    if sign_bit_w != 0 && (low & sign_bit_w) != 0 && above == above_w_mask {
                        return true; // sign-extended form
                    }
                }
                false
            }),
        );
        o
    }
}

impl crate::template_pat::TemplatePat for SignedIntConst {
    fn compile(self, b: &mut TemplateBuilder) -> TmplOutRef {
        // Materialise the sign-extended two's-complement bit pattern as
        // an `IntConst`; the output type inherits the rewrite root.
        let v: u128 = i128::from(self.v) as u128;
        b.leaf(KindSpec::Exact(NodeKind::IntConst(v)))
    }
}

/// Match a signed integer constant `v` across width encodings.
#[must_use]
pub fn signed_int_const(v: i64) -> SignedIntConst {
    SignedIntConst { v }
}

/// Match the boolean constant `b` at width `I1`.
pub struct BoolConst {
    b: bool,
}

impl MatchPat for BoolConst {
    fn compile(self, builder: &mut MatcherBuilder) -> PatOutRef {
        let v: u128 = u128::from(self.b);
        let o = builder.leaf(KindSpec::Exact(NodeKind::IntConst(v)));
        builder.set_output_ty(o, NodeOutputType::I1);
        o
    }
}

impl crate::template_pat::TemplatePat for BoolConst {
    fn compile(self, builder: &mut TemplateBuilder) -> TmplOutRef {
        let v: u128 = u128::from(self.b);
        let o = builder.leaf(KindSpec::Exact(NodeKind::IntConst(v)));
        builder.set_output_ty(o, NodeOutputType::I1);
        o
    }
}

/// Match the boolean constant `b` at width `I1`.
#[must_use]
pub fn bool_const(b: bool) -> BoolConst {
    BoolConst { b }
}

/// Match the float constant whose IEEE 754 bit pattern equals `bits`.
pub struct FloatConst {
    bits: u64,
}

impl MatchPat for FloatConst {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        b.leaf(KindSpec::Exact(NodeKind::FloatConst(self.bits)))
    }
}

impl crate::template_pat::TemplatePat for FloatConst {
    fn compile(self, b: &mut TemplateBuilder) -> TmplOutRef {
        b.leaf(KindSpec::Exact(NodeKind::FloatConst(self.bits)))
    }
}

/// Match the float constant whose IEEE 754 bit pattern equals `bits`.
#[must_use]
pub fn float_const(bits: u64) -> FloatConst {
    FloatConst { bits }
}

/// Match any `IntConst`. Match-only.
pub struct AnyIntConst;

impl MatchPat for AnyIntConst {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let exemplar = NodeKind::IntConst(0);
        b.leaf(KindSpec::Variant(std::mem::discriminant(&exemplar)))
    }
}

/// Match any `IntConst`.
#[must_use]
pub fn any_int_const() -> AnyIntConst {
    AnyIntConst
}

/// Match any boolean constant — an `IntConst` typed `I1`. Match-only.
pub struct AnyBoolConst;

impl MatchPat for AnyBoolConst {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let exemplar = NodeKind::IntConst(0);
        let o = b.leaf(KindSpec::Variant(std::mem::discriminant(&exemplar)));
        b.set_output_ty(o, NodeOutputType::I1);
        o
    }
}

/// Match any boolean constant.
#[must_use]
pub fn any_bool_const() -> AnyBoolConst {
    AnyBoolConst
}

/// Match any `FloatConst`. Match-only.
pub struct AnyFloatConst;

impl MatchPat for AnyFloatConst {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let exemplar = NodeKind::FloatConst(0);
        b.leaf(KindSpec::Variant(std::mem::discriminant(&exemplar)))
    }
}

/// Match any `FloatConst`.
#[must_use]
pub fn any_float_const() -> AnyFloatConst {
    AnyFloatConst
}

/// Match an `IntConst` whose value is in `set`. Match-only.
pub struct IntConstAnyOf {
    set: HashSet<u128>,
}

impl MatchPat for IntConstAnyOf {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let exemplar = NodeKind::IntConst(0);
        let set = self.set;
        b.leaf(KindSpec::VariantWith {
            discriminant: std::mem::discriminant(&exemplar),
            check: Box::new(move |k: &NodeKind| matches!(k, NodeKind::IntConst(v) if set.contains(v))),
        })
    }
}

/// Match an `IntConst` whose value is one of `set`.
#[must_use]
pub fn int_const_any_of<I: IntoIterator<Item = u64>>(set: I) -> IntConstAnyOf {
    IntConstAnyOf {
        set: set.into_iter().map(u128::from).collect(),
    }
}

/// Match an `IntConst` (or `IntConstWide`) whose stored value, masked to
/// the node's output width, equals the all-ones bit pattern. Match-only.
pub struct IntConstAllOnes;

impl MatchPat for IntConstAllOnes {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let o = b.leaf(KindSpec::Any);
        b.set_node_limit(
            o,
            Box::new(move |m, node, _ty| {
                let f = m.function();
                let Some(out_ty) = f
                    .node_outputs(node)
                    .iter()
                    .find_map(|&out| f.output_kind(out).as_value())
                else {
                    return false;
                };
                if !out_ty.is_integer() {
                    return false;
                }
                match *f.node_kind(node) {
                    NodeKind::IntConst(stored) => {
                        // IntConst rejects I256 / I512 at build time, so a
                        // stored all-ones at those widths is impossible;
                        // the wide branch handles those.
                        if matches!(out_ty, NodeOutputType::I256 | NodeOutputType::I512) {
                            return false;
                        }
                        let mask = out_ty.bit_mask_u128();
                        (stored & mask) == mask
                    }
                    NodeKind::IntConstWide(id) => {
                        let stored = f.wide_const(id);
                        let Some(all_ones) = WideConstStorage::all_ones(out_ty.byte_size()) else {
                            return false;
                        };
                        *stored == all_ones
                    }
                    _ => false,
                }
            }),
        );
        o
    }
}

/// Match a width-relative all-ones integer constant.
#[must_use]
pub fn int_const_all_ones() -> IntConstAllOnes {
    IntConstAllOnes
}

// ── Build-time constants computed from captures ───────────────────────

/// A build-only constant whose materialised [`NodeKind`] is computed at
/// rewrite time from the captured LHS [`Bindings`](crate::Bindings).
///
/// Produced by [`int_const_with_fn`] / [`bool_const_with_fn`] /
/// [`float_const_with_fn`] (and the `*_const_with!` macros). [`TemplatePat`]
/// only — it has no match form, so landing one on a rule's LHS is a
/// compile error.
pub struct ConstWith {
    kind: TemplateKind,
    ty: TemplateTy,
}

impl TemplatePat for ConstWith {
    fn compile(self, b: &mut TemplateBuilder) -> TmplOutRef {
        // Materialise a leaf whose kind is computed at instantiation.
        // The exact-kind stamp from `leaf` is overwritten with the
        // dynamic closure.
        let o = b.leaf(KindSpec::Exact(NodeKind::IntConst(0)));
        b.set_template_kind(o, self.kind);
        match self.ty {
            TemplateTy::Fixed(t) => b.set_output_ty(o, t),
            TemplateTy::InheritRoot => b.set_inherit_root_ty(o),
        }
        o
    }
}

/// Builds an `IntConst` node whose value is computed by `f` at rewrite
/// time. The closure receives the per-rewrite [`TemplateCtx`](crate::TemplateCtx)
/// and returns a `u128`. Used by the `int_const_with!` macro. The output
/// type inherits the rewrite root.
#[must_use]
pub fn int_const_with_fn<F>(f: F) -> ConstWith
where
    F: Fn(&crate::TemplateCtx<'_>) -> anyhow::Result<u128> + 'static,
{
    ConstWith {
        kind: TemplateKind::Fn(Box::new(move |ctx| Ok(NodeKind::IntConst(f(ctx)?)))),
        ty: TemplateTy::InheritRoot,
    }
}

/// Builds a boolean constant (an `IntConst(b as u128)` typed `I1`) whose
/// value is computed by `f` at rewrite time. Used by the
/// `bool_const_with!` macro.
#[must_use]
pub fn bool_const_with_fn<F>(f: F) -> ConstWith
where
    F: Fn(&crate::TemplateCtx<'_>) -> anyhow::Result<bool> + 'static,
{
    ConstWith {
        kind: TemplateKind::Fn(Box::new(move |ctx| {
            Ok(NodeKind::IntConst(u128::from(f(ctx)?)))
        })),
        ty: TemplateTy::Fixed(NodeOutputType::I1),
    }
}

/// Builds a `FloatConst` node whose IEEE 754 bit pattern is computed by
/// `f` at rewrite time. Used by the `float_const_with!` macro. The
/// output type inherits the rewrite root.
#[must_use]
pub fn float_const_with_fn<F>(f: F) -> ConstWith
where
    F: Fn(&crate::TemplateCtx<'_>) -> anyhow::Result<u64> + 'static,
{
    ConstWith {
        kind: TemplateKind::Fn(Box::new(move |ctx| Ok(NodeKind::FloatConst(f(ctx)?)))),
        ty: TemplateTy::InheritRoot,
    }
}

/// Build a width-relative all-ones `IntConst` operand into the template
/// `b`, returning its handle. The concrete value is computed at
/// instantiation time from the rewrite root's resolved output width
/// (the build-side counterpart of [`int_const_all_ones`]). Used by the
/// `bit_not` template lowering to feed the all-ones operand into an
/// `xor`.
pub(crate) fn template_all_ones(b: &mut TemplateBuilder) -> TmplOutRef {
    let o = b.leaf(KindSpec::Exact(NodeKind::IntConst(0)));
    b.set_template_kind(
        o,
        crate::template::TemplateKind::Fn(Box::new(|ctx| {
            Ok(NodeKind::IntConst(ctx.root_ty.bit_mask_u128()))
        })),
    );
    o
}
