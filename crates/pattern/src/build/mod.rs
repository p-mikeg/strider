//! Pattern-to-pattern rewrite-rule API.
//!
//! This module provides the right-hand side of a rewrite rule: a [`Build`]
//! tree that describes either
//!
//! * a captured [`ir::node::NodeOutputId`] from the LHS match, reused verbatim
//!   via [`Build::Capture`], or
//! * a fresh subgraph to be constructed and spliced into the IR via
//!   [`BuiltFunctionGraph::make_value_node`] / friends.
//!
//! [`rewrite_rule`] takes an existing [`crate::Pat`] (the LHS) and a `Build`
//! (the RHS) and returns a closure that, when applied to a function graph and
//! a candidate root node, attempts the match and on success redirects the
//! root's uses to the RHS output via
//! [`BuiltFunctionGraph::replace_all_uses`].
//!
//! [`apply_rules_in_order`] composes a list of rule closures, short-circuiting
//! as soon as any rule fires on a given root.
//!
//! # Typing policy (A3 simplification)
//!
//! For fresh nodes built from a `Build` subtree, the interpreter uses the
//! root's output type ([`BuildCtx::root_ty`]) for every node **unless** the
//! node kind dictates its own type:
//!
//! * `BoolConst`, `BoolBinary`, `BoolUnary`, `IntCmp`, `FloatCmp`
//!   — always produce [`NodeOutputType::Bool`].
//! * Every other arithmetic, bitwise, or constant node inherits `root_ty`.
//!
//! This is intentionally simple: a rule that needs mixed integer widths inside
//! a single RHS subtree is out of scope for A3 and should use a custom fold
//! function instead.  A later phase can extend the `Build` tree with
//! per-subtree type annotations if mixed-width rewrites become common.

use std::sync::Arc;

use ir::{
    BoolBinaryOp, BoolUnaryOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp,
    IntUnaryOp,
};

use crate::var::{
    BoolBinaryOpVar, BoolUnaryOpVar, FloatBinaryOpVar, FloatCmpOpVar, FloatUnaryOpVar,
    IntBinaryOpVar, IntCmpOpVar, IntUnaryOpVar, Var,
};

mod constructors;
mod ctx;
mod eval;
mod from_ctx_impls;
mod macros;
mod rule;

pub use constructors::{
    add, and, bool_and, bool_binary_from_var, bool_const_fn, bool_const_lit, bool_neg, bool_not,
    bool_or, bool_unary_from_var, bool_xor, cap, div, float_abs, float_add, float_binary_from_var,
    float_ceil, float_cmp_from_var, float_const_fn, float_const_lit, float_div, float_eq,
    float_floor, float_le, float_lt, float_mul, float_ne, float_neg, float_round, float_sqrt,
    float_sub, float_unary_from_var, int_binary_from_var, int_cmp_from_var, int_const_fn,
    int_const_lit, int_eq, int_le, int_lt, int_sle, int_slt, int_unary_from_var, mul, neg, not,
    or, rem, sdiv, shl, shr, skip, srem, sshr, sub, xor,
};
pub use ctx::{BuildCtx, BuildValue, BuildValueFn, FromCtx};
pub use macros::first_value_input_type;
pub use rule::{BoxedRule, RewriteOutcome, apply_rules_in_order, boxed_rule, rewrite_rule};

// ── Build tree ────────────────────────────────────────────────────────────────

/// RHS of a rewrite rule: either a reused capture or a fresh subgraph.
///
/// Every `Build` node that represents a value produces a single
/// [`NodeOutputId`] at evaluation time.  Composition is explicit — use the
/// `Arc<Build>` fields directly or the ergonomic helpers at the module root
/// (`cap`, `add`, `int_const_lit`, …).
#[derive(Clone)]
pub enum Build {
    /// Reuse a captured [`NodeOutputId`] from the LHS match.
    Capture(Var),

    /// Build a fresh `IntConst` node.
    IntConst(BuildValue<u64>),
    /// Build a fresh `BoolConst` node.  Always produces `NodeOutputType::Bool`.
    BoolConst(BuildValue<bool>),
    /// Build a fresh `FloatConst` node (IEEE 754 bit pattern).
    FloatConst(BuildValue<u64>),

    /// Build a fresh `IntBinaryOp` node with a concrete operator variant.
    IntBinary(IntBinaryOp, Arc<Build>, Arc<Build>),
    /// Build a fresh `IntUnaryOp` node with a concrete operator variant.
    IntUnary(IntUnaryOp, Arc<Build>),
    /// Build a fresh `IntCmpOp` node.  Always produces `NodeOutputType::Bool`.
    IntCmp(IntCmpOp, Arc<Build>, Arc<Build>),

    /// Build a fresh `BoolBinaryOp` node.  Always produces `NodeOutputType::Bool`.
    BoolBinary(BoolBinaryOp, Arc<Build>, Arc<Build>),
    /// Build a fresh `BoolUnaryOp` node.  Always produces `NodeOutputType::Bool`.
    BoolUnary(BoolUnaryOp, Arc<Build>),

    /// Build a fresh `FloatBinaryOp` node.
    FloatBinary(FloatBinaryOp, Arc<Build>, Arc<Build>),
    /// Build a fresh `FloatUnaryOp` node.
    FloatUnary(FloatUnaryOp, Arc<Build>),
    /// Build a fresh `FloatCmpOp` node.  Always produces `NodeOutputType::Bool`.
    FloatCmp(FloatCmpOp, Arc<Build>, Arc<Build>),

    // Variant-pass-through: the operator variant is resolved from a captured
    // `*OpVar` at evaluation time.  Fails if the variable is unbound.
    /// Build `IntBinaryOp(op_captured, lhs, rhs)`.
    IntBinaryFromVar(IntBinaryOpVar, Arc<Build>, Arc<Build>),
    /// Build `IntUnaryOp(op_captured, operand)`.
    IntUnaryFromVar(IntUnaryOpVar, Arc<Build>),
    /// Build `IntCmpOp(op_captured, lhs, rhs)` → `Bool`.
    IntCmpFromVar(IntCmpOpVar, Arc<Build>, Arc<Build>),
    /// Build `BoolBinaryOp(op_captured, lhs, rhs)`.
    BoolBinaryFromVar(BoolBinaryOpVar, Arc<Build>, Arc<Build>),
    /// Build `BoolUnaryOp(op_captured, operand)`.
    BoolUnaryFromVar(BoolUnaryOpVar, Arc<Build>),
    /// Build `FloatBinaryOp(op_captured, lhs, rhs)`.
    FloatBinaryFromVar(FloatBinaryOpVar, Arc<Build>, Arc<Build>),
    /// Build `FloatUnaryOp(op_captured, operand)`.
    FloatUnaryFromVar(FloatUnaryOpVar, Arc<Build>),
    /// Build `FloatCmpOp(op_captured, lhs, rhs)` → `Bool`.
    FloatCmpFromVar(FloatCmpOpVar, Arc<Build>, Arc<Build>),

    /// Abort the rewrite: a closure or structural check decided the rule
    /// doesn't apply after all.  At the top level this maps to
    /// [`RewriteOutcome::Skip`]; inside a larger subtree it propagates upward,
    /// causing the whole rewrite to be skipped.
    Skip,
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
