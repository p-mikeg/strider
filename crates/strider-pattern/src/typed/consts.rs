//! Constant-literal typed builders.
//!
//! `int_const`'s match is width-masked, so `int_const(u128::MAX)` matches a
//! width-relative all-ones constant. `signed_int_const` additionally recognises
//! the zero-extended-narrow signed encoding that `int_const` misses.

use rustc_hash::FxHashSet;

use strider_ir::node::{NodeId, NodeKind, ValueType};
use strider_ir::{ConstId, IRViewer};

use crate::matcher::match_pat::MatchPat;
use crate::matcher::{KindSpec, MatcherBuilder, PatValueRef};
use crate::template::template_pat::TemplatePat;
use crate::template::{TemplateBuilder, TemplateKind, TemplateTy, TmplValueRef};
use crate::typed::builder_like::BuilderLike;

/// One `leaf` node, optionally pinned to an exact value type.
fn compile_const_leaf<B: BuilderLike>(
    b: &mut B,
    kind: KindSpec,
    ty: Option<ValueType>,
) -> B::OutRef {
    let o = b.leaf(kind);
    if let Some(t) = ty {
        b.set_value_ty(o, t);
    }
    o
}

/// Masks `v` and the stored payload to the matched node's output width before
/// comparing.
pub struct IntConst {
    v: u128,
}

/// The stored value of `node`'s first value output, with that output's type.
/// `None` for an I256/I512 value too wide for `u128`.
fn first_int_const_value(f: &strider_ir::Function, node: NodeId) -> Option<(u128, ValueType)> {
    let out = f
        .node_outputs(node)
        .iter()
        .copied()
        .find(|&o| f.value_kind(o).as_value().is_some())?;
    let stored = f.int_const_u128(out)?;
    let ty = f.value_kind(out).as_value().expect("checked via find");
    Some((stored, ty))
}

/// An `IntConst`-variant leaf gated on `pred(stored_value, output_type)`,
/// failing if the value can't be read at all.
fn int_const_leaf(
    b: &mut MatcherBuilder,
    pin: Option<ValueType>,
    pred: impl Fn(u128, ValueType) -> bool + 'static,
) -> PatValueRef {
    let o = b.leaf(KindSpec::variant_of(&NodeKind::IntConst(
        ConstId::from_u32(0),
    )));
    if let Some(t) = pin {
        b.set_value_ty(o, t);
    }
    b.set_node_predicate(
        o,
        Box::new(move |m, node| {
            first_int_const_value(m.function(), node).is_some_and(|(v, ty)| pred(v, ty))
        }),
    );
    o
}

impl MatchPat for IntConst {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        let v = self.v;
        int_const_leaf(b, None, move |stored, ty| {
            let mask = ty.bit_mask_u128();
            (stored & mask) == (v & mask)
        })
    }
}

/// Build-side twin for a value known at pattern-build time, interned at the
/// resolved output width.
fn const_template(v: u128, ty: TemplateTy) -> ConstWith {
    ConstWith {
        kind: TemplateKind::FnIntConst(Box::new(move |_ctx| Ok(v))),
        ty,
    }
}

impl crate::template::template_pat::TemplatePat for IntConst {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        const_template(self.v, TemplateTy::InheritRoot).compile(b)
    }
}

pub fn int_const(v: impl Into<u128>) -> IntConst {
    IntConst { v: v.into() }
}

/// Recognises exact, sign-extended, and zero-extended-narrow encodings of `v`
/// across widths. The zero-extended-narrow form (a 32-bit `-50` widened to
/// `IntConst(0x0000_0000_FFFF_FFCE)` at `I64`) is what a strict bit-pattern
/// `int_const((v as i128) as u128)` cannot match.
pub struct SignedIntConst {
    v: i64,
}

impl MatchPat for SignedIntConst {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        let v_unsigned: u128 = i128::from(self.v) as u128;
        int_const_leaf(b, None, move |stored, out_ty| {
            let output_width = out_ty.bit_width();
            if output_width == 0 {
                return false;
            }
            let output_mask = out_ty.bit_mask_u128();
            for &w in &[8usize, 16, 32, 64, 128] {
                if w > output_width {
                    break;
                }
                let w_mask: u128 = if w >= 128 {
                    u128::MAX
                } else {
                    (1u128 << w) - 1
                };
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
        })
    }
}

impl crate::template::template_pat::TemplatePat for SignedIntConst {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        let v: u128 = i128::from(self.v) as u128;
        const_template(v, TemplateTy::InheritRoot).compile(b)
    }
}

pub fn signed_int_const(v: i64) -> SignedIntConst {
    SignedIntConst { v }
}

/// Matches at width `I1`.
pub struct BoolConst {
    b: bool,
}

impl MatchPat for BoolConst {
    fn compile(self, builder: &mut MatcherBuilder) -> PatValueRef {
        let expected: u128 = u128::from(self.b);
        // KindSpec::Exact won't work: NodeKind::IntConst(ConstId) stores an
        // opaque interned id, and the same value in two different Functions
        // gets different ConstIds. Hence Variant prefilter plus a value read.
        int_const_leaf(builder, Some(ValueType::I1), move |v, _ty| v == expected)
    }
}

impl crate::template::template_pat::TemplatePat for BoolConst {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        // ConstId is function-specific, so the value must be interned at
        // rewrite time, not at pattern-build time.
        const_template(u128::from(self.b), TemplateTy::Fixed(ValueType::I1)).compile(b)
    }
}

pub fn bool_const(b: bool) -> BoolConst {
    BoolConst { b }
}

/// `bits` is the IEEE 754 bit pattern.
pub struct FloatConst {
    bits: u64,
}

impl MatchPat for FloatConst {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        compile_const_leaf(b, KindSpec::Exact(NodeKind::FloatConst(self.bits)), None)
    }
}

impl crate::template::template_pat::TemplatePat for FloatConst {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        compile_const_leaf(b, KindSpec::Exact(NodeKind::FloatConst(self.bits)), None)
    }
}

pub fn float_const(bits: u64) -> FloatConst {
    FloatConst { bits }
}

/// Any integer constant whose value fits `u128` (I1..I128 always; I256/I512
/// only when their high limbs are zero). Match-only.
pub struct AnyIntConst;

impl MatchPat for AnyIntConst {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        // The width filter is implicit: `first_int_const_value` returns `None`
        // for an I256/I512 with nonzero high limbs, so the predicate never runs.
        int_const_leaf(b, None, move |_v, _ty| true)
    }
}

pub fn any_int_const() -> AnyIntConst {
    AnyIntConst
}

/// An `IntConst` typed `I1`. Match-only.
pub struct AnyBoolConst;

impl MatchPat for AnyBoolConst {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        int_const_leaf(b, Some(ValueType::I1), |_v, _ty| true)
    }
}

pub fn any_bool_const() -> AnyBoolConst {
    AnyBoolConst
}

/// Match-only.
pub struct AnyFloatConst;

impl MatchPat for AnyFloatConst {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        b.leaf(KindSpec::variant_of(&NodeKind::FloatConst(0)))
    }
}

pub fn any_float_const() -> AnyFloatConst {
    AnyFloatConst
}

/// Match-only.
pub struct IntConstAnyOf {
    set: FxHashSet<u128>,
}

impl MatchPat for IntConstAnyOf {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        // The value lives in the Function's interner, not the NodeKind, so this
        // needs a predicate rather than a KindSpec::Exact.
        let set = self.set;
        int_const_leaf(b, None, move |v, _ty| set.contains(&v))
    }
}

/// Match an `IntConst` whose value is one of `set`. An I256/I512 value too
/// wide for `u128` never matches. The set takes `u64` inputs.
pub fn int_const_any_of<I: IntoIterator<Item = u64>>(set: I) -> IntConstAnyOf {
    IntConstAnyOf {
        set: set.into_iter().map(u128::from).collect(),
    }
}

/// A constant whose [`NodeKind`] is computed at rewrite time from the captured
/// LHS [`Bindings`](crate::Bindings). [`TemplatePat`] only.
pub struct ConstWith {
    kind: TemplateKind,
    ty: TemplateTy,
}

impl TemplatePat for ConstWith {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        // The placeholder kind is immediately overwritten by set_template_kind
        // and never read.
        let o = b.leaf(KindSpec::Any);
        b.set_template_kind(o, self.kind);
        // `InheritRoot` is already the leaf default, so it needs no override.
        match self.ty {
            TemplateTy::Fixed(t) => b.set_value_ty(o, t),
            TemplateTy::InheritBinding(cap) => b.set_value_ty_of_binding(o, cap),
            TemplateTy::InheritRoot => {}
        }
        o
    }
}

/// Types the wrapped node's value output to the width of a bound LHS capture,
/// for when an interior node's width comes from a captured operand the rewrite
/// root does not expose (`Sless(x<<C, 0) -> Xor(Equal(And(x,mask),0),1)`: the
/// `I1` root has no `x`-wide input to inherit).
pub struct CaptureTyped<P> {
    cap: crate::Capture,
    inner: P,
}

impl<P: TemplatePat> TemplatePat for CaptureTyped<P> {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        let out = self.inner.compile(b);
        b.set_value_ty_of_binding(out, self.cap);
        out
    }
}

pub fn capture_typed<P: TemplatePat>(cap: crate::Capture, inner: P) -> CaptureTyped<P> {
    CaptureTyped { cap, inner }
}

/// An `IntConst` whose value `f` computes at rewrite time. Output type inherits
/// the rewrite root.
pub fn int_const_with_fn<F>(f: F) -> ConstWith
where
    F: Fn(&crate::TemplateCtx<'_>) -> anyhow::Result<u128> + 'static,
{
    ConstWith {
        // FnIntConst interns the full u128 at the resolved output width, so
        // large I128 constants survive a rewrite.
        kind: TemplateKind::FnIntConst(Box::new(f)),
        ty: TemplateTy::InheritRoot,
    }
}

/// An `IntConst` typed `I1` whose value `f` computes at rewrite time.
pub fn bool_const_with_fn<F>(f: F) -> ConstWith
where
    F: Fn(&crate::TemplateCtx<'_>) -> anyhow::Result<bool> + 'static,
{
    ConstWith {
        kind: TemplateKind::FnIntConst(Box::new(move |ctx| Ok(u128::from(f(ctx)?)))),
        ty: TemplateTy::Fixed(ValueType::I1),
    }
}

/// A `FloatConst` whose IEEE 754 bit pattern `f` computes at rewrite time.
/// Output type inherits the rewrite root.
pub fn float_const_with_fn<F>(f: F) -> ConstWith
where
    F: Fn(&crate::TemplateCtx<'_>) -> anyhow::Result<u64> + 'static,
{
    ConstWith {
        kind: TemplateKind::Fn(Box::new(move |ctx| Ok(NodeKind::FloatConst(f(ctx)?)))),
        ty: TemplateTy::InheritRoot,
    }
}
