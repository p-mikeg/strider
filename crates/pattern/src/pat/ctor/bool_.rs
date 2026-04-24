//! Boolean binary and unary pattern constructors.
//!
//! This module is named `bool_` (trailing underscore) because `bool` is a Rust
//! primitive type and reusing it as a module name requires `mod r#bool;` /
//! `r#bool::…` at every call site, which is uglier than the suffix.

use std::sync::Arc;

use ir::node::NodeKind;
use ir::{BoolBinaryOp, BoolUnaryOp};

use crate::macros::{decl_pat_binary_ops, decl_pat_unary_ops};
use crate::pat::BoolBinaryOpPat;
use crate::pat::Pat;
use crate::pat::node_pat::{BuildTy, InputsSpec, KindFilter, NodePat};

/// Matches a boolean binary operation with the given `op`.
///
/// Commutative ops (`And`, `Or`, `Xor`) try both orderings automatically.
pub fn bool_binary(op: BoolBinaryOp, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> BoolBinaryOpPat {
    BoolBinaryOpPat::new(op, lhs.into(), rhs.into())
}

decl_pat_binary_ops!(bool_binary, BoolBinaryOp, BoolBinaryOpPat, [
    /// Matches a boolean AND node.  Commutative.
    (bool_and, And),
    /// Matches a boolean OR node.  Commutative.
    (bool_or, Or),
    /// Matches a boolean XOR node.  Commutative.
    (bool_xor, Xor),
]);

/// Matches a boolean unary operation with the given `op`.
pub fn bool_unary(op: BoolUnaryOp, operand: impl Into<Pat>) -> Pat {
    NodePat::matcher(
        KindFilter::exact(&NodeKind::BoolUnaryOp(op)),
        Arc::new(move |ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::BoolUnaryOp(x) if *x == op)
        }),
        InputsSpec::fixed_ordered(vec![operand.into()]),
    )
    .with_build(Arc::new(move |_b| Ok(NodeKind::BoolUnaryOp(op))))
    .with_build_ty(BuildTy::Fixed(ir::node::NodeOutputType::Bool))
    .into_pat()
}

decl_pat_unary_ops!(bool_unary, BoolUnaryOp, Pat, [
    /// Matches a boolean NOT node.
    (bool_not, Neg),
]);
