//! Memory / phi / function-entry / call / return / branch constructors.
//!
//! These are the "structural" pattern ctors — all of them are thin
//! forwards to the builder types in [`super::super::builders`].  Grouped
//! in one file rather than scattered across tiny per-family files
//! (memory.rs, control.rs) because each ctor is a one-liner; the file as
//! a whole is still under 120 lines.

use ir::node::{FunctionArgSource, NodeKind};

use crate::pat::node_pat::{InputsSpec, KindSpec, NodePat, exemplar_vn};
use crate::pat::{
    CallOtherPat, CallPat, FunctionArgPat, IfPat, LoadPat, Pat, PhiPat, RetPat, StackStorePat,
    StackStorePhiPat, StorePat,
};

// ── Memory ops ────────────────────────────────────────────────────────────────

/// Starts building a `Load` pattern.  Chain `.addr()` / `.space()` to add
/// constraints.
#[must_use]
pub fn load() -> LoadPat { LoadPat::new() }
/// Starts building a `Store` pattern.  Chain `.addr()` / `.data()` / `.space()`
/// to add constraints.
#[must_use]
pub fn store() -> StorePat { StorePat::new() }
/// Starts building a `StackStore` pattern.  Chain `.offset()` / `.data()` /
/// `.space()` to add constraints.
#[must_use]
pub fn stack_store() -> StackStorePat { StackStorePat::new() }
/// Starts building a `StackStorePhi` pattern.  Chain `.offsets(…)` /
/// `.data()` / `.space()` to add constraints.
#[must_use]
pub fn stack_store_phi() -> StackStorePhiPat { StackStorePhiPat::new() }


// ── Phi nodes ─────────────────────────────────────────────────────────────────

/// Starts building a `ControlPhi` pattern.  Matches any phi node.
#[must_use]
pub fn phi() -> PhiPat {
    PhiPat::new()
}
/// Starts building a `ControlPhi` pattern pinned to varnode `vn`.
#[must_use]
pub fn phi_for(vn: rsleigh::Vn) -> PhiPat {
    PhiPat::new().for_vn(vn)
}

// ── Entry values ──────────────────────────────────────────────────────────────

/// Matches any `InitialVar` node (function-entry value of any varnode).
#[must_use]
pub fn initial_var() -> Pat {
    initial_var_impl(None)
}
/// Matches the `InitialVar` node for the specific varnode `vn`.
#[must_use]
pub fn initial_var_for(vn: rsleigh::Vn) -> Pat {
    initial_var_impl(Some(vn))
}

fn initial_var_impl(vn: Option<rsleigh::Vn>) -> Pat {
    let kind = match vn {
        None => KindSpec::variant(&NodeKind::InitialVar(exemplar_vn())),
        Some(expected) => KindSpec::Exact(NodeKind::InitialVar(expected)),
    };
    NodePat::matcher(kind, InputsSpec::None).into_pat()
}

// ── Function-argument constructors ────────────────────────────────────────────

/// Matches a `FunctionArg` node for argument index `i` regardless of source
/// (register or stack).
#[must_use]
pub fn function_arg(i: u32) -> FunctionArgPat {
    FunctionArgPat::new().index(i)
}

/// Matches any `FunctionArg` node regardless of index or source.
#[must_use]
pub fn function_arg_any() -> FunctionArgPat {
    FunctionArgPat::new()
}

/// Matches a `FunctionArg` node whose source is the register varnode `vn`.
#[must_use]
pub fn function_arg_reg(vn: rsleigh::Vn) -> FunctionArgPat {
    FunctionArgPat::new().source(FunctionArgSource::Register(vn))
}

/// Matches a `FunctionArg` node whose source is a stack slot at SP-relative
/// `offset` bytes (in address space `space`).
#[must_use]
pub fn function_arg_stack(space: rsleigh::VnSpace, offset: i64) -> FunctionArgPat {
    FunctionArgPat::new().source(FunctionArgSource::Stack { space, offset })
}

// ── Control nodes ─────────────────────────────────────────────────────────────

/// Starts building a `Call` pattern.  Chain `.at()`, `.arg()`, `.target()` to
/// add constraints.
#[must_use]
pub fn call() -> CallPat {
    CallPat::new()
}
/// Starts building a `CallOther` (user-defined op) pattern.  Chain
/// `.user_op_id()`, `.arg()` to add constraints.  Use
/// [`crate::pat::IntoPat::capture`] to bind the matched node id.
#[must_use]
pub fn call_other() -> CallOtherPat {
    CallOtherPat::new()
}
/// Starts building a `Return` pattern.  Chain `.preceded_by()` / `.ret_val()`
/// to add constraints.
#[must_use]
pub fn ret() -> RetPat {
    RetPat::new()
}
/// Starts building an `If` pattern.  Chain `.cond()`, `.true_branch()`,
/// `.false_branch()` to add constraints.
#[must_use]
pub fn if_node() -> IfPat {
    IfPat::new()
}
