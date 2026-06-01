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
/// In build position (RHS of a rewrite rule), constructs an
/// `IntConst(v)` whose output type inherits the rewrite root.
#[must_use]
pub fn int_const(v: u128) -> Pat<Concrete> {
    let mut g: PatGraph<Concrete> = PatGraph::new();
    let n = g.add_node(NodeData {
        kind: KindSpec::Exact(NodeKind::IntConst(v)),
        output_ty: None,
        capture: None,
        post_match: None,
        build_spec: Some(BuildSpec {
            kind: BuildKind::Exact(NodeKind::IntConst(v)),
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
    let check: Box<dyn Fn(&NodeKind) -> bool> = Box::new(move |k: &NodeKind| -> bool {
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

/// Match a signed integer constant `v`.  Stored as `i64` and cast to
/// `u128` via two's complement; equivalent to `int_const(v as u128)`
/// at the call site but reads more naturally for negative literals
/// (e.g. `signed_int_const(-1)` for `x - 1` lowered to `Add(x,
/// IntConst(...))`).
#[must_use]
pub fn signed_int_const(v: i64) -> Pat<Concrete> {
    let u: u128 = (i128::from(v)) as u128;
    int_const(u)
}
