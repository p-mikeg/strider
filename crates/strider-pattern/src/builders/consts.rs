//! Constant-literal pattern constructors.
//!
//! Ported from `strider-analyze::pattern::pat::ctor::wildcards`.  Each
//! builder constructs a single-node `PatGraph<R>` whose `KindSpec` is
//! either `Exact` (a literal value), `Variant` (any value of the kind),
//! or `VariantWith` (a value-set filter).  The build spec mirrors the
//! match spec for the `Concrete`-roled builders so they can also be
//! used as rewrite RHSs.

use strider_ir::node::{NodeKind, NodeOutputType};

use crate::pat_graph::{
    BuildKind, BuildSpec, BuildTy, Concrete, KindSpec, NodeData, PatGraph, Wildcard,
};

use super::Pat;

/// Match the integer constant `v` (any width).
///
/// **Width-aware:** masks `v` and the stored payload to the matched
/// IR node's output bit-width before comparing.  So `int_const(-1i64
/// as u128)` matches `IntConst(0xff):I8`, `IntConst(0xffff):I16`,
/// `IntConst(0xffff_ffff):I32`, … — without per-arch width pinning.
///
/// In build position (RHS of a rewrite rule), constructs an
/// `IntConst(v)` whose output type inherits the rewrite root.
#[must_use]
pub fn int_const(v: u128) -> Pat<Concrete> {
    let exemplar = NodeKind::IntConst(0);
    let mut g: PatGraph<Concrete> = PatGraph::new();
    let post_match: crate::pat_graph::PostMatchFn = std::rc::Rc::new(move |ctx, node, ty, _b| {
        let NodeKind::IntConst(stored) = *ctx.function.node_kind(node) else {
            return false;
        };
        let mask = ty.bit_mask_u128();
        (stored & mask) == (v & mask)
    });
    let n = g.add_node(NodeData {
        kind: KindSpec::Variant(std::mem::discriminant(&exemplar)),
        output_ty: None,
        capture: None,
        post_match: Some(post_match),
        build_spec: Some(BuildSpec {
            kind: BuildKind::Fn(std::rc::Rc::new(move |ctx| {
                let mask = ctx.root_ty.bit_mask_u128();
                Ok(NodeKind::IntConst(v & mask))
            })),
            ty: BuildTy::InheritRoot,
        }),

        force_ordered: false,
    });
    g.set_root(n);
    Pat::from_graph(g)
}

/// Match the boolean constant `b` at width `I1`.  Booleans are 1-bit
/// integers, so this matches `IntConst(0|1)` typed `I1`.
#[must_use]
pub fn bool_const(b: bool) -> Pat<Concrete> {
    let v: u128 = u128::from(b);
    let mut g: PatGraph<Concrete> = PatGraph::new();
    let n = g.add_node(NodeData {
        kind: KindSpec::Exact(NodeKind::IntConst(v)),
        output_ty: Some(NodeOutputType::I1),
        capture: None,
        post_match: None,
        build_spec: Some(BuildSpec {
            kind: BuildKind::Exact(NodeKind::IntConst(v)),
            ty: BuildTy::Fixed(NodeOutputType::I1),
        }),
    
        force_ordered: false,
    });
    g.set_root(n);
    Pat::from_graph(g)
}

/// Match the float constant whose IEEE 754 bit pattern equals `bits`.
#[must_use]
pub fn float_const(bits: u64) -> Pat<Concrete> {
    let mut g: PatGraph<Concrete> = PatGraph::new();
    let n = g.add_node(NodeData {
        kind: KindSpec::Exact(NodeKind::FloatConst(bits)),
        output_ty: None,
        capture: None,
        post_match: None,
        build_spec: Some(BuildSpec {
            kind: BuildKind::Exact(NodeKind::FloatConst(bits)),
            ty: BuildTy::InheritRoot,
        }),
    
        force_ordered: false,
    });
    g.set_root(n);
    Pat::from_graph(g)
}

/// Match any `IntConst`.  Wildcard role (no fixed value, no build
/// path without a capture).
#[must_use]
pub fn any_int_const() -> Pat<Wildcard> {
    let exemplar = NodeKind::IntConst(0);
    let mut g: PatGraph<Wildcard> = PatGraph::new();
    let n = g.add_node(NodeData {
        kind: KindSpec::Variant(std::mem::discriminant(&exemplar)),
        output_ty: None,
        capture: None,
        post_match: None,
        build_spec: None,
    
        force_ordered: false,
    });
    g.set_root(n);
    Pat::from_graph(g)
}

/// Match any boolean constant — an `IntConst` typed `I1`.
///
/// The `I1` width filter is recorded in `output_ty`; the matcher will
/// honour it once the output-type guard is wired (it currently lives
/// in `node_data.output_ty` but is not yet checked — pinning it here
/// keeps the data path correct for when that guard turns on).
#[must_use]
pub fn any_bool_const() -> Pat<Wildcard> {
    let exemplar = NodeKind::IntConst(0);
    let mut g: PatGraph<Wildcard> = PatGraph::new();
    let n = g.add_node(NodeData {
        kind: KindSpec::Variant(std::mem::discriminant(&exemplar)),
        output_ty: Some(NodeOutputType::I1),
        capture: None,
        post_match: None,
        build_spec: None,
    
        force_ordered: false,
    });
    g.set_root(n);
    Pat::from_graph(g)
}

/// Match any `FloatConst`.
#[must_use]
pub fn any_float_const() -> Pat<Wildcard> {
    let exemplar = NodeKind::FloatConst(0);
    let mut g: PatGraph<Wildcard> = PatGraph::new();
    let n = g.add_node(NodeData {
        kind: KindSpec::Variant(std::mem::discriminant(&exemplar)),
        output_ty: None,
        capture: None,
        post_match: None,
        build_spec: None,
    
        force_ordered: false,
    });
    g.set_root(n);
    Pat::from_graph(g)
}

/// Match an `IntConst` whose value is in `set`.  Useful when querying
/// a call site whose target may be one of several known addresses.
#[must_use]
pub fn int_const_any_of<I: IntoIterator<Item = u64>>(set: I) -> Pat<Wildcard> {
    let set: std::collections::HashSet<u128> = set.into_iter().map(u128::from).collect();
    let exemplar = NodeKind::IntConst(0);
    let check: std::rc::Rc<dyn Fn(&NodeKind) -> bool> = std::rc::Rc::new(move |k: &NodeKind| -> bool {
        matches!(k, NodeKind::IntConst(v) if set.contains(v))
    });
    let mut g: PatGraph<Wildcard> = PatGraph::new();
    let n = g.add_node(NodeData {
        kind: KindSpec::VariantWith {
            discriminant: std::mem::discriminant(&exemplar),
            check,
        },
        output_ty: None,
        capture: None,
        post_match: None,
        build_spec: None,
    
        force_ordered: false,
    });
    g.set_root(n);
    Pat::from_graph(g)
}

/// Match a signed integer constant `v`.
///
/// Unlike [`int_const`] (which is a strict bit-pattern match at the
/// output width), `signed_int_const` recognises every reasonable
/// encoding of the same source-level signed value across widths:
///
/// * Exact bit-pattern: `IntConst(0xFFFFFFCE):U32` matches
///   `signed_int_const(-50)` because `0xFFFFFFCE` is `-50` reinterpreted
///   as `i32`.
/// * Sign-extended: `IntConst(0xFFFFFFFFFFFFFFCE):U64` matches `-50`
///   because the high 32 bits replicate the sign of the 32-bit value.
/// * **Zero-extended narrow form:** `IntConst(0x00000000FFFFFFCE):U64`
///   *also* matches `-50` — this is the common gcc -O2 / x64 shape for
///   `return -50;` (`mov eax, 0xFFFFFFCE; ret` leaves the high 32 bits
///   of RAX zero, so the stored U64 value is `+4294967246`, but the
///   *source-level* meaning is still `-50`).  The
///   `signed_int_const(-50)` pattern matches all three.
///
/// At the build site (rewrite RHS), `signed_int_const(v)` materialises
/// the same `IntConst(v_unsigned)` as `int_const(v as u128)` — masked
/// to the rewrite root's output type.
#[must_use]
pub fn signed_int_const(v: i64) -> Pat<Concrete> {
    let v_signed: i128 = i128::from(v);
    let v_unsigned: u128 = v_signed as u128;
    let exemplar = NodeKind::IntConst(0);
    let mut g: PatGraph<Concrete> = PatGraph::new();
    let post_match: crate::pat_graph::PostMatchFn =
        std::rc::Rc::new(move |ctx, node, _ty, _b| {
            let NodeKind::IntConst(stored) = *ctx.function.node_kind(node) else {
                return false;
            };
            // Match-time output type is the matched node's first
            // value output (not the `_ty` parameter, which would be
            // the consumer's expected input width when a cast walks
            // through).  Mirrors v1 behaviour.
            let Some(out_ty) = ctx
                .function
                .node_outputs(node)
                .iter()
                .find_map(|&out| ctx.function.output_kind(out).as_value())
            else {
                return false;
            };
            let output_width = out_ty.bit_width();
            if output_width == 0 {
                return false;
            }
            let output_mask = out_ty.bit_mask_u128();
            // Iterate widths up to and including the output width.  For
            // each width `w`, treat the stored value as a w-bit signed
            // value (low `w` bits) and check whether that signed value
            // equals `v` AND the high bits above `w` are consistent
            // with either zero- or sign-extension to the output width.
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
                // Above `w` bits, within the output type, the stored
                // value must be either all-zero (zero-extended) or
                // all-one (sign-extended, for negative-at-w values).
                let above_w_mask = output_mask & !w_mask;
                let above = stored & above_w_mask;
                if above == 0 {
                    return true; // zero-extended form of v at width w
                }
                let sign_bit_w = if w >= 128 { 0 } else { 1u128 << (w - 1) };
                if sign_bit_w != 0
                    && (low & sign_bit_w) != 0
                    && above == above_w_mask
                {
                    return true; // sign-extended form
                }
            }
            false
        });
    let n = g.add_node(NodeData {
        kind: KindSpec::Variant(std::mem::discriminant(&exemplar)),
        output_ty: None,
        capture: None,
        post_match: Some(post_match),
        build_spec: Some(BuildSpec {
            kind: BuildKind::Fn(std::rc::Rc::new(move |ctx| {
                let mask = ctx.root_ty.bit_mask_u128();
                Ok(NodeKind::IntConst(v_unsigned & mask))
            })),
            ty: BuildTy::InheritRoot,
        }),
        force_ordered: false,
    });
    g.set_root(n);
    Pat::from_graph(g)
}

// ── Build-time constants from captures ────────────────────────────────

/// Returns the [`NodeOutputType`] of the matched root's first value
/// input, or `None` if the root has no inputs or its first input
/// isn't a value edge.  Exposed for the `*_const_with!` macros via
/// the magic `in_ty` identifier — for `IntCmp(lhs, rhs)` rules where
/// the comparison's input type (needed for signed / carry handling)
/// differs from the root's output type (always `I1`).
#[must_use]
pub fn first_value_input_type(
    ctx: &crate::matcher::BuildCtx<'_>,
) -> Option<NodeOutputType> {
    use strider_ir::node::NodeOutputKind;
    let inputs = ctx.function.node_inputs(ctx.root);
    let inp = inputs.into_iter().next()?;
    match ctx.function.output_kind(inp) {
        NodeOutputKind::OutputType(t) => Some(t),
        _ => None,
    }
}

/// Internal helper: returns a [`Concrete`]-roled `Pat` whose
/// `BuildKind::Fn` materialises the closure's value as an
/// `IntConst(...)` / `FloatConst(...)`-shaped node with the chosen
/// output type.  The match-side `KindSpec` is `Any` and the
/// `post_match` always returns `false`, so accidentally landing one
/// of these patterns on the LHS of a rule silently no-matches rather
/// than causing a panic.
fn build_only_const_pat(
    build_kind: BuildKind,
    build_ty: BuildTy,
) -> Pat<Concrete> {
    let mut g: PatGraph<Concrete> = PatGraph::new();
    // Match-only-false guard: build-only patterns never want to match
    // on the LHS.  Boxing a `Fn` here is fine — the closure captures
    // nothing.
    let never_match: crate::pat_graph::PostMatchFn =
        std::rc::Rc::new(|_ctx, _node, _ty, _b| false);
    let n = g.add_node(NodeData {
        kind: KindSpec::Any,
        output_ty: None,
        capture: None,
        post_match: Some(never_match),
        build_spec: Some(BuildSpec {
            kind: build_kind,
            ty: build_ty,
        }),
        force_ordered: false,
    });
    g.set_root(n);
    Pat::from_graph(g)
}

/// Builds an `IntConst` node whose value is computed by `f` at
/// rewrite-rule build time.  The closure receives the per-rewrite
/// [`BuildCtx`](crate::matcher::BuildCtx) — exposing the captured
/// LHS [`Bindings`](crate::Bindings) plus the matched root's NodeId
/// and resolved output type — and returns a `u128` value.
///
/// Used by the `int_const_with!` macro.  The output type inherits
/// the rewrite root.
#[must_use]
pub fn int_const_with_fn<F>(f: F) -> Pat<Concrete>
where
    F: Fn(&crate::matcher::BuildCtx<'_>) -> anyhow::Result<u128> + 'static,
{
    build_only_const_pat(
        BuildKind::Fn(std::rc::Rc::new(move |ctx| Ok(NodeKind::IntConst(f(ctx)?)))),
        BuildTy::InheritRoot,
    )
}

/// Builds a boolean constant (an `IntConst(b as u128)` typed `I1`)
/// whose value is computed by `f` at rewrite-rule build time.  Used
/// by the `bool_const_with!` macro.
#[must_use]
pub fn bool_const_with_fn<F>(f: F) -> Pat<Concrete>
where
    F: Fn(&crate::matcher::BuildCtx<'_>) -> anyhow::Result<bool> + 'static,
{
    build_only_const_pat(
        BuildKind::Fn(std::rc::Rc::new(move |ctx| {
            Ok(NodeKind::IntConst(u128::from(f(ctx)?)))
        })),
        BuildTy::Fixed(NodeOutputType::I1),
    )
}

/// Builds a `FloatConst` node whose IEEE 754 bit pattern is computed
/// by `f` at rewrite-rule build time.  Used by the
/// `float_const_with!` macro.  The output type inherits the rewrite
/// root.
#[must_use]
pub fn float_const_with_fn<F>(f: F) -> Pat<Concrete>
where
    F: Fn(&crate::matcher::BuildCtx<'_>) -> anyhow::Result<u64> + 'static,
{
    build_only_const_pat(
        BuildKind::Fn(std::rc::Rc::new(move |ctx| Ok(NodeKind::FloatConst(f(ctx)?)))),
        BuildTy::InheritRoot,
    )
}
