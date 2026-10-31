//! Constant-literal typed builders.
//!
//! `int_const`'s match wraps the query into the constant's own declared
//! width, so `int_const(u128::MAX)` matches a width-relative all-ones
//! constant up to I128 and, at I256 and I512, the constant `2^128 - 1` those
//! widths spell out. The all-ones of a width the query does not fix is
//! [`int_const_all_ones`], which no literal names: `int_const` names one
//! value, that one is relative to whatever width it is matched at.
//! `int_const_any_width` additionally recognises the value held at a narrower
//! width and widened into the constant's type, by zero or by sign extension.

use std::cell::RefCell;

use rustc_hash::FxHashSet;

use strider_ir::eval::{self, mask_below, sign_bit};
use strider_ir::node::{NodeId, NodeKind, ValueType};
use strider_ir::{IBig, UBig};
use strider_ir::{IRViewer, IntConstId};

use crate::capture::Capture;
use crate::expr::{Expr, IntoExpr, Leaf};
use crate::matcher::match_pat::{CaptureExt, Captured};
use crate::matcher::{KindSpec, MatcherBuilder, OutputKindSpec, PatValueRef};
use crate::template::template_pat::TemplatePat;
use crate::template::{TemplateBuilder, TemplateKind, TemplateTy, TmplValueRef};
use crate::typed::value_ops::{MatchOnly, Op};

/// The stored value of `node`'s first value output, with that output's type.
fn first_int_const_value(f: &strider_ir::Function, node: NodeId) -> Option<(&UBig, ValueType)> {
    let out = f
        .node_outputs(node)
        .iter()
        .copied()
        .find(|&o| f.value_kind(o).as_value().is_some())?;
    let stored = f.int_const(out)?;
    let ty = f.value_kind(out).as_value().expect("checked via find");
    Some((stored, ty))
}

/// An `IntConst`-variant leaf gated on `pred(stored_value, output_type)`,
/// failing if the value can't be read at all. `IntConstId` is per function, so
/// the variant prefilters and the value is read at match time.
fn int_const_leaf(
    pin: Option<ValueType>,
    pred: impl Fn(&UBig, ValueType) -> bool + 'static + Send,
) -> Leaf {
    let kind = KindSpec::variant_of(&NodeKind::IntConst(IntConstId::from_u32(0)));
    Leaf {
        out: pin.map(OutputKindSpec::Value),
        pred: Some(Box::new(move |m, node| {
            first_int_const_value(m.function(), node).is_some_and(|(v, ty)| pred(v, ty))
        })),
        ..Leaf::new(kind)
    }
}

/// Build-side twin for a value known at pattern-build time, interned at the
/// resolved output width, into which a negative is wrapped.
///
/// A non-negative value passes through raw: the shift-count saturation in
/// `template` reads whether the value exceeds the width, which a wrap here
/// would erase.
fn const_template(v: IBig, ty: TemplateTy) -> ConstWith {
    ConstWith {
        kind: TemplateKind::FnIntConst(Box::new(move |ctx| {
            Ok(
                UBig::try_from(v.clone())
                    .unwrap_or_else(|_| eval::unsigned(v.clone(), ctx.root_ty)),
            )
        })),
        ty,
        caps: Vec::new(),
        opt_caps: Vec::new(),
    }
}

/// A constant valid on both sides, given each side's form only when it is
/// lowered.
pub(crate) enum Const {
    /// Wraps `v` into the matched node's output width, and masks the stored
    /// payload to it, before comparing. `v` is signed and unbounded, so a
    /// negative names its two's complement at whatever width it is matched
    /// at.
    Int(IBig),
    /// An `I1` constant.
    Bool(bool),
    /// `v` however it was width-extended into the constant's own type.
    AnyWidth(i64),
    /// Every bit of the constant's own output type set, at any declared
    /// width. On the build side, all-ones of the width it is evaluated at; a
    /// float, whose `eval::all_ones` is 0 and no complement's mask, skips the
    /// rewrite.
    AllOnes,
}

impl Const {
    pub(crate) fn lower(self, b: &mut MatcherBuilder) -> PatValueRef {
        let leaf = match self {
            Const::Int(v) => int_const_leaf(None, move |stored, ty| {
                ty.is_integer() && eval::mask(stored, ty) == eval::unsigned(v.clone(), ty)
            }),
            Const::Bool(b) => {
                let expected = UBig::from(u8::from(b));
                int_const_leaf(Some(ValueType::I1), move |v, _ty| *v == expected)
            }
            Const::AnyWidth(v) => any_width_leaf(vec![i128::from(v) as u128]),
            Const::AllOnes => int_const_leaf(None, |stored, ty| {
                ty.is_integer() && *stored == eval::all_ones(ty)
            }),
        };
        leaf.lower(b)
    }

    pub(crate) fn lower_template(self, b: &mut TemplateBuilder) -> TmplValueRef {
        let built = match self {
            Const::Int(v) => const_template(v, TemplateTy::InheritRoot),
            Const::Bool(v) => {
                const_template(IBig::from(u8::from(v)), TemplateTy::Fixed(ValueType::I1))
            }
            Const::AnyWidth(v) => const_template(IBig::from(v), TemplateTy::InheritRoot),
            Const::AllOnes => int_const_with_fn(|ctx| {
                ctx.root_ty
                    .is_integer()
                    .then(|| eval::all_ones(ctx.root_ty))
                    .ok_or_else(crate::skip)
            }),
        };
        built.lower(b)
    }
}

/// The all-ones constant of whatever integer width it is matched at.
/// Match-only: no `u128` literal names an I256 or I512 all-ones.
pub fn int_const_all_ones() -> MatchOnly {
    MatchOnly(Const::AllOnes.into())
}

/// True when `v` is the zero or sign extension of its own low `w` bits, i.e.
/// a `w`-wide value.
fn fits_width(v: u128, w: usize) -> bool {
    if w >= 128 {
        return true;
    }
    let above = v & !mask_below(w as u32);
    above == 0 || ((v >> (w - 1)) & 1 == 1 && above == !mask_below(w as u32))
}

/// True when `v` is a value at `out_ty` that `stored`, an `out_ty`-wide
/// constant, equals, or is `v` held at a supported integer width (not `I1`) at
/// most as wide as `out_ty` and widened to it by zero or sign extension. Every
/// `ValueType` width is a candidate rather than only the powers of two: an
/// `I40` constant is not reachable through an `I32` or `I64` probe.
fn value_at_some_width(stored: u128, out_ty: ValueType, v: u128) -> bool {
    let output_width = out_ty.bit_width();
    if output_width == 0 {
        return false;
    }
    let output_mask = mask_below(output_width as u32);
    // `v` must itself BE a value at the width it is compared at, or a search
    // for `0x1234` matches a stored `0x34` on the low bits they share.
    if (stored & output_mask) == (v & output_mask) && fits_width(v, output_width) {
        return true;
    }
    // `I1` is a boolean, not a source width: a one-bit candidate would make
    // every odd `v` match the constants `1` and all-ones.
    for w in ValueType::SUPPORTED
        .into_iter()
        .filter(|t| t.is_integer() && !t.is_bool())
        .map(ValueType::bit_width)
    {
        if w > output_width {
            break;
        }
        let w_mask = mask_below(w as u32);
        let low = stored & w_mask;
        if low != (v & w_mask) || !fits_width(v, w) {
            continue;
        }
        let sign_bit_w = sign_bit(w as u32);
        let above_w_mask = output_mask & !w_mask;
        let above = stored & above_w_mask;
        if above == 0 {
            return true; // zero-extended form
        }
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
/// both forms the bit-exact [`int_const`] cannot match. Any member of
/// `values` matches; `value_at_some_width` works in the `u128` the query
/// literal is given in, so a stored value past it is no extension of one.
fn any_width_leaf(values: Vec<u128>) -> Leaf {
    int_const_leaf(None, move |stored, out_ty| {
        u128::try_from(stored)
            .is_ok_and(|s| values.iter().any(|&v| value_at_some_width(s, out_ty, v)))
    })
}

/// The [`int_const_any_width`] argument: one value, or a collection of them.
pub trait IntConstAnyWidthArg {
    type Pat;
    fn into_int_const_any_width(self) -> Self::Pat;
}

impl IntConstAnyWidthArg for i64 {
    type Pat = Op<()>;
    fn into_int_const_any_width(self) -> Op<()> {
        Op::new(Const::AnyWidth(self).into())
    }
}

fn any_width_set<T: Into<i64>>(values: impl IntoIterator<Item = T>) -> MatchOnly {
    let values = values
        .into_iter()
        .map(|v| i128::from(v.into()) as u128)
        .collect();
    MatchOnly(any_width_leaf(values).into())
}

impl<T: Into<i64>> IntConstAnyWidthArg for Vec<T> {
    type Pat = MatchOnly;
    fn into_int_const_any_width(self) -> MatchOnly {
        any_width_set(self)
    }
}

impl<T: Into<i64>, const N: usize> IntConstAnyWidthArg for [T; N] {
    type Pat = MatchOnly;
    fn into_int_const_any_width(self) -> MatchOnly {
        any_width_set(self)
    }
}

impl<T: Into<i64> + Copy> IntConstAnyWidthArg for &[T] {
    type Pat = MatchOnly;
    fn into_int_const_any_width(self) -> MatchOnly {
        any_width_set(self.iter().copied())
    }
}

/// Match `value` however it was width-extended into the constant it is stored
/// in; given a collection, any member of it. `value` must fit the width it is
/// matched at, so `0x1234` never matches `0x34` at `I8`. An empty collection
/// matches nothing.
pub fn int_const_any_width<A: IntConstAnyWidthArg>(value: A) -> A::Pat {
    value.into_int_const_any_width()
}

/// The [`bool_const`] argument: a value, or a capture to bind any `I1`
/// constant to.
pub trait BoolConstArg {
    type Pat;
    fn into_bool_const(self) -> Self::Pat;
}

impl BoolConstArg for bool {
    type Pat = Op<()>;
    fn into_bool_const(self) -> Op<()> {
        Op::new(Const::Bool(self).into())
    }
}

impl BoolConstArg for Capture {
    type Pat = Captured<MatchOnly>;
    fn into_bool_const(self) -> Captured<MatchOnly> {
        any_bool_const().capture(self)
    }
}

/// Match an `I1` `IntConst` equal to `value`; given a [`Capture`], any of them,
/// bound to it.
pub fn bool_const<A: BoolConstArg>(value: A) -> A::Pat {
    value.into_bool_const()
}

/// The [`float_const`] argument: an IEEE 754 bit pattern, or a capture to bind
/// any `FloatConst` to.
pub trait FloatConstArg {
    type Pat;
    fn into_float_const(self) -> Self::Pat;
}

impl FloatConstArg for u64 {
    type Pat = Op<()>;
    fn into_float_const(self) -> Op<()> {
        Op::new(Leaf::new(KindSpec::Exact(NodeKind::FloatConst(self))).into())
    }
}

impl FloatConstArg for Capture {
    type Pat = Captured<MatchOnly>;
    fn into_float_const(self) -> Captured<MatchOnly> {
        any_float_const().capture(self)
    }
}

/// Match a `FloatConst` whose IEEE 754 bit pattern equals `bits`; given a
/// [`Capture`], any `FloatConst`, bound to it. As a template, `bits` is the
/// pattern at the width the constant is evaluated at, and a set bit above
/// that width refuses the rewrite.
pub fn float_const<A: FloatConstArg>(bits: A) -> A::Pat {
    bits.into_float_const()
}

/// Any integer constant, whatever its value, at any declared width.
pub fn any_int_const() -> MatchOnly {
    MatchOnly(int_const_leaf(None, |_v, _ty| true).into())
}

/// Any `I1` integer constant, whatever its value.
pub fn any_bool_const() -> MatchOnly {
    MatchOnly(int_const_leaf(Some(ValueType::I1), |_v, _ty| true).into())
}

/// Any float constant, whatever its value.
pub fn any_float_const() -> MatchOnly {
    let kind = KindSpec::variant_of(&NodeKind::FloatConst(0));
    MatchOnly(Leaf::new(kind).into())
}

/// Any member of `set`, masked to the output width on both sides like the
/// single-value form, or `int_const([u128::MAX])` would miss the `I32`
/// all-ones constant `int_const(u128::MAX)` finds. The width is only known per
/// candidate, so the masked set is built once per width met rather than
/// scanning the candidates for each.
fn int_const_set(set: Vec<IBig>) -> MatchOnly {
    let by_width: RefCell<Vec<(usize, FxHashSet<UBig>)>> = RefCell::new(Vec::new());
    MatchOnly(
        int_const_leaf(None, move |stored, ty| {
            let bits = ty.bit_width();
            let mut cache = by_width.borrow_mut();
            let masked = match cache.iter().find(|(w, _)| *w == bits) {
                Some((_, s)) => s,
                None => {
                    cache.push((
                        bits,
                        set.iter().map(|v| eval::unsigned(v.clone(), ty)).collect(),
                    ));
                    &cache[cache.len() - 1].1
                }
            };
            masked.contains(&eval::mask(stored, ty))
        })
        .into(),
    )
}

/// The [`int_const`] argument: one value, or a collection of them.
pub trait IntConstArg {
    type Pat;
    fn into_int_const(self) -> Self::Pat;
}

macro_rules! int_const_scalar {
    ($($t:ty),+ $(,)?) => {$(
        impl IntConstArg for $t {
            type Pat = Op<()>;
            fn into_int_const(self) -> Op<()> {
                Op::new(Const::Int(IBig::from(self)).into())
            }
        }
    )+};
}
int_const_scalar!(u8, u16, u32, u64, u128, i8, i16, i32, i64, i128, UBig, IBig);

fn const_set<T: Into<IBig>>(values: impl IntoIterator<Item = T>) -> MatchOnly {
    int_const_set(values.into_iter().map(Into::into).collect())
}

impl<T: Into<IBig>> IntConstArg for Vec<T> {
    type Pat = MatchOnly;
    fn into_int_const(self) -> MatchOnly {
        const_set(self)
    }
}

impl<T: Into<IBig>, const N: usize> IntConstArg for [T; N] {
    type Pat = MatchOnly;
    fn into_int_const(self) -> MatchOnly {
        const_set(self)
    }
}

impl IntConstArg for Capture {
    type Pat = Captured<MatchOnly>;
    fn into_int_const(self) -> Captured<MatchOnly> {
        any_int_const().capture(self)
    }
}

impl<T: Into<IBig> + Copy> IntConstArg for &[T] {
    type Pat = MatchOnly;
    fn into_int_const(self) -> MatchOnly {
        const_set(self.iter().copied())
    }
}

/// Match an `IntConst` whose stored value equals `value` wrapped into the
/// output width; given a collection, any member of it; given a [`Capture`],
/// any integer constant, bound to it. The query is wrapped too, so `0x1234`
/// matches `0x34` at `I8` and `-1` matches the all-ones constant of any
/// width. `value` is any integer, [`UBig`] or [`IBig`] included, so a
/// constant of any declared width is nameable; a width-relative all-ones at
/// a width the query does not fix is [`int_const_all_ones`]. An empty
/// collection matches nothing.
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
    /// Those of them read as an `Option`, from the list's `kind?` entries.
    opt_caps: Vec<crate::Capture>,
}

impl ConstWith {
    /// Declares a capture the closure resolves at instantiation.
    #[must_use]
    pub fn declare(mut self, c: crate::Capture) -> Self {
        self.caps.push(c);
        self
    }

    /// Declares a capture the closure resolves to an `Option`, which the
    /// coverage check lets bind on only some alternation arms.
    #[must_use]
    pub fn declare_optional(mut self, c: crate::Capture) -> Self {
        self.opt_caps.push(c);
        self
    }

    pub(crate) fn lower(self, b: &mut TemplateBuilder) -> TmplValueRef {
        let o = b.dynamic_leaf(self.kind);
        for c in self.caps {
            b.declare_capture(c);
        }
        for c in self.opt_caps {
            b.declare_optional_capture(c);
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

impl IntoExpr for ConstWith {
    fn into_expr(self) -> Expr {
        self.into()
    }
}

impl TemplatePat for ConstWith {}

/// Types the wrapped node's value output to the width of a bound LHS capture,
/// for when an interior node's width comes from a captured operand the rewrite
/// root does not expose (`Sless(x<<C, 0) -> Xor(Equal(And(x,mask),0),1)`: the
/// `I1` root has no `x`-wide input to inherit).
pub struct CaptureTyped(Expr);

impl IntoExpr for CaptureTyped {
    fn into_expr(self) -> Expr {
        self.0
    }
}

impl TemplatePat for CaptureTyped {}

pub fn capture_typed<P: TemplatePat + 'static>(cap: crate::Capture, inner: P) -> CaptureTyped {
    CaptureTyped(inner.into_expr().capture_typed(cap))
}

/// An `IntConst` whose value `f` computes at rewrite time, at the width it is
/// evaluated at (`TemplateTy::InheritRoot`).
pub fn int_const_with_fn<F>(f: F) -> ConstWith
where
    F: Fn(&crate::TemplateCtx<'_>) -> anyhow::Result<strider_ir::UBig> + 'static + Send,
{
    ConstWith {
        kind: TemplateKind::FnIntConst(Box::new(f)),
        ty: TemplateTy::InheritRoot,
        caps: Vec::new(),
        opt_caps: Vec::new(),
    }
}

/// An `IntConst` typed `I1` whose value `f` computes at rewrite time.
pub fn bool_const_with_fn<F>(f: F) -> ConstWith
where
    F: Fn(&crate::TemplateCtx<'_>) -> anyhow::Result<bool> + 'static + Send,
{
    ConstWith {
        kind: TemplateKind::FnIntConst(Box::new(move |ctx| {
            Ok(strider_ir::UBig::from(u8::from(f(ctx)?)))
        })),
        ty: TemplateTy::Fixed(ValueType::I1),
        caps: Vec::new(),
        opt_caps: Vec::new(),
    }
}

/// A `FloatConst` whose IEEE 754 bit pattern `f` computes at rewrite time, at
/// the width it is evaluated at (`TemplateTy::InheritRoot`). A set bit above
/// that width refuses the rewrite.
pub fn float_const_with_fn<F>(f: F) -> ConstWith
where
    F: Fn(&crate::TemplateCtx<'_>) -> anyhow::Result<u64> + 'static + Send,
{
    ConstWith {
        kind: TemplateKind::Fn(Box::new(move |ctx| Ok(NodeKind::FloatConst(f(ctx)?)))),
        ty: TemplateTy::InheritRoot,
        caps: Vec::new(),
        opt_caps: Vec::new(),
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
        assert!(value_at_some_width(stored, ValueType::int(40), stored));
        assert!(value_at_some_width(
            0x80_0001,
            ValueType::int(24),
            0x80_0001
        ));
        assert!(value_at_some_width(
            0x80_0000_0000_0001,
            ValueType::int(56),
            0x80_0000_0000_0001
        ));
    }

    /// The widening forms still work, and an unrelated value still misses.
    #[test]
    fn widened_forms_still_match_and_others_do_not() {
        // -1 held at I8 and sign-extended into I40.
        assert!(value_at_some_width(
            0xFF_FFFF_FFFF,
            ValueType::int(40),
            0xFF
        ));
        // 0x80 held at I8 and zero-extended into I40.
        assert!(value_at_some_width(0x80, ValueType::int(40), 0x80));
        assert!(!value_at_some_width(
            0x80_0000_0002,
            ValueType::int(40),
            0x80_0000_0001
        ));
    }

    /// A query wider than the candidate width is not that width's value, so
    /// sharing its low bits is not a match. Comparing only the low `w` bits
    /// made every constant a hit for any query ending in the same byte.
    #[test]
    fn a_query_with_bits_above_the_candidate_width_does_not_match() {
        // 0 is not 256, 0x1_0000 or 0x1_0000_0000 at any width.
        for q in [256u128, 0x1_0000, 0x1_0000_0000] {
            assert!(
                !value_at_some_width(0, ValueType::int(64), q),
                "0 matched {q:#x}"
            );
        }
        // The realistic shape: hunting a magic number, hitting its low byte.
        assert!(!value_at_some_width(0x34, ValueType::int(64), 0x1234));
        assert!(!value_at_some_width(0x2A, ValueType::int(32), 0x122A));
        // Same hole on the sign-extended arm: -128 is not 384.
        let minus_128 = (-128i128) as u128;
        assert!(!value_at_some_width(minus_128, ValueType::int(64), 384));
    }

    /// The negative queries the doc promises still reach their stored form.
    #[test]
    fn a_negative_query_still_matches_both_widened_forms() {
        // -50 held at I32 and ZERO-extended into I64.
        let minus_50 = (-50i128) as u128;
        assert!(value_at_some_width(
            0x0000_0000_FFFF_FFCE,
            ValueType::int(64),
            minus_50
        ));
        // -1 held at I8 and sign-extended into I64.
        let minus_1 = (-1i128) as u128;
        assert!(value_at_some_width(
            u128::from(u64::MAX),
            ValueType::int(64),
            minus_1
        ));
    }
}
