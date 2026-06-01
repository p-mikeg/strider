//! Wildcard / capture pattern constructors.
//!
//! Ported from `strider-analyze::pattern::pat::ctor::wildcards`.  The
//! storage shape is different (one-node `PatGraph<R>` instead of a
//! `NodePat`) but the semantics — `any` accepts every node kind, `var`
//! additionally binds a capture — are identical.

use crate::capture::Capture;
use crate::pat_graph::{Concrete, KindSpec, NodeData, PatGraph, Wildcard};

use super::Pat;

/// Match any node.  Wildcard role (not buildable).
#[must_use]
pub fn any() -> Pat<Wildcard> {
    let mut g: PatGraph<Wildcard> = PatGraph::new();
    let n = g.add_node(NodeData {
        kind: KindSpec::Any,
        output_ty: None,
        capture: None,
        post_match: None,
        build_spec: None,
    
        force_ordered: false,
    });
    g.set_root(n);
    Pat::from_graph(g)
}

/// Match any node and bind its output to `c`.  Concrete role: the
/// capture provides a build path (resolve through `Bindings` at
/// template time).
#[must_use]
pub fn var(c: Capture) -> Pat<Concrete> {
    let mut g: PatGraph<Concrete> = PatGraph::new();
    let n = g.add_node(NodeData {
        kind: KindSpec::Any,
        output_ty: None,
        capture: Some(c.as_ref()),
        post_match: None,
        build_spec: None,
    
        force_ordered: false,
    });
    g.set_root(n);
    Pat::from_graph(g)
}

/// Match any node for which `f` returns `true`.  Equivalent to
/// `any().when_match(move |ctx, ty, _b| f(ctx, ty))` but spelled as
/// a single free function for the simple "predicate on the matched
/// output's type / function context" case.
///
/// Always returns a `Pat<Wildcard>` because a custom predicate has no
/// template counterpart.
#[must_use]
pub fn predicate<F>(f: F) -> Pat<Wildcard>
where
    F: Fn(&crate::MatchCtx, strider_ir::node::NodeOutputType) -> bool + 'static,
{
    any().when_match(move |ctx, ty, _b| f(ctx, ty))
}

/// Matches any value output that is exactly `n` bits wide.
///
/// The width-limit mechanism for querying by output type: `value_of_width(1)`
/// (a.k.a. [`bool_value`]) selects booleans (the 1-bit `I1`); `value_of_width(32)`
/// matches any `I32`- or `F32`-typed value, etc.
///
/// Strictly requires a value output: non-value outputs (Control / Memory /
/// PhiToken) and zero-output nodes never match — bypasses the I1 placeholder
/// that the post_match hook uses for zero-output kinds like `Return`.
#[must_use]
#[allow(clippy::expect_used)]
pub fn value_of_width(n: u32) -> Pat<Wildcard> {
    let want = n as usize;
    let mut g: crate::pat_graph::PatGraph<Wildcard> = crate::pat_graph::PatGraph::new();
    let post_match: crate::pat_graph::PostMatchFn = std::rc::Rc::new(move |ctx, node, _ty, _b| {
        // Find this node's first value output and check its width; reject
        // if the node has no value output (non-value-producing kinds).
        ctx.function
            .node_outputs(node)
            .iter()
            .find_map(|&out| ctx.function.output_kind(out).as_value())
            .is_some_and(|ty| ty.bit_width() == want)
    });
    let root = g.add_node(crate::pat_graph::NodeData {
        kind: crate::pat_graph::KindSpec::Any,
        output_ty: None,
        capture: None,
        post_match: Some(post_match),
        build_spec: None,
        force_ordered: false,
    });
    g.set_root(root);
    Pat::from_graph(g)
}

/// Matches any boolean value — i.e. any value output 1 bit wide (`I1`).
/// Sugar for [`value_of_width`]`(1)`.
#[must_use]
pub fn bool_value() -> Pat<Wildcard> {
    value_of_width(1)
}

/// Matches `inner` **and** requires all of the matched node's value
/// inputs to be `n` bits wide.  The input-side width filter:
/// `inputs_of_width(1, …)` (a.k.a. [`bool_inputs`]) selects operations
/// that *operate on* booleans (`And`/`Or`/`Xor` on `I1` — a logical NOT
/// is `Xor(_, IntConst(1))` at `I1` since the former BitNot unary-op
/// was removed) and excludes comparisons (whose operands are wider
/// even though they produce `I1`).
///
/// The `inner` pattern is the wrapping shape — typically a `var(c)` or
/// an operation builder; the input-width check runs after `inner`'s
/// own match via the `post_match` hook on the root node.
///
/// Reaches into the underlying [`PatGraph`] to install a [`PostMatchFn`]
/// that has direct `NodeId` access, since the user-facing
/// `Pat::when_match` doesn't expose the matched node id.
#[must_use]
#[allow(clippy::expect_used)]
pub fn inputs_of_width<R: crate::pat_graph::Role>(
    n: u32,
    inner: Pat<R>,
) -> Pat<crate::pat_graph::Wildcard> {
    let mut g = inner.0.into_wildcard();
    let root = g.root().expect("Pat has no root");
    let want = n as usize;
    let nd = g
        .inner
        .node_weight_mut(root)
        .expect("root index invalid");
    let new_fn: crate::pat_graph::PostMatchFn = if let Some(prev) = nd.post_match.take() {
        std::rc::Rc::new(move |ctx, node, ty, b| {
            if !prev(ctx, node, ty, b) {
                return false;
            }
            inputs_of_width_check(ctx, node, want)
        })
    } else {
        std::rc::Rc::new(move |ctx, node, _ty, _b| inputs_of_width_check(ctx, node, want))
    };
    nd.post_match = Some(new_fn);
    Pat::from_graph(g)
}

/// Check that every value input of `node` has width `want`, and that the
/// node has at least one value input.  Non-value inputs (control / memory)
/// are ignored.  Mirrors the strider-analyze `InputWidthPat::try_match`
/// semantics — including the v1 invariant that the matched node has at
/// least one value output (so zero-output kinds like `Return` never
/// qualify even when their ret-val inputs are width-matched).
fn inputs_of_width_check(
    ctx: &crate::MatchCtx,
    node: strider_ir::node::NodeId,
    want: usize,
) -> bool {
    let f = ctx.function;
    // Reject zero-value-output kinds (Return, IndirectBranch, …) — v1
    // never dispatched `InputWidthPat::try_match` against them because the
    // pattern signature took a `NodeOutputId`.
    let has_value_output = f
        .node_outputs(node)
        .iter()
        .any(|&out| f.output_kind(out).as_value().is_some());
    if !has_value_output {
        return false;
    }
    let mut value_inputs = 0usize;
    for inp in f.node_inputs(node) {
        if let Some(ty) = f.output_kind(inp).as_value() {
            value_inputs += 1;
            if ty.bit_width() != want {
                return false;
            }
        }
    }
    value_inputs > 0
}

/// Matches `inner` whose value inputs are all booleans (1-bit `I1`).
/// Sugar for [`inputs_of_width`]`(1, inner)`.
#[must_use]
pub fn bool_inputs<R: crate::pat_graph::Role>(
    inner: Pat<R>,
) -> Pat<crate::pat_graph::Wildcard> {
    inputs_of_width(1, inner)
}
