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

/// An `IntConst` leaf gated on `pred(stored_value, output_type)`. `IntConstId` is per function,
/// so the value is read at match time.
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

/// Build side of a literal: a negative is wrapped into the resolved width. A non-negative one
/// stays raw, since `template`'s shift-count saturation reads whether it exceeds the width.
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

/// A constant valid on both sides.
#[derive(Clone)]
pub(crate) enum Const {
    /// `v` wrapped into the matched node's output width; a negative is its two's complement there.
    Int(IBig),
    /// An `I1` constant.
    Bool(bool),
    /// Any member, each masked to the matched node's output width. Match-only.
    Set(Vec<IBig>),
    /// Any of the values however it was width-extended into the constant's own type. Buildable
    /// with one value.
    AnyWidth(Vec<i64>),
    /// All-ones at the constant's own integer width. On the build side a float root skips the
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
            Const::Set(set) => int_const_set(set),
            Const::AnyWidth(vs) => any_width_leaf(vs),
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
            Const::AnyWidth(vs) if vs.len() == 1 => {
                const_template(IBig::from(vs[0]), TemplateTy::InheritRoot)
            }
            Const::Set(_) | Const::AnyWidth(_) => return b.refuse(crate::expr::MATCH_SIDE),
            Const::AllOnes => int_const_with_fn(|ctx| {
                ctx.root_ty
                    .is_integer()
                    .then(|| eval::all_ones(ctx.root_ty))
                    .ok_or_else(crate::skip)
            }),
        };
        built.lower(b)
    }

    /// Matches exactly the negations of what `self` matches. Match-only.
    pub(crate) fn negated(&self) -> Expr {
        match self {
            Const::Int(v) => Const::Int(-v.clone()).into(),
            // `-1` is `1` at `I1`.
            Const::Bool(v) => Const::Bool(*v).into(),
            Const::AllOnes => Const::Int(IBig::from(1)).into(),
            Const::Set(set) => Const::Set(set.iter().map(|v| -v.clone()).collect()).into(),
            Const::AnyWidth(vs) => {
                let vs = vs.clone();
                int_const_leaf(None, move |stored, ty| {
                    let neg = eval::unsigned(-IBig::from(stored.clone()), ty);
                    any_width_holds(&vs, &neg, ty)
                })
                .into()
            }
        }
    }
}

/// A `FloatConst` of a width up to 64 bits holding `bits` with its sign bit flipped.
pub(crate) fn negated_float_const(bits: u64) -> Leaf {
    Leaf {
        pred: Some(Box::new(move |m, node| {
            let f = m.function();
            let NodeKind::FloatConst(stored) = *f.node_kind(node) else {
                return false;
            };
            let width = f
                .node_outputs(node)
                .first()
                .and_then(|&o| f.value_kind(o).as_value())
                .map_or(0, |ty| ty.bit_width());
            (1..=64).contains(&width) && stored == bits ^ (1 << (width - 1))
        })),
        ..Leaf::new(KindSpec::variant_of(&NodeKind::FloatConst(0)))
    }
}

/// The all-ones constant of whatever integer width it is matched at.
pub fn int_const_all_ones() -> MatchOnly {
    MatchOnly(Const::AllOnes.into())
}

/// True when `v` is a `w`-wide value: the zero or sign extension of its low `w` bits.
fn fits_width(v: u128, w: usize) -> bool {
    if w >= 128 {
        return true;
    }
    let above = v & !mask_below(w as u32);
    above == 0 || ((v >> (w - 1)) & 1 == 1 && above == !mask_below(w as u32))
}

/// True when `stored` (at `out_ty`) is `v`, or `v` held at a supported integer width (not `I1`)
/// no wider than `out_ty` and zero- or sign-extended to it. Every supported width is tried, not
/// only powers of two.
fn value_at_some_width(stored: &UBig, out_ty: ValueType, v: u128) -> bool {
    match u128::try_from(stored) {
        Ok(s) => narrow_at_some_width(s, out_ty, v),
        Err(_) => past_the_carrier(stored, out_ty, v),
    }
}

/// [`value_at_some_width`] for a `stored` inside the `u128` carrier.
fn narrow_at_some_width(stored: u128, out_ty: ValueType, v: u128) -> bool {
    let output_width = out_ty.bit_width();
    if output_width == 0 {
        return false;
    }
    let output_mask = mask_below(output_width as u32);
    // `v` must fit the width, or `0x1234` matches a stored `0x34`.
    if (stored & output_mask) == (v & output_mask) && fits_width(v, output_width) {
        return true;
    }
    // Not `I1`: every odd `v` would match `1` and all-ones.
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
        // Past 128 bits the sign extension leaves the `u128` carrier.
        if output_width <= 128
            && sign_bit_w != 0
            && (low & sign_bit_w) != 0
            && above == above_w_mask
        {
            return true; // sign-extended form
        }
    }
    false
}

/// [`value_at_some_width`] for a `stored` past `u128`: `v` sign-extended out of `I128`, or
/// zero-extended from a width between `I128` and `out_ty`.
fn past_the_carrier(stored: &UBig, out_ty: ValueType, v: u128) -> bool {
    let i128_ty = ValueType::int(128);
    let lo = eval::mask(stored, i128_ty);
    let sign_extended = eval::signed(&lo, i128_ty)
        .is_some_and(|s| eval::unsigned(s, out_ty) == *stored)
        && value_at_some_width(&lo, i128_ty, v);
    sign_extended
        || ValueType::SUPPORTED.into_iter().any(|t| {
            let w = t.bit_width();
            t.is_integer()
                && w > 128
                && w < out_ty.bit_width()
                && eval::unsigned(IBig::from(v as i128), t) == *stored
        })
}

/// Matches any of `values`, exact or zero- / sign-extended into the constant's output type (e.g.
/// `128` as `0xFFFF_FFFF_FFFF_FF80` from `I8`, which [`int_const`] misses).
fn any_width_leaf(values: Vec<i64>) -> Leaf {
    int_const_leaf(None, move |stored, out_ty| {
        any_width_holds(&values, stored, out_ty)
    })
}

fn any_width_holds(values: &[i64], stored: &UBig, out_ty: ValueType) -> bool {
    let mut values = values.iter().map(|&v| i128::from(v) as u128);
    match u128::try_from(stored) {
        Ok(s) => values.any(|v| narrow_at_some_width(s, out_ty, v)),
        Err(_) => values.any(|v| past_the_carrier(stored, out_ty, v)),
    }
}

/// The [`int_const_any_width`] argument: one value, or a collection of them.
pub trait IntConstAnyWidthArg {
    type Pat;
    fn into_int_const_any_width(self) -> Self::Pat;
}

impl IntConstAnyWidthArg for i64 {
    type Pat = Op<()>;
    fn into_int_const_any_width(self) -> Op<()> {
        Op::new(Const::AnyWidth(vec![self]).into())
    }
}

fn any_width_set<T: Into<i64>>(values: impl IntoIterator<Item = T>) -> MatchOnly {
    MatchOnly(Const::AnyWidth(values.into_iter().map(Into::into).collect()).into())
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

/// Match `value` (or any member of a collection) however it was width-extended into the
/// constant. `value` must fit the width it is matched at, so `0x1234` never matches `0x34` at
/// `I8`.
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

/// Match a `FloatConst` with IEEE 754 bit pattern `bits`; given a [`Capture`], any `FloatConst`,
/// bound to it. As a template, a set bit above the evaluated width refuses the rewrite.
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

/// Any member of `set`, both sides masked to the output width as in the single-value form.
fn int_const_set(set: Vec<IBig>) -> Leaf {
    // The masked set, built once per width met.
    let by_width: RefCell<Vec<(usize, FxHashSet<UBig>)>> = RefCell::new(Vec::new());
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
    MatchOnly(Const::Set(values.into_iter().map(Into::into).collect()).into())
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

/// Match an `IntConst` equal to `value` wrapped into the output width; given a collection, any
/// member; given a [`Capture`], any integer constant, bound to it. Wrapping means `0x1234`
/// matches `0x34` at `I8` and `-1` matches all-ones at any width, but `u128::MAX` is
/// `2^128 - 1` at `I256` (see [`int_const_all_ones`]). `value` may be [`UBig`] or [`IBig`].
pub fn int_const<A: IntConstArg>(value: A) -> A::Pat {
    value.into_int_const()
}

/// A constant whose [`NodeKind`] is computed at rewrite time from the captured
/// LHS [`Bindings`](crate::Bindings). [`TemplatePat`] only.
pub struct ConstWith {
    kind: TemplateKind,
    ty: TemplateTy,
    /// Captures the closure reads, for the rewrite coverage check.
    caps: Vec<crate::Capture>,
    /// Captures read as an `Option`.
    opt_caps: Vec<crate::Capture>,
}

impl ConstWith {
    /// Declares a capture the closure resolves at instantiation.
    #[must_use]
    pub fn declare(mut self, c: crate::Capture) -> Self {
        self.caps.push(c);
        self
    }

    /// Declares a capture the closure reads as an `Option`; it may bind on only some arms.
    #[must_use]
    pub fn declare_optional(mut self, c: crate::Capture) -> Self {
        self.opt_caps.push(c);
        self
    }

    pub(crate) fn lower(self, b: &mut TemplateBuilder) -> TmplValueRef {
        for c in self.caps {
            b.declare_capture(c);
        }
        for c in self.opt_caps {
            b.declare_optional_capture(c);
        }
        b.build(self.kind, &[], self.ty)
    }
}

impl IntoExpr for ConstWith {
    fn into_expr(self) -> Expr {
        self.into()
    }
}

impl TemplatePat for ConstWith {}
impl crate::template::template_pat::sealed::Sealed for ConstWith {}

/// Types the wrapped node's output to a bound LHS capture's width, for an interior node the
/// root cannot type (`Sless(x<<C, 0) -> Xor(Equal(And(x,mask),0),1)`: the `I1` root has no
/// `x`-wide input).
pub struct CaptureTyped(Expr);

impl IntoExpr for CaptureTyped {
    fn into_expr(self) -> Expr {
        self.0
    }
}

impl TemplatePat for CaptureTyped {}
impl crate::template::template_pat::sealed::Sealed for CaptureTyped {}

/// `inner`, typed to `cap`'s bound width.
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

/// A `FloatConst` whose IEEE 754 bit pattern `f` computes at rewrite time
/// (`TemplateTy::InheritRoot`). A set bit above that width refuses the rewrite.
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
    use super::{IBig, UBig, ValueType};

    fn value_at_some_width(stored: u128, ty: ValueType, v: u128) -> bool {
        super::value_at_some_width(&UBig::from(stored), ty, v)
    }

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

    /// A query wider than the candidate width does not match on shared low bits.
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

    fn ones(w: usize) -> UBig {
        (UBig::from(1u8) << w) - UBig::from(1u8)
    }

    /// `v`, a `from`-bit value, sign-extended to `to` bits.
    fn sext(v: &UBig, from: usize, to: usize) -> UBig {
        if (v.clone() >> (from - 1)) & UBig::from(1u8) == UBig::from(0u8) {
            v.clone()
        } else {
            v.clone() + (ones(to) ^ ones(from))
        }
    }

    /// `x` mod `2^w`.
    fn low_of(x: i64, w: usize) -> UBig {
        let m = IBig::from(1) << w;
        UBig::try_from(((IBig::from(x) % m.clone()) + m.clone()) % m).unwrap()
    }

    /// The rule from first principles: `x` held at a supported integer width
    /// `w` at most `out`'s (`I1` only when `out` is `I1`), so
    /// `-2^(w-1) <= x < 2^w`, then widened ONCE to `out` by zero or sign
    /// extension.
    fn reference(stored: &UBig, out: ValueType, x: i64) -> bool {
        let big_w = out.bit_width();
        ValueType::SUPPORTED
            .into_iter()
            .filter(|t| t.is_integer())
            .map(ValueType::bit_width)
            .filter(|&w| w <= big_w && (w > 1 || big_w == 1))
            .any(|w| {
                let half = IBig::from(1) << (w - 1);
                if IBig::from(x) < -half.clone() || IBig::from(x) >= (IBig::from(1) << w) {
                    return false;
                }
                let low = low_of(x, w);
                *stored == low
                    || (low >= UBig::try_from(half).unwrap() && *stored == sext(&low, w, big_w))
            })
    }

    /// A positive value whose top bit is set at its width, sign-extended to an
    /// intermediate width and then zero-extended, is that value widened at no
    /// single width, past the `u128` carrier as below it.
    #[test]
    fn a_chained_extension_is_no_single_widening() {
        for (x, w, mid, out) in [
            (200, 8, 64, 128),
            (200, 8, 128, 256),
            (200, 8, 256, 512),
            (163, 8, 128, 256),
        ] {
            let stored = sext(&UBig::from(x as u8), w, mid);
            let ty = ValueType::int(out);
            assert!(!reference(&stored, ty, x), "{x} via I{mid} at I{out}");
            assert!(
                !super::value_at_some_width(&stored, ty, x as u128),
                "{x} via I{mid} at I{out}"
            );
        }
    }

    #[test]
    fn any_width_agrees_with_the_exact_rule_up_to_512_bits() {
        let mut rng = strider_ir_test_utils::Rng(0x5eed_0fa1_1000_0001);
        let tys: Vec<ValueType> = ValueType::SUPPORTED
            .into_iter()
            .filter(|t| t.is_integer())
            .collect();
        let edge = |r: &mut strider_ir_test_utils::Rng| -> i64 {
            match r.below(7) {
                0 => r.below(600) as i64 - 300,
                1 => -(1i64 << r.below(63)),
                2 => (1i64 << r.below(63)) - r.below(3) as i64,
                3 => -((1i64 << r.below(63)) - r.below(3) as i64),
                4 => i64::MIN + r.below(3) as i64,
                5 => i64::MAX - r.below(3) as i64,
                _ => r.next_u64() as i64,
            }
        };
        for _ in 0..300_000 {
            let ty = tys[rng.below(tys.len())];
            let big_w = ty.bit_width();
            let x = edge(&mut rng);
            let base = if rng.below(5) == 0 { edge(&mut rng) } else { x };
            let widths: Vec<usize> = tys
                .iter()
                .map(|t| t.bit_width())
                .filter(|&w| w <= big_w)
                .collect();
            let w1 = widths[rng.below(widths.len())];
            let w2 = widths[rng.below(widths.len())].max(w1);
            let l1 = low_of(base, w1);
            let stored = match rng.below(7) {
                0 => l1,
                1 => sext(&l1, w1, big_w),
                2 => sext(&l1, w1, w2),
                3 => sext(&sext(&l1, w1, w2), w2, big_w),
                4 => ones(big_w),
                5 => {
                    let mut v = UBig::from(rng.next_u64());
                    for _ in 0..rng.below(8) {
                        v = (v << 64) | UBig::from(rng.next_u64());
                    }
                    v
                }
                _ => sext(&l1, w1, big_w) ^ (UBig::from(1u8) << rng.below(big_w)),
            } & ones(big_w);
            assert_eq!(
                super::value_at_some_width(&stored, ty, i128::from(x) as u128),
                reference(&stored, ty, x),
                "{x} at {ty:?}, stored {stored:#x}"
            );
        }
    }
}
