//! Constant-literal typed builders.
//!
//! `int_const`'s match is width-masked, so `int_const(u128::MAX)` matches a
//! width-relative all-ones constant. `int_const_any_width` additionally
//! recognises the value held at a narrower width and widened into the
//! constant's type, by zero or by sign extension.

use std::cell::RefCell;

use rustc_hash::FxHashSet;

use strider_ir::node::{NodeId, NodeKind, ValueType};
use strider_ir::{ConstId, IRViewer};

use crate::capture::Capture;
use crate::matcher::match_pat::{CaptureExt, Captured, MatchPat};
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
    pred: impl Fn(u128, ValueType) -> bool + 'static + Send,
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
        caps: Vec::new(),
    }
}

impl crate::template::template_pat::TemplatePat for IntConst {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        const_template(self.v, TemplateTy::InheritRoot).compile(b)
    }
}

/// Every width a constant could have been widened FROM, ascending. `I1` is
/// excluded: it is a boolean, not an integer source width, and a one-bit
/// candidate would make every odd `v` match the constants `1` and all-ones.
const INT_WIDTHS: [usize; 15] = [
    8, 16, 24, 32, 40, 48, 56, 64, 72, 80, 96, 112, 128, 256, 512,
];

/// True when `stored`, an `out_ty`-wide constant, equals `v` at `out_ty`, or is
/// `v` held at one of [`INT_WIDTHS`] and widened to `out_ty` by zero extension
/// or by sign extension. Every `ValueType` width is a candidate rather than only
/// the powers of two: an `I40` constant is not reachable through an `I32` or
/// `I64` probe.
fn value_at_some_width(stored: u128, out_ty: ValueType, v: u128) -> bool {
    let output_width = out_ty.bit_width();
    if output_width == 0 {
        return false;
    }
    let output_mask = out_ty.bit_mask_u128();
    if (stored & output_mask) == (v & output_mask) {
        return true;
    }
    for &w in &INT_WIDTHS {
        if w > output_width {
            break;
        }
        let w_mask: u128 = if w >= 128 {
            u128::MAX
        } else {
            (1u128 << w) - 1
        };
        let low = stored & w_mask;
        if low != (v & w_mask) {
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
}

/// Matches `v` however it was width-extended into the constant's own output
/// type: exact, widened by zero extension, or widened by sign extension. A
/// 32-bit `-50` widened to `IntConst(0x0000_0000_FFFF_FFCE)` at `I64`, and a
/// positive `128` widened to `IntConst(0xFFFF_FFFF_FFFF_FF80)` from `I8`, are
/// both forms the bit-exact [`int_const`] cannot match.
pub struct IntConstAnyWidth {
    v: i64,
}

impl MatchPat for IntConstAnyWidth {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        let v: u128 = i128::from(self.v) as u128;
        int_const_leaf(b, None, move |stored, out_ty| {
            value_at_some_width(stored, out_ty, v)
        })
    }
}

impl crate::template::template_pat::TemplatePat for IntConstAnyWidth {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        let v: u128 = i128::from(self.v) as u128;
        const_template(v, TemplateTy::InheritRoot).compile(b)
    }
}

/// Match-only.
pub struct IntConstAnyWidthAnyOf {
    values: Vec<u128>,
}

impl MatchPat for IntConstAnyWidthAnyOf {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        let values = self.values;
        int_const_leaf(b, None, move |stored, out_ty| {
            values
                .iter()
                .any(|&v| value_at_some_width(stored, out_ty, v))
        })
    }
}

/// The [`int_const_any_width`] argument: one value, or a collection of them.
pub trait IntConstAnyWidthArg {
    type Pat;
    fn into_int_const_any_width(self) -> Self::Pat;
}

impl IntConstAnyWidthArg for i64 {
    type Pat = IntConstAnyWidth;
    fn into_int_const_any_width(self) -> IntConstAnyWidth {
        IntConstAnyWidth { v: self }
    }
}

fn any_width_set<T: Into<i64>>(values: impl IntoIterator<Item = T>) -> IntConstAnyWidthAnyOf {
    IntConstAnyWidthAnyOf {
        values: values
            .into_iter()
            .map(|v| i128::from(v.into()) as u128)
            .collect(),
    }
}

impl<T: Into<i64>> IntConstAnyWidthArg for Vec<T> {
    type Pat = IntConstAnyWidthAnyOf;
    fn into_int_const_any_width(self) -> IntConstAnyWidthAnyOf {
        any_width_set(self)
    }
}

impl<T: Into<i64>, const N: usize> IntConstAnyWidthArg for [T; N] {
    type Pat = IntConstAnyWidthAnyOf;
    fn into_int_const_any_width(self) -> IntConstAnyWidthAnyOf {
        any_width_set(self)
    }
}

impl<T: Into<i64> + Copy> IntConstAnyWidthArg for &[T] {
    type Pat = IntConstAnyWidthAnyOf;
    fn into_int_const_any_width(self) -> IntConstAnyWidthAnyOf {
        any_width_set(self.iter().copied())
    }
}

/// Match `value` however it was width-extended into the constant it is stored
/// in; given a collection, any member of it. An empty collection matches
/// nothing.
pub fn int_const_any_width<A: IntConstAnyWidthArg>(value: A) -> A::Pat {
    value.into_int_const_any_width()
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

/// The [`bool_const`] argument: a value, or a capture to bind any `I1`
/// constant to.
pub trait BoolConstArg {
    type Pat;
    fn into_bool_const(self) -> Self::Pat;
}

impl BoolConstArg for bool {
    type Pat = BoolConst;
    fn into_bool_const(self) -> BoolConst {
        BoolConst { b: self }
    }
}

impl BoolConstArg for Capture {
    type Pat = Captured<AnyBoolConst>;
    fn into_bool_const(self) -> Captured<AnyBoolConst> {
        AnyBoolConst.capture(self)
    }
}

/// Match an `I1` `IntConst` equal to `value`; given a [`Capture`], any of them,
/// bound to it.
pub fn bool_const<A: BoolConstArg>(value: A) -> A::Pat {
    value.into_bool_const()
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

/// The [`float_const`] argument: an IEEE 754 bit pattern, or a capture to bind
/// any `FloatConst` to.
pub trait FloatConstArg {
    type Pat;
    fn into_float_const(self) -> Self::Pat;
}

impl FloatConstArg for u64 {
    type Pat = FloatConst;
    fn into_float_const(self) -> FloatConst {
        FloatConst { bits: self }
    }
}

impl FloatConstArg for Capture {
    type Pat = Captured<AnyFloatConst>;
    fn into_float_const(self) -> Captured<AnyFloatConst> {
        AnyFloatConst.capture(self)
    }
}

/// Match a `FloatConst` whose IEEE 754 bit pattern equals `bits`; given a
/// [`Capture`], any `FloatConst`, bound to it.
pub fn float_const<A: FloatConstArg>(bits: A) -> A::Pat {
    bits.into_float_const()
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

/// An `IntConst` typed `I1`. Match-only.
pub struct AnyBoolConst;

impl MatchPat for AnyBoolConst {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        int_const_leaf(b, Some(ValueType::I1), |_v, _ty| true)
    }
}

/// Any `FloatConst`. Match-only.
pub struct AnyFloatConst;

impl MatchPat for AnyFloatConst {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        b.leaf(KindSpec::variant_of(&NodeKind::FloatConst(0)))
    }
}

/// Any integer constant, whatever its value.
pub fn any_int_const() -> AnyIntConst {
    AnyIntConst
}

/// Any `I1` integer constant, whatever its value.
pub fn any_bool_const() -> AnyBoolConst {
    AnyBoolConst
}

/// Any float constant, whatever its value.
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
        // needs a predicate rather than a KindSpec::Exact. Masked to the output
        // width on both sides like the single-value form, or
        // `int_const([u128::MAX])` would miss the `I32` all-ones constant
        // `int_const(u128::MAX)` finds. The width is only known per candidate,
        // so the masked set is built once per width met rather than scanning the
        // candidates for each.
        let set = self.set;
        let by_width: RefCell<Vec<(usize, FxHashSet<u128>)>> = RefCell::new(Vec::new());
        int_const_leaf(b, None, move |stored, ty| {
            let bits = ty.bit_width();
            let mut cache = by_width.borrow_mut();
            let masked = match cache.iter().find(|(w, _)| *w == bits) {
                Some((_, s)) => s,
                None => {
                    let mask = ty.bit_mask_u128();
                    cache.push((bits, set.iter().map(|v| v & mask).collect()));
                    &cache[cache.len() - 1].1
                }
            };
            masked.contains(&(stored & ty.bit_mask_u128()))
        })
    }
}

/// The [`int_const`] argument: one value, or a collection of them.
pub trait IntConstArg {
    type Pat;
    fn into_int_const(self) -> Self::Pat;
}

macro_rules! int_const_scalar {
    ($($t:ty),+ $(,)?) => {$(
        impl IntConstArg for $t {
            type Pat = IntConst;
            fn into_int_const(self) -> IntConst {
                IntConst { v: u128::from(self) }
            }
        }
    )+};
}
int_const_scalar!(u8, u16, u32, u64, u128);

fn const_set<T: Into<u128>>(values: impl IntoIterator<Item = T>) -> IntConstAnyOf {
    IntConstAnyOf {
        set: values.into_iter().map(Into::into).collect(),
    }
}

impl<T: Into<u128>> IntConstArg for Vec<T> {
    type Pat = IntConstAnyOf;
    fn into_int_const(self) -> IntConstAnyOf {
        const_set(self)
    }
}

impl<T: Into<u128>, const N: usize> IntConstArg for [T; N] {
    type Pat = IntConstAnyOf;
    fn into_int_const(self) -> IntConstAnyOf {
        const_set(self)
    }
}

impl IntConstArg for Capture {
    type Pat = Captured<AnyIntConst>;
    fn into_int_const(self) -> Captured<AnyIntConst> {
        AnyIntConst.capture(self)
    }
}

impl<T: Into<u128> + Copy> IntConstArg for &[T] {
    type Pat = IntConstAnyOf;
    fn into_int_const(self) -> IntConstAnyOf {
        const_set(self.iter().copied())
    }
}

/// Match an `IntConst` whose stored value, masked to the output width, equals
/// `value`; given a collection, any member of it; given a [`Capture`], any
/// integer constant, bound to it. An I256/I512 value too wide for `u128` never
/// matches, and an empty collection matches nothing.
///
/// The collection form stays one pattern vertex carrying a set-membership
/// test, so it costs what the scalar form costs.
pub fn int_const<A: IntConstArg>(value: A) -> A::Pat {
    value.into_int_const()
}

/// A constant whose [`NodeKind`] is computed at rewrite time from the captured
/// LHS [`Bindings`](crate::Bindings). [`TemplatePat`] only.
pub struct ConstWith {
    kind: TemplateKind,
    ty: TemplateTy,
    /// Captures the closure reads, declared so the rewrite coverage check sees
    /// them; the `*_const_with!` macros fill this from their capture list.
    caps: Vec<crate::Capture>,
}

impl ConstWith {
    /// Declares the captures the closure resolves at instantiation.
    #[must_use]
    pub fn declaring(mut self, caps: Vec<crate::Capture>) -> Self {
        self.caps = caps;
        self
    }
}

impl TemplatePat for ConstWith {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        // The placeholder kind is immediately overwritten by set_template_kind
        // and never read.
        let o = b.leaf(KindSpec::Any);
        b.set_template_kind(o, self.kind);
        for c in self.caps {
            b.declare_capture(c);
        }
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
    F: Fn(&crate::TemplateCtx<'_>) -> anyhow::Result<u128> + 'static + Send,
{
    ConstWith {
        // FnIntConst interns the full u128 at the resolved output width, so
        // large I128 constants survive a rewrite.
        kind: TemplateKind::FnIntConst(Box::new(f)),
        ty: TemplateTy::InheritRoot,
        caps: Vec::new(),
    }
}

/// An `IntConst` typed `I1` whose value `f` computes at rewrite time.
pub fn bool_const_with_fn<F>(f: F) -> ConstWith
where
    F: Fn(&crate::TemplateCtx<'_>) -> anyhow::Result<bool> + 'static + Send,
{
    ConstWith {
        kind: TemplateKind::FnIntConst(Box::new(move |ctx| Ok(u128::from(f(ctx)?)))),
        ty: TemplateTy::Fixed(ValueType::I1),
        caps: Vec::new(),
    }
}

/// A `FloatConst` whose IEEE 754 bit pattern `f` computes at rewrite time.
/// Output type inherits the rewrite root.
pub fn float_const_with_fn<F>(f: F) -> ConstWith
where
    F: Fn(&crate::TemplateCtx<'_>) -> anyhow::Result<u64> + 'static + Send,
{
    ConstWith {
        kind: TemplateKind::Fn(Box::new(move |ctx| Ok(NodeKind::FloatConst(f(ctx)?)))),
        ty: TemplateTy::InheritRoot,
        caps: Vec::new(),
    }
}

#[cfg(test)]
mod any_width_tests {
    use super::{ValueType, value_at_some_width};

    /// The IR holds constants at widths that are not powers of two, so probing
    /// only 8/16/32/64/128 cannot reach one stored at its own width.
    #[test]
    fn a_constant_at_a_non_power_of_two_width_matches_exactly() {
        // I40, top bit set, not all-ones: reachable through no narrower probe.
        let stored: u128 = 0x80_0000_0001;
        assert!(value_at_some_width(stored, ValueType::I40, stored));
        assert!(value_at_some_width(0x80_0001, ValueType::I24, 0x80_0001));
        assert!(value_at_some_width(
            0x80_0000_0000_0001,
            ValueType::I56,
            0x80_0000_0000_0001
        ));
    }

    /// The widening forms still work, and an unrelated value still misses.
    #[test]
    fn widened_forms_still_match_and_others_do_not() {
        // -1 held at I8 and sign-extended into I40.
        assert!(value_at_some_width(0xFF_FFFF_FFFF, ValueType::I40, 0xFF));
        // 0x80 held at I8 and zero-extended into I40.
        assert!(value_at_some_width(0x80, ValueType::I40, 0x80));
        assert!(!value_at_some_width(
            0x80_0000_0002,
            ValueType::I40,
            0x80_0000_0001
        ));
    }
}
