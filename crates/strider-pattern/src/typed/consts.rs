//! Constant-literal typed builders.
//!
//! `int_const` / `signed_int_const` / `bool_const` / `float_const` match
//! a literal value; the `any_*` family matches any constant of a kind;
//! `int_const_any_of` matches one of a value set. `int_const`'s match is
//! width-masked, so `int_const(u128::MAX)` matches a width-relative
//! all-ones constant. `signed_int_const` additionally recognises the
//! zero-extended-narrow signed encoding that a strict `int_const` misses.

use rustc_hash::FxHashSet;

use strider_ir::node::{NodeId, NodeKind, ValueType};
use strider_ir::{ConstId, IRViewer};

use crate::matcher::match_pat::MatchPat;
use crate::matcher::{KindSpec, MatcherBuilder, PatValueRef};
use crate::template::template_pat::TemplatePat;
use crate::template::{TemplateBuilder, TemplateKind, TemplateTy, TmplValueRef};
use crate::typed::builder_like::BuilderLike;

/// Shared lowering for the fixed-shape constant leaves (`BoolConst`,
/// `FloatConst`): one `leaf` node, optionally pinned to an exact value type.
/// Written once over any [`BuilderLike`] so the match- and build-side impls
/// don't carry byte-for-byte twin bodies.
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

/// Match the integer constant `v` (width-aware: masks `v` and the stored
/// payload to the matched node's output width before comparing).
pub struct IntConst {
    v: u128,
}

/// Shared match-time read for the integer-constant predicates: finds the
/// matched node's first value output and returns the stored value together with
/// that output's type.  The leaf's [`KindSpec::Variant`] already guarantees the
/// node is an `IntConst`, so this only locates and reads the value output
/// (`int_const_u128` is itself kind-checked, so it stays correct even off that
/// path).
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

/// Build a match-side `IntConst`-variant leaf, optionally pinning its
/// output type, then install `pred` as the node predicate. `pred` receives
/// the matched node's stored value and output type (already located via
/// [`first_int_const_value`], which the [`KindSpec::Variant`] prefilter
/// guarantees succeeds for an `IntConst`); the predicate fails the match if
/// the value can't be read or the test returns `false`. Shared by every
/// `IntConst`-predicate leaf so the leaf+predicate scaffold lives once.
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
        // The leaf's Variant prefilter declares the expected kind; the
        // predicate width-masks against the constant node's own output type.
        int_const_leaf(b, None, move |stored, ty| {
            let mask = ty.bit_mask_u128();
            (stored & mask) == (v & mask)
        })
    }
}

/// Build-side twin for a constant whose value is already known at
/// pattern-build time: a [`ConstWith`] carrying the precomputed `v` via
/// [`TemplateKind::FnIntConst`] (so the instantiator interns the full
/// `u128` at the resolved output width — never truncating to `u64`) and the
/// given output-type policy. Shared by every `*Const` build impl whose
/// value is constant.
fn const_template(v: u128, ty: TemplateTy) -> ConstWith {
    ConstWith {
        kind: TemplateKind::FnIntConst(Box::new(move |_ctx| Ok(v))),
        ty,
    }
}

impl crate::template::template_pat::TemplatePat for IntConst {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        // Route the full u128 through the FnIntConst path so the instantiator
        // interns the value at the resolved output width without ever
        // truncating here (e.g. int_const(u128::MAX) on an I128 root must
        // produce a full-width all-ones, not a u64-truncated one).
        const_template(self.v, TemplateTy::InheritRoot).compile(b)
    }
}

/// Match the integer constant `v` (any width).
pub fn int_const(v: impl Into<u128>) -> IntConst {
    IntConst { v: v.into() }
}

/// Match a signed integer constant `v`, recognising exact, sign-extended,
/// and zero-extended-narrow encodings across widths.
///
/// Unlike a width-masked [`int_const`], this also recognises the
/// zero-extended-narrow form (e.g. a 32-bit `-50` widened to 64 bits by
/// zero-extension — `IntConst(0x0000_0000_FFFF_FFCE)` at `I64`), which a
/// strict bit-pattern `int_const((v as i128) as u128)` deliberately
/// misses. That is the capability `int_const` cannot replicate.
pub struct SignedIntConst {
    v: i64,
}

impl MatchPat for SignedIntConst {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        let v_unsigned: u128 = i128::from(self.v) as u128;
        // The leaf's Variant prefilter declares the expected kind; the
        // predicate checks the exact / sign-extended / zero-extended-narrow
        // value encodings against the constant node's output type.
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
        // Carry the full sign-extended two's-complement bit pattern to
        // instantiate time via FnIntConst so the instantiator interns at the
        // resolved output width.  A u64-truncated value would lose the upper
        // bits for I128+ roots.
        let v: u128 = i128::from(self.v) as u128; // full-width two's-complement
        const_template(v, TemplateTy::InheritRoot).compile(b)
    }
}

/// Match a signed integer constant `v` across width encodings.
pub fn signed_int_const(v: i64) -> SignedIntConst {
    SignedIntConst { v }
}

/// Match the boolean constant `b` at width `I1`.
pub struct BoolConst {
    b: bool,
}

impl MatchPat for BoolConst {
    fn compile(self, builder: &mut MatcherBuilder) -> PatValueRef {
        let expected: u128 = u128::from(self.b);
        // Use Variant prefilter (pinned to I1) + a value-reading predicate.
        // KindSpec::Exact cannot be used here because NodeKind::IntConst(ConstId)
        // stores an opaque interned id, not a value — the same value in two
        // different Functions will have different ConstIds.
        int_const_leaf(builder, Some(ValueType::I1), move |v, _ty| v == expected)
    }
}

impl crate::template::template_pat::TemplatePat for BoolConst {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        // Route through FnIntConst (fixed to I1) so the instantiator interns
        // the value into the Function at rewrite time (ConstId is
        // function-specific and cannot be materialised at pattern-build time).
        const_template(u128::from(self.b), TemplateTy::Fixed(ValueType::I1)).compile(b)
    }
}

/// Match the boolean constant `b` at width `I1`.
pub fn bool_const(b: bool) -> BoolConst {
    BoolConst { b }
}

/// Match the float constant whose IEEE 754 bit pattern equals `bits`.
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

/// Match the float constant whose IEEE 754 bit pattern equals `bits`.
pub fn float_const(bits: u64) -> FloatConst {
    FloatConst { bits }
}

/// Match any integer constant whose value fits in `u128` (I1..I128 always;
/// I256/I512 only when their high limbs are zero). Match-only.
pub struct AnyIntConst;

impl MatchPat for AnyIntConst {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        // All integer constants are now `IntConst(ConstId)` — a single
        // payload-uniform variant. The Variant prefilter selects them; the
        // predicate accepts any whose value reads back as `u128` (I1..I128
        // always fit; I256/I512 with nonzero high limbs are rejected because
        // `first_int_const_value` returns `None`, so the predicate never runs
        // for them).
        int_const_leaf(b, None, move |_v, _ty| true)
    }
}

/// Match any integer constant whose value fits in `u128` (I1..I128 always;
/// I256/I512 only when their high limbs are zero).
pub fn any_int_const() -> AnyIntConst {
    AnyIntConst
}

/// Match any boolean constant — an `IntConst` typed `I1`. Match-only.
pub struct AnyBoolConst;

impl MatchPat for AnyBoolConst {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        int_const_leaf(b, Some(ValueType::I1), |_v, _ty| true)
    }
}

/// Match any boolean constant.
pub fn any_bool_const() -> AnyBoolConst {
    AnyBoolConst
}

/// Match any `FloatConst`. Match-only.
pub struct AnyFloatConst;

impl MatchPat for AnyFloatConst {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        b.leaf(KindSpec::variant_of(&NodeKind::FloatConst(0)))
    }
}

/// Match any `FloatConst`.
pub fn any_float_const() -> AnyFloatConst {
    AnyFloatConst
}

/// Match an `IntConst` whose value is in `set`. Match-only.
pub struct IntConstAnyOf {
    set: FxHashSet<u128>,
}

impl MatchPat for IntConstAnyOf {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        // NodeKind::IntConst(ConstId) is payload-uniform — the value can only
        // be read through the Function's interner (not from the NodeKind
        // directly). Variant prefilter + a predicate that reads the value via
        // int_const_u128. Only ≤u64 values are in the set (int_const_any_of
        // accepts u64 inputs), so I256/I512 constants (whose value may not fit
        // u128) never match.
        let set = self.set;
        int_const_leaf(b, None, move |v, _ty| set.contains(&v))
    }
}

/// Match an `IntConst` whose value is one of `set`.
///
/// The value is read via the interner (`int_const_u128`), so any constant
/// whose value fits `u128` and equals a set member matches; an I256/I512
/// value too wide for `u128` never matches. The set is built from `u64`
/// inputs (the primary use-case is jump-table target addresses, which are
/// pointer-width — at most 64 bits).
pub fn int_const_any_of<I: IntoIterator<Item = u64>>(set: I) -> IntConstAnyOf {
    IntConstAnyOf {
        set: set.into_iter().map(u128::from).collect(),
    }
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

impl ConstWith {
    /// Types the materialised const from the rewrite root's *first value
    /// input* instead of its output.  Use on a rule whose root's output
    /// width differs from its operand width — e.g. `eq(add(x, C1), C2) →
    /// eq(x, C2 - C1)`, where the fresh `C2 - C1` const must take the
    /// operand width (`x`'s), not the comparison's `I1` output width.
    pub fn of_input_type(mut self) -> Self {
        self.ty = TemplateTy::InheritInput;
        self
    }
}

impl TemplatePat for ConstWith {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        // Materialise a leaf whose kind is computed at instantiation.
        // The placeholder KindSpec::Any is overwritten by set_template_kind
        // with the actual dynamic closure below — the placeholder value is
        // never used.
        let o = b.leaf(KindSpec::Any);
        b.set_template_kind(o, self.kind);
        // `InheritRoot` is the leaf's default (stamped by `TmplOutput::value`),
        // so only a non-default type needs an explicit override.
        match self.ty {
            TemplateTy::Fixed(t) => b.set_value_ty(o, t),
            TemplateTy::InheritInput => b.set_value_ty_inherit_input(o),
            TemplateTy::InheritRoot => {}
        }
        o
    }
}

/// Builds an `IntConst` node whose value is computed by `f` at rewrite
/// time. The closure receives the per-rewrite [`TemplateCtx`](crate::TemplateCtx)
/// and returns a `u128`. Used by the `int_const_with!` macro. The output
/// type inherits the rewrite root.
pub fn int_const_with_fn<F>(f: F) -> ConstWith
where
    F: Fn(&crate::TemplateCtx<'_>) -> anyhow::Result<u128> + 'static,
{
    ConstWith {
        // Use TemplateKind::FnIntConst so the instantiator interns the full
        // u128 value at the resolved output width without ever truncating to
        // u64 here.  This preserves large I128 constants through rewrites.
        kind: TemplateKind::FnIntConst(Box::new(f)),
        ty: TemplateTy::InheritRoot,
    }
}

/// Builds a boolean constant (an `IntConst` typed `I1`) whose value is
/// computed by `f` at rewrite time. Used by the `bool_const_with!` macro.
///
/// Uses [`TemplateKind::FnIntConst`] so the instantiator interns the value
/// via `intern_int_const` at rewrite time (ConstId is function-specific and
/// cannot be materialised at pattern-build time).
pub fn bool_const_with_fn<F>(f: F) -> ConstWith
where
    F: Fn(&crate::TemplateCtx<'_>) -> anyhow::Result<bool> + 'static,
{
    ConstWith {
        kind: TemplateKind::FnIntConst(Box::new(move |ctx| Ok(u128::from(f(ctx)?)))),
        ty: TemplateTy::Fixed(ValueType::I1),
    }
}

/// Builds a `FloatConst` node whose IEEE 754 bit pattern is computed by
/// `f` at rewrite time. Used by the `float_const_with!` macro. The
/// output type inherits the rewrite root.
pub fn float_const_with_fn<F>(f: F) -> ConstWith
where
    F: Fn(&crate::TemplateCtx<'_>) -> anyhow::Result<u64> + 'static,
{
    ConstWith {
        kind: TemplateKind::Fn(Box::new(move |ctx| Ok(NodeKind::FloatConst(f(ctx)?)))),
        ty: TemplateTy::InheritRoot,
    }
}
