//! Wildcard, capture, and constant-literal pattern constructors.

use std::sync::Arc;

use ir::BuiltFunctionGraph;
use ir::node::{NodeKind, NodeOutputId, NodeOutputType};

use crate::pat::any::{AnyPat, VarPat};
use crate::pat::node_pat::{BuildTy, InputsSpec, KindSpec, NodePat};
use crate::pat::{IntoPat, Pat};
use crate::var::Capture;

/// Matches any single output unconditionally.
#[must_use]
pub fn any() -> Pat {
    Pat::from_dyn(Arc::new(AnyPat))
}

/// Matches any output and binds it to `c`.
///
/// If `c` is already bound the output must equal the stored binding.
/// Equivalent in behavior to `any().capture(c)`, but constructs a dedicated
/// [`VarPat`] rather than wrapping [`AnyPat`] in a [`CapturePat`] — one
/// fewer vtable hop and no backtracking snapshot per match.
#[must_use]
pub fn var(c: Capture) -> Pat {
    Pat::from_dyn(Arc::new(VarPat { capture: c }))
}

/// Matches an `IntConst` node whose stored value, when masked to the node's
/// declared width, equals `v`'s representation at that same width.
///
/// `v` accepts any signed or unsigned integer literal (`i32`, `i64`, `i128`,
/// `u32`, `u64`).  Negative values are sign-extended to the IntConst's width
/// before comparison, so `int_const(-50)` matches both
/// `IntConst(0xffff_ffce, U32)` and `IntConst(0xffff_ffff_ffff_ffce, U64)` —
/// no per-arch default needed.
///
/// Values larger than `i128::MAX` (e.g. `u128::MAX` as raw bits) require
/// passing an `i128` constructed via `as i128` — the `Into<i128>` conversion
/// reinterprets the high bit as sign.
///
/// In build position (RHS of a rewrite rule), constructs an `IntConst(v
/// masked to the root's output type)` node.
#[must_use]
pub fn int_const(v: impl Into<i128>) -> Pat {
    let v_signed: i128 = v.into();
    let v_unsigned: u128 = v_signed as u128;
    // Discriminant-only prefilter; the width-aware equality is done in
    // post_match where we have the output type.
    NodePat::matcher(
        KindSpec::variant(&NodeKind::IntConst(0u128)),
        InputsSpec::None,
    )
    .with_post_match(Arc::new(move |ctx, node, _b| {
        let NodeKind::IntConst(stored) = *ctx.graph.graph.node_kind(node) else {
            return false;
        };
        // Determine the output type from the node's single value output.
        let ty = ctx
            .graph
            .graph
            .node_outputs(node)
            .into_iter()
            .find_map(|out| ctx.graph.graph.output_kind(out).as_value());
        let Some(ty) = ty else { return false; };
        let mask = ty.bit_mask_u128();
        (stored & mask) == (v_unsigned & mask)
    }))
    .with_build_fn(
        Arc::new(move |ctx| {
            let mask = ctx.root_ty.bit_mask_u128();
            Ok(NodeKind::IntConst(v_unsigned & mask))
        }),
        BuildTy::InheritRoot,
    )
    .into_pat()
}

/// Matches a `BoolConst` node with value exactly `v`.
#[must_use]
pub fn bool_const(v: bool) -> Pat {
    NodePat::matcher(KindSpec::Exact(NodeKind::BoolConst(v)), InputsSpec::None)
        .with_build_exact(NodeKind::BoolConst(v), BuildTy::Fixed(NodeOutputType::Bool))
        .into_pat()
}

/// Matches a `FloatConst` node with the exact bit pattern `bits`.
#[must_use]
pub fn float_const(bits: u64) -> Pat {
    NodePat::matcher(KindSpec::Exact(NodeKind::FloatConst(bits)), InputsSpec::None)
        .with_build_exact(NodeKind::FloatConst(bits), BuildTy::InheritRoot)
        .into_pat()
}

/// Matches any output for which `f` returns `true`.  Equivalent to
/// `any().when(f)`.
pub fn predicate<F>(f: F) -> Pat
where
    F: Fn(&BuiltFunctionGraph, NodeOutputType, NodeOutputId) -> bool + Send + Sync + 'static,
{
    any().when(f)
}
