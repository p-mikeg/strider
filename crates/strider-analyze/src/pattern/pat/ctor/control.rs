//! Memory / phi / function-entry / call / return / branch constructors.
//!
//! These are the "structural" pattern ctors — all of them are thin
//! forwards to the builder types in [`super::super::builders`].  Grouped
//! in one file rather than scattered across tiny per-family files
//! (memory.rs, control.rs) because each ctor is a one-liner; the file as
//! a whole is still under 120 lines.

use strider_ir::node::{FunctionArgSource, NodeKind};

use crate::pattern::pat::node_pat::{InputsSpec, KindSpec, NodePat, exemplar_vn};
use crate::pattern::pat::{
    CallOtherPat, CallPat, FunctionArgPat, IfPat, LoadPat, MemPhiPat, Pat, PhiPat, RetPat,
    StorePat, ValuePhiPat,
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
// ── Phi nodes ─────────────────────────────────────────────────────────────────

/// Starts building a tagged-`Phi` pattern.  Matches `Phi` nodes whose
/// optional source-varnode tag (in `Graph::phi_var_tag`) is `Some` —
/// the lifter-emitted SSA φ for a register-aliased read.
///
/// For other phi kinds use [`mem_phi`] (memory-token phi) or
/// [`value_phi`] (anonymous value phi, `phi_var_tag` is `None`, e.g.
/// the one `LoadForward` synthesises).
#[must_use]
pub fn phi() -> PhiPat {
    PhiPat::new()
}

/// Starts building a `MemPhi` pattern.  Matches the memory-token phi
/// at control-flow join points.
#[must_use]
pub fn mem_phi() -> MemPhiPat {
    MemPhiPat::new()
}

/// Starts building a `ValuePhi` pattern.  Matches the value phi
/// `LoadForward` synthesises when forwarding stack-store values
/// across a control-flow join.
#[must_use]
pub fn value_phi() -> ValuePhiPat {
    ValuePhiPat::new()
}
/// Starts building a tagged-`Phi` pattern (see [`phi`]) pinned to
/// varnode `vn` in `Graph::phi_var_tag`.
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
/// [`crate::pattern::pat::IntoPat::capture`] to bind the matched node id.
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
///
/// **Direct layout only.**  By the time the matcher runs, every `If` node
/// in the graph is in canonical direct layout — the `opt::IfCondInversion`
/// pass eagerly rewrites `If(BitNot(C)){A}{B}` into `If(C){B}{A}` (and
/// `ConstantFold` collapses double negations first).  Patterns are
/// matched against the canonical direct layout only; write the pattern
/// from the source-level POV (non-negated condition).
#[must_use]
pub fn if_node() -> IfPat {
    IfPat::new()
}
