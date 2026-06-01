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
//! The current `PostMatchFn` stub `Box<dyn Fn() -> bool>` cannot
//! reach the `MatchCtx`, so the side-table-aware matchers are deferred
//! to a follow-up alongside the closure widening (Task 11).
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
    BuildKind, BuildSpec, BuildTy, Concrete, KindSpec, NodeData, PatGraph, Wildcard,
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
        build_spec: None,
    
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
        build_spec: Some(BuildSpec {
            kind: BuildKind::Exact(kind),
            ty: BuildTy::InheritRoot,
        }),
    
        force_ordered: false,
    });
    g.set_root(n);
    Pat::from_graph(g)
}

// `function_arg(idx)` / `function_arg_reg(vn, idx)` /
// `function_arg_stack(space, offset, idx)` are deferred — they need
// `Function::arg_index_to_nodes` (and for the stack variant
// `Function::stack_offsets`) at match time, which is reachable only
// from a `MatchCtx`-aware `post_match` closure.  The current
// `PostMatchFn` stub `Box<dyn Fn() -> bool>` can't reach those
// side-tables; once the closure signature widens (Task 11), the
// full set of factories will land here as a hand-written `Pattern`
// impl (enum-dispatch source, per the plan).
