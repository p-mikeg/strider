//! Boolean binary and unary pattern constructors.
//!
//! This module is named `bool_` (trailing underscore) because `bool` is a Rust
//! primitive type and reusing it as a module name requires `mod r#bool;` /
//! `r#bool::…` at every call site, which is uglier than the suffix.
//!
//! Booleans are 1-bit integers (`I1`) in this IR: there is no separate
//! `BoolBinaryOp` / `BoolUnaryOp` node kind.  A boolean AND / OR / XOR is an
//! [`IntBinaryOp`](strider_ir::IntBinaryOp) (`And` / `Or` / `Xor`) whose output
//! is `I1`, and a logical NOT is an [`IntUnaryOp`](strider_ir::IntUnaryOp)
//! `BitNot` at `I1` (`~0 & 1 == 1`, `~1 & 1 == 0`).  These constructors keep
//! their historical `bool_*` names but build/match the integer shapes at `I1`.

use std::sync::Arc;

use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir::{IntBinaryOp, IntUnaryOp};

use crate::pattern::pat::node_pat::{BuildTy, InputsSpec, KindSpec, PostMatchFn, NodePat};
use crate::pattern::pat::{BoolBinaryOpPat, Pat};

/// Post-match guard restricting a match to a node whose value output is the
/// 1-bit boolean `I1`.  Without it the `bool_*` matchers would also accept a
/// same-shaped wide integer op (e.g. a 64-bit `And`), since after the bool→I1
/// collapse a boolean op and a wide integer op share the same `NodeKind`.
///
/// (The binary-op constructors carry an equivalent guard inside
/// [`BoolBinaryOpPat`]; this copy serves the unary constructors.)
fn require_i1_output() -> PostMatchFn {
    Arc::new(|ctx, node, _bindings| {
        ctx.function
            .node_outputs(node)
            .iter()
            .find_map(|&out| ctx.function.output_kind(out).as_value())
            .is_some_and(|ty| ty.bit_width() == 1)
    })
}

/// Matches a boolean binary operation with the given `op`.
///
/// `op` is an [`IntBinaryOp`] (booleans are `I1` integers); use `And`, `Or`,
/// or `Xor`.  Commutative ops try both orderings automatically; call
/// `.ordered()` on the returned `BoolBinaryOpPat` to disable this.  The
/// result node is typed `I1`.
pub fn bool_binary(op: IntBinaryOp, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> BoolBinaryOpPat {
    BoolBinaryOpPat::new(op, lhs.into(), rhs.into())
}

/// Matches a boolean AND node (`IntBinaryOp::And` at `I1`).  Commutative;
/// call `.ordered()` to pin operand order.
pub fn bool_and(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> BoolBinaryOpPat {
    BoolBinaryOpPat::new(IntBinaryOp::And, lhs.into(), rhs.into())
}

/// Matches a boolean OR node (`IntBinaryOp::Or` at `I1`).  Commutative;
/// call `.ordered()` to pin operand order.
pub fn bool_or(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> BoolBinaryOpPat {
    BoolBinaryOpPat::new(IntBinaryOp::Or, lhs.into(), rhs.into())
}

/// Matches a boolean XOR node (`IntBinaryOp::Xor` at `I1`).  Commutative;
/// call `.ordered()` to pin operand order.
pub fn bool_xor(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> BoolBinaryOpPat {
    BoolBinaryOpPat::new(IntBinaryOp::Xor, lhs.into(), rhs.into())
}

/// Matches a boolean unary operation with the given `op` (an [`IntUnaryOp`]
/// at `I1`).
pub fn bool_unary(op: IntUnaryOp, operand: impl Into<Pat>) -> Pat {
    let kind = NodeKind::IntUnaryOp(op);
    NodePat::matcher(KindSpec::Exact(kind), InputsSpec::fixed_ordered(vec![operand.into()]))
        .with_post_match(require_i1_output())
        .with_build_exact(kind, BuildTy::Fixed(NodeOutputType::I1))
        .into_pat()
}

/// Matches a boolean NOT node (`IntUnaryOp::BitNot` at `I1`, i.e. a 1-bit
/// complement — a logical NOT).
pub fn bool_not(operand: impl Into<Pat>) -> Pat {
    bool_unary(IntUnaryOp::BitNot, operand)
}
