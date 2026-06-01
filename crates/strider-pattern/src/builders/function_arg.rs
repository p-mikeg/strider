//! Function-argument-carrier chained builders.
//!
//! After `FunctionArgDetect`, function arguments are represented as
//! `InitialVar` nodes (register args) or `Load` nodes (stack args)
//! recorded in the `Function::arg_index_to_nodes` side-table.  The
//! proven semantics in `strider-analyze` expose three matchers:
//!
//! * `function_arg(idx)` — match the carrier registered at side-table
//!   index `idx`.
//! * `function_arg_reg(vn, idx)` — match the carrier at `idx`, restricted
//!   to register-source (`InitialVar(vn)`).
//! * `function_arg_stack(space, offset, idx)` — match the carrier at
//!   `idx`, restricted to stack-source (`Load` with the given offset
//!   recorded in `Function::stack_offsets`).
//!
//! All three need to read `Function::arg_index_to_nodes` (and
//! `Function::stack_offsets` for the stack variant) at match time.
//! The current `PostMatchFn` stub `Rc<dyn Fn() -> bool>` cannot
//! reach the `MatchCtx`, so the side-table-aware matchers are deferred
//! to a follow-up alongside the closure widening.
//!
//! What lands today: the [`initial_var`] / [`initial_var_for`]
//! factories that match `InitialVar` nodes directly by `NodeKind`
//! discriminant / payload.  These are the underlying carrier for
//! register-passed args, so callers that don't need the
//! arg-index-to-side-table mapping can already pattern-match the
//! relevant nodes through these.
//!
//! Hand-written status: the plan acknowledges `FunctionArgPat` stays
//! hand-written when the full side-table-aware matcher lands (it's an
//! enum-dispatch source whose shape doesn't fit the field-based PatGraph
//! storage).  The PyO3 mirror in `strider-py` therefore won't be
//! macro-emitted from a `*Def` struct — same constraint as the proven
//! semantics today.

use strider_ir::node::NodeKind;

use crate::pat_graph::{
    TemplateKind, TemplateSpec, TemplateTy, Concrete, KindSpec, NodeData, PatGraph, Wildcard,
};

use super::Pat;

/// Match any `InitialVar(_)` node (any varnode).  Wildcard role.
///
/// `InitialVar` is the carrier kind for register-passed function
/// arguments after `FunctionArgDetect`, so this is the closest
/// today-buildable approximation of `function_arg_reg(..)` without
/// the side-table-aware index filter.
#[must_use]
pub fn initial_var() -> Pat<Wildcard> {
    // Use a sentinel varnode for the discriminant exemplar; the
    // `Variant` matcher ignores the payload.
    let sentinel = rsleigh::Vn {
        size: 0,
        addr_off: 0,
        addr_space: rsleigh::VnSpace::REGISTER,
    };
    let exemplar = NodeKind::InitialVar(sentinel);
    let mut g: PatGraph<Wildcard> = PatGraph::new();
    let n = g.add_node(NodeData {
        kind: KindSpec::Variant(std::mem::discriminant(&exemplar)),
        output_ty: None,
        capture: None,
        post_match: None,
        template_spec: None,
    
        force_ordered: false,
    });
    g.set_root(n);
    Pat::from_graph(g)
}

/// Match `InitialVar(vn)` for the exact varnode `vn`.  Concrete role:
/// the kind payload pins the build path, so the builder can also be
/// used as a rewrite RHS.
#[must_use]
pub fn initial_var_for(vn: rsleigh::Vn) -> Pat<Concrete> {
    let kind = NodeKind::InitialVar(vn);
    let mut g: PatGraph<Concrete> = PatGraph::new();
    let n = g.add_node(NodeData {
        kind: KindSpec::Exact(kind),
        output_ty: None,
        capture: None,
        post_match: None,
        template_spec: Some(TemplateSpec {
            kind: TemplateKind::Exact(kind),
            ty: TemplateTy::InheritRoot,
        }),
    
        force_ordered: false,
    });
    g.set_root(n);
    Pat::from_graph(g)
}

// ── FunctionArgPat ───────────────────────────────────────────────────────────

use strider_ir::node::FunctionArgSource;

use crate::pat_graph::PostMatchFn;

/// Match a function-argument carrier registered in
/// `Function::arg_index_to_nodes`.
///
/// Hand-written builder (the enum-dispatch source — kind-`Any` plus
/// post_match guard — doesn't fit the field-based PatGraph storage
/// the rest of the crate uses).  All filters operate via the
/// post_match closure; the kind-spec is `Any` since register-passed
/// args are `InitialVar` and stack-passed args are `Load`.
pub struct FunctionArgPat {
    source: Option<FunctionArgSource>,
    index: Option<u32>,
}

impl FunctionArgPat {
    fn new() -> Self {
        Self { source: None, index: None }
    }

    /// Restrict the match to a specific ABI source.
    #[must_use]
    pub fn source(mut self, s: FunctionArgSource) -> Self {
        self.source = Some(s);
        self
    }

    /// Restrict the match to a specific argument index.
    #[must_use]
    pub fn index(mut self, i: u32) -> Self {
        self.index = Some(i);
        self
    }
}

impl From<FunctionArgPat> for Pat<Wildcard> {
    fn from(b: FunctionArgPat) -> Pat<Wildcard> {
        let FunctionArgPat { source, index } = b;
        let mut g: PatGraph<Wildcard> = PatGraph::new();
        let post_match: PostMatchFn = std::rc::Rc::new(move |ctx, node, _ty, _b| {
            // Index constraint.
            match index {
                Some(idx) => {
                    if !ctx.function.arg_index_to_nodes(idx).contains(&node) {
                        return false;
                    }
                }
                None => {
                    let any = ctx.function.iter_arg_indices().any(|i| {
                        ctx.function.arg_index_to_nodes(i).contains(&node)
                    });
                    if !any {
                        return false;
                    }
                }
            }
            // Source constraint.
            let Some(expected) = source else {
                return true;
            };
            match (expected, ctx.function.node_kind(node)) {
                (FunctionArgSource::Register(want), NodeKind::InitialVar(actual)) => {
                    want == *actual
                }
                (FunctionArgSource::Stack { .. }, NodeKind::Load(_)) => true,
                _ => false,
            }
        });
        let n = g.add_node(NodeData {
            kind: KindSpec::Any,
            output_ty: None,
            capture: None,
            post_match: Some(post_match),
            template_spec: None,
            force_ordered: false,
        });
        g.set_root(n);
        Pat::from_graph(g)
    }
}

/// Match the carrier registered at side-table index `idx`.  No source
/// filter — accepts both register-passed (`InitialVar`) and
/// stack-passed (`Load`) carriers.
#[must_use]
pub fn function_arg(idx: u32) -> FunctionArgPat {
    FunctionArgPat::new().index(idx)
}

/// Match any carrier registered in the side-table, regardless of index
/// or source.  Used by passes that want to enumerate every
/// function-arg carrier in a function.
#[must_use]
pub fn function_arg_any() -> FunctionArgPat {
    FunctionArgPat::new()
}

/// Match the carrier at side-table index `idx`, restricted to a
/// register-passed `InitialVar(vn)`.
#[must_use]
pub fn function_arg_reg(vn: rsleigh::Vn, idx: u32) -> FunctionArgPat {
    FunctionArgPat::new()
        .index(idx)
        .source(FunctionArgSource::Register(vn))
}

/// Match the carrier at side-table index `idx`, restricted to a
/// stack-passed `Load` at `(space, offset)`.
#[must_use]
pub fn function_arg_stack(space: rsleigh::VnSpace, offset: i64, idx: u32) -> FunctionArgPat {
    FunctionArgPat::new()
        .index(idx)
        .source(FunctionArgSource::Stack { space, offset })
}
