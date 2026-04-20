//! Phi / function-entry / call / return / branch / region-search constructors.

use crate::pat::{CallOtherPat, CallPat, IfPat, Pat, PatKind, PhiPat, RetPat};

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
    Pat::new(PatKind::InitialVar { vn: None })
}
/// Matches the `InitialVar` node for the specific varnode `vn`.
pub fn initial_var_for(vn: rsleigh::Vn) -> Pat {
    Pat::new(PatKind::InitialVar { vn: Some(vn) })
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
