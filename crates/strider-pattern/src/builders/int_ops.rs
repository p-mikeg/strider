//! Integer binary-op chained builders.
//!
//! Each builder takes two child `Pat<R>`s with possibly different roles,
//! merges them into a parent `PatGraph<<R1 as Combine<R2>>::Output>`,
//! and returns the combined-role `Pat`.  Role propagation follows the
//! `Combine` trait — a wildcard sub-pattern infects the parent's role;
//! concrete-only sub-patterns produce a concrete parent.
//!
//! Commutative ops (`Add`, `Mul`, `And`, `Or`, `Xor`) work out of the
//! box: the matcher's commutative-retry pass swaps operand orderings on
//! the IR side, so this layer only has to record the correct
//! `IntBinaryOp` variant on the parent pat node.

use strider_ir::node::NodeKind;
use strider_ir::IntBinaryOp;

use crate::pat_graph::{
    BuildKind, BuildSpec, BuildTy, Combine, EdgeData, KindSpec, NodeData, PatGraph, Role,
    merge_subgraph,
};

use super::Pat;
use super::unary_ops::neg;

/// Build a two-input `IntBinaryOp(op)` parent pattern around `lhs` /
/// `rhs`.  Role propagation goes through `Combine`, so the parent's
/// role is the weaker of the two children's roles.
fn binary_op_pat<R1, R2>(
    op: IntBinaryOp,
    lhs: Pat<R1>,
    rhs: Pat<R2>,
) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    let kind = NodeKind::IntBinaryOp(op);
    let mut parent: PatGraph<<R1 as Combine<R2>>::Output> = PatGraph::new();
    let lhs_root = merge_subgraph(&mut parent, lhs.0);
    let rhs_root = merge_subgraph(&mut parent, rhs.0);
    let root = parent.add_node(NodeData {
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
    parent.add_edge(
        lhs_root,
        root,
        EdgeData {
            consumer_slot: 0,
            producer_output_slot: 0,
        },
    );
    parent.add_edge(
        rhs_root,
        root,
        EdgeData {
            consumer_slot: 1,
            producer_output_slot: 0,
        },
    );
    parent.set_root(root);
    Pat::from_graph(parent)
}

/// Variant-agnostic dispatcher: takes any `IntBinaryOp` at the pattern
/// level (useful for matching across op variants in generic rules).
/// Role propagation is identical to the typed variants.
#[must_use]
pub fn int_binary<R1, R2>(
    op: IntBinaryOp,
    lhs: Pat<R1>,
    rhs: Pat<R2>,
) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    binary_op_pat(op, lhs, rhs)
}

/// Match unsigned addition `lhs + rhs`.  Commutative.
#[must_use]
pub fn add<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    binary_op_pat(IntBinaryOp::Add, lhs, rhs)
}

/// Match a subtraction `lhs - rhs`.
///
/// `IntBinaryOp::Sub` is not a primitive in this IR; pcode-lift lowers
/// `IntSub(a, b)` at lift time to `Add(a, IntUnaryOp::Neg(b))`.  This
/// builder produces the lowered shape directly so `sub(a, b)` matches
/// the same IR `a - b` produces.
#[must_use]
pub fn sub<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    let neg_rhs: Pat<R2> = neg(rhs);
    add(lhs, neg_rhs)
}

/// Match wrapping multiplication `lhs * rhs`.  Commutative.
#[must_use]
pub fn mul<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    binary_op_pat(IntBinaryOp::Mul, lhs, rhs)
}

/// Match unsigned division `lhs / rhs`.  Not commutative.
#[must_use]
pub fn div<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    binary_op_pat(IntBinaryOp::Div, lhs, rhs)
}

/// Match signed division `(signed)lhs / (signed)rhs`.  Not commutative.
#[must_use]
pub fn sdiv<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    binary_op_pat(IntBinaryOp::Sdiv, lhs, rhs)
}

/// Match unsigned remainder `lhs % rhs`.  Not commutative.
#[must_use]
pub fn rem<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    binary_op_pat(IntBinaryOp::Rem, lhs, rhs)
}

/// Match signed remainder `(signed)lhs % (signed)rhs`.  Not commutative.
#[must_use]
pub fn srem<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    binary_op_pat(IntBinaryOp::Srem, lhs, rhs)
}

/// Match bitwise AND `lhs & rhs`.  Commutative.
#[must_use]
pub fn and<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    binary_op_pat(IntBinaryOp::And, lhs, rhs)
}

/// Match bitwise OR `lhs | rhs`.  Commutative.
#[must_use]
pub fn or<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    binary_op_pat(IntBinaryOp::Or, lhs, rhs)
}

/// Match bitwise XOR `lhs ^ rhs`.  Commutative.
#[must_use]
pub fn xor<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    binary_op_pat(IntBinaryOp::Xor, lhs, rhs)
}

/// Match logical left-shift `lhs << rhs`.  Not commutative.
#[must_use]
pub fn shl<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    binary_op_pat(IntBinaryOp::ShiftLeft, lhs, rhs)
}

/// Match logical (unsigned) right-shift `lhs >> rhs`.  Not commutative.
#[must_use]
pub fn shr<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    binary_op_pat(IntBinaryOp::ShiftRight, lhs, rhs)
}

/// Match arithmetic (signed) right-shift `(signed)lhs >> rhs`.  Not
/// commutative.
#[must_use]
pub fn sshr<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    binary_op_pat(IntBinaryOp::SShiftRight, lhs, rhs)
}
