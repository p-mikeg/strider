//! Phi / function-entry / call / return / branch / region-search constructors.

use std::sync::Arc;

use ir::node::{FunctionArgSource, NodeKind};

use crate::pat::node_pat::{InputsSpec, NodePat};
use crate::pat::{CallOtherPat, CallPat, FunctionArgPat, IfPat, Pat, PatKind, PhiPat, RetPat};

// ── Phi nodes ─────────────────────────────────────────────────────────────────

/// Starts building a `ControlPhi` pattern.  Matches any phi node.
pub fn phi() -> PhiPat {
    PhiPat::new()
}
/// Starts building a `ControlPhi` pattern pinned to varnode `vn`.
pub fn phi_for(vn: rsleigh::Vn) -> PhiPat {
    PhiPat::new().for_vn(vn)
}

// ── Entry values ──────────────────────────────────────────────────────────────

/// Matches any `InitialVar` node (function-entry value of any varnode).
pub fn initial_var() -> Pat {
    initial_var_impl(None)
}
/// Matches the `InitialVar` node for the specific varnode `vn`.
pub fn initial_var_for(vn: rsleigh::Vn) -> Pat {
    initial_var_impl(Some(vn))
}

fn initial_var_impl(vn: Option<rsleigh::Vn>) -> Pat {
    Pat::from_dyn(Arc::new(NodePat {
        kind_match: Arc::new(move |ctx, node, _b| {
            let NodeKind::InitialVar(actual_vn) = ctx.graph.graph.node_kind(node) else {
                return false;
            };
            if let Some(expected) = vn
                && *actual_vn != expected
            {
                return false;
            }
            true
        }),
        inputs: InputsSpec::None,
        post_match: None,
        output_var: None,
        node_var: None,
    }))
}

// ── Function-argument constructors ────────────────────────────────────────────

/// Matches a `FunctionArg` node for argument index `i` regardless of source
/// (register or stack).
pub fn function_arg(i: u32) -> FunctionArgPat {
    FunctionArgPat::new().index(i)
}

/// Matches any `FunctionArg` node regardless of index or source.
pub fn function_arg_any() -> FunctionArgPat {
    FunctionArgPat::new()
}

/// Matches a `FunctionArg` node whose source is the register varnode `vn`.
pub fn function_arg_reg(vn: rsleigh::Vn) -> FunctionArgPat {
    FunctionArgPat::new().source(FunctionArgSource::Register(vn))
}

/// Matches a `FunctionArg` node whose source is a stack slot at SP-relative
/// `offset` bytes (in address space `space`).
pub fn function_arg_stack(space: rsleigh::VnSpace, offset: i64) -> FunctionArgPat {
    FunctionArgPat::new().source(FunctionArgSource::Stack { space, offset })
}

// ── Control nodes ─────────────────────────────────────────────────────────────

/// Starts building a `Call` pattern.  Chain `.at()`, `.arg()`, `.target()` to
/// add constraints.
pub fn call() -> CallPat {
    CallPat::new()
}
/// Starts building a `CallOther` (user-defined op) pattern.  Chain
/// `.user_op_id()`, `.arg()`, `.capture()` to add constraints.
pub fn call_other() -> CallOtherPat {
    CallOtherPat::new()
}
/// Starts building a `Return` pattern.  Chain `.preceded_by()` / `.ret_val()`
/// to add constraints.
pub fn ret() -> RetPat {
    RetPat::new()
}
/// Starts building an `If` pattern.  Chain `.cond()`, `.true_branch()`,
/// `.false_branch()` to add constraints.
pub fn if_node() -> IfPat {
    IfPat::new()
}

// ── Region search ─────────────────────────────────────────────────────────────

/// Matches any node reachable via a forward control-chain walk from the current
/// node that satisfies `p`.
///
/// Transparent to `ControlState`, `IfCase`, and `Call` nodes; stops at `If`
/// and `Return` terminators.
pub fn contains(p: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::Contains(p.into()))
}
