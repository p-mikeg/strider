//! Constant-literal typed builders.
//!
//! `int_const` / `signed_int_const` / `bool_const` / `float_const` match
//! a literal value; the `any_*` family matches any constant of a kind;
//! `int_const_any_of` matches one of a value set. `int_const`'s match is
//! width-masked, so `int_const(u128::MAX)` matches a width-relative
//! all-ones constant. `signed_int_const` additionally recognises the
//! zero-extended-narrow signed encoding that a strict `int_const` misses.

use std::mem::{Discriminant, discriminant};

use rustc_hash::FxHashSet;

use strider_ir::IRViewer;
use strider_ir::node::{IntPayload, NodeId, NodeKind, ValueType};

use crate::matcher::KindSpec;
use crate::matcher::match_pat::MatchPat;
use crate::matcher::{MatcherBuilder, PatValueRef};
use crate::template::template_pat::TemplatePat;
use crate::template::{TemplateBuilder, TemplateKind, TemplateTy, TmplValueRef};

/// Match the integer constant `v` (width-aware: masks `v` and the stored
/// payload to the matched node's output width before comparing).
pub struct IntConst {
    v: u128,
}

/// The `IntConst` discriminant, used as the leaf [`KindSpec::Variant`] so the
/// matcher's kind index prefilters to integer-constant nodes BEFORE the
/// value-comparison predicate runs — the pattern declares its expected kind
/// structurally rather than hiding a discriminant check inside an opaque
/// closure.
fn int_const_discriminant() -> Discriminant<NodeKind> {
    discriminant(&NodeKind::IntConst(IntPayload::Small(0)))
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

impl MatchPat for IntConst {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        let v = self.v;
        // Declare the expected kind in the leaf so the kind index prefilters
        // to IntConst nodes; the predicate then only compares the value.
        let o = b.leaf(KindSpec::Variant(int_const_discriminant()));
        b.set_node_predicate(
            o,
            Box::new(move |m, node| {
                let Some((stored, ty)) = first_int_const_value(m.function(), node) else {
                    return false;
                };
                // Width-mask against the constant node's own output type.
                let mask = ty.bit_mask_u128();
                (stored & mask) == (v & mask)
            }),
        );
        o
    }
}

impl crate::template::template_pat::TemplatePat for IntConst {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        // Route the full u128 through the FnIntConst path so the instantiator
        // chooses Small vs Wide from the resolved output type without ever
        // truncating here (e.g. int_const(u128::MAX) on an I128 root must
        // produce a full-width all-ones, not a u64-truncated one).
        let v = self.v;
        int_const_with_fn(move |_ctx| Ok(v)).compile(b)
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
        // Declare the expected kind in the leaf so the kind index prefilters
        // to IntConst nodes; the predicate then only checks the value encoding.
        let o = b.leaf(KindSpec::Variant(int_const_discriminant()));
        b.set_node_predicate(
            o,
            Box::new(move |m, node| {
                let Some((stored, out_ty)) = first_int_const_value(m.function(), node) else {
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
            }),
        );
        o
    }
}

impl crate::template::template_pat::TemplatePat for SignedIntConst {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        // Carry the full sign-extended two's-complement bit pattern to
        // instantiate time via FnIntConst so the instantiator picks
        // Small vs Wide from the resolved output type.  A u64-truncated
        // Small(v as u64) loses the upper bits for I128+ roots.
        let v: u128 = i128::from(self.v) as u128; // full-width two's-complement
        int_const_with_fn(move |_ctx| Ok(v)).compile(b)
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
        let v: u64 = u64::from(self.b);
        let o = builder.leaf(KindSpec::Exact(NodeKind::IntConst(IntPayload::Small(v))));
        builder.set_value_ty(o, ValueType::I1);
        o
    }
}

impl crate::template::template_pat::TemplatePat for BoolConst {
    fn compile(self, builder: &mut TemplateBuilder) -> TmplValueRef {
        let v: u64 = u64::from(self.b);
        let o = builder.leaf(KindSpec::Exact(NodeKind::IntConst(IntPayload::Small(v))));
        builder.set_value_ty(o, ValueType::I1);
        o
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
        b.leaf(KindSpec::Exact(NodeKind::FloatConst(self.bits)))
    }
}

impl crate::template::template_pat::TemplatePat for FloatConst {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        b.leaf(KindSpec::Exact(NodeKind::FloatConst(self.bits)))
    }
}

/// Match the float constant whose IEEE 754 bit pattern equals `bits`.
pub fn float_const(bits: u64) -> FloatConst {
    FloatConst { bits }
}

/// Match any integer constant — `IntConst(Small(_))` (I1..I64) or
/// `IntConst(Wide(_))` whose stored value fits in `u128` (I80 / I128).
/// Match-only.
pub struct AnyIntConst;

impl MatchPat for AnyIntConst {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        // Both Small (≤I64) and Wide (I80+) integer constants are now the
        // single `IntConst` variant.  Use KindSpec::Variant to bucket by
        // discriminant, then accept Wide only when the stored value fits in
        // u128 (i.e. I80 or I128); I256/I512 are excluded.
        let int_const_d = discriminant(&NodeKind::IntConst(IntPayload::Small(0)));
        let o = b.leaf(KindSpec::Variant(int_const_d));
        b.set_node_predicate(
            o,
            Box::new(move |m, ir_node| {
                let f = m.function();
                match f.node_kind(ir_node) {
                    // Small inline constant — always matches.
                    NodeKind::IntConst(IntPayload::Small(_)) => true,
                    // Wide constant — accept only if the stored value fits in
                    // u128 (I80 or I128); I256/I512 are excluded.
                    NodeKind::IntConst(IntPayload::Wide(_)) => {
                        let out = f
                            .node_outputs(ir_node)
                            .iter()
                            .copied()
                            .find(|&o| f.value_kind(o).as_value().is_some());
                        out.is_some_and(|o| f.int_const_u128(o).is_some())
                    }
                    _ => false,
                }
            }),
        );
        o
    }
}

/// Match any integer constant — `IntConst(Small(_))` (I1..I64) or
/// `IntConst(Wide(_))` whose stored value fits in `u128` (I80 / I128).
pub fn any_int_const() -> AnyIntConst {
    AnyIntConst
}

/// Match any boolean constant — an `IntConst` typed `I1`. Match-only.
pub struct AnyBoolConst;

impl MatchPat for AnyBoolConst {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        let exemplar = NodeKind::IntConst(IntPayload::Small(0));
        let o = b.leaf(KindSpec::Variant(std::mem::discriminant(&exemplar)));
        b.set_value_ty(o, ValueType::I1);
        o
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
        let exemplar = NodeKind::FloatConst(0);
        b.leaf(KindSpec::Variant(std::mem::discriminant(&exemplar)))
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
        let exemplar = NodeKind::IntConst(IntPayload::Small(0));
        let set = self.set;
        b.leaf(KindSpec::VariantWith {
            discriminant: std::mem::discriminant(&exemplar),
            check: Box::new(move |k: &NodeKind| {
                // Only Small (≤I64) values match; Wide values are never in
                // the set (int_const_any_of accepts u64 inputs only).
                matches!(k, NodeKind::IntConst(IntPayload::Small(v)) if set.contains(&u128::from(*v)))
            }),
        })
    }
}

/// Match an inline `IntConst` whose value is one of `set`.
///
/// Matches only ≤I64 inline `IntConst(Small)` nodes; I80/I128/I256/I512
/// constants stored in the wide interner are intentionally excluded.  This
/// limitation is correct for the primary use-case (jump-table target
/// addresses, which are pointer-width — at most 64 bits).
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

impl TemplatePat for ConstWith {
    fn compile(self, b: &mut TemplateBuilder) -> TmplValueRef {
        // Materialise a leaf whose kind is computed at instantiation.
        // The exact-kind stamp from `leaf` is overwritten with the
        // dynamic closure.
        let o = b.leaf(KindSpec::Exact(NodeKind::IntConst(IntPayload::Small(0))));
        b.set_template_kind(o, self.kind);
        match self.ty {
            TemplateTy::Fixed(t) => b.set_value_ty(o, t),
            TemplateTy::InheritRoot => b.set_inherit_root_ty(o),
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
        // Use TemplateKind::FnIntConst so the instantiator routes the full
        // u128 value through the correct payload (Small for ≤I64, Wide via
        // the interner for I80/I128/I256/I512) without ever truncating to
        // u64 here.  This preserves large I128 constants through rewrites.
        kind: TemplateKind::FnIntConst(Box::new(f)),
        ty: TemplateTy::InheritRoot,
    }
}

/// Builds a boolean constant (an `IntConst(Small(b as u64))` typed `I1`) whose
/// value is computed by `f` at rewrite time. Used by the
/// `bool_const_with!` macro.
pub fn bool_const_with_fn<F>(f: F) -> ConstWith
where
    F: Fn(&crate::TemplateCtx<'_>) -> anyhow::Result<bool> + 'static,
{
    ConstWith {
        kind: TemplateKind::Fn(Box::new(move |ctx| {
            Ok(NodeKind::IntConst(IntPayload::Small(u64::from(f(ctx)?))))
        })),
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
