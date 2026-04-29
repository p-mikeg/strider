//! Variant-agnostic ("`*_any`") op constructors.
//!
//! These patterns match **any** variant of an op family (int binary, bool
//! unary, …) and bind the actual operator variant to a typed capture
//! variable.  All eight constructors share the same shape — a single
//! `impl_variant_any!` macro produces each, picking between binary/unary/cmp
//! input layouts and InheritRoot/Fixed(Bool) result type.

use std::sync::Arc;

use ir::node::NodeKind;

use crate::matcher::commutativity::{
    is_commutative_bool_op, is_commutative_float_op, is_commutative_int_cmp_op,
    is_commutative_int_op,
};
use crate::pat::Pat;
use crate::pat::node_pat::{BuildTy, InputsSpec, KindSpec, NodePat};
use crate::var::{
    BoolBinaryOpVar, BoolUnaryOpVar, FloatBinaryOpVar, FloatCmpOpVar, FloatUnaryOpVar,
    IntBinaryOpVar, IntCmpOpVar, IntUnaryOpVar,
};

// `binary` / `cmp` / `unary` tags select the ctor's input layout + arity.
// Commutativity deciders and missing-binding messages are derived from the
// enum / Var names.  `$sample_op` is an arbitrary variant of the op enum
// used only to build the `KindSpec::variant(...)` discriminant — payload
// is ignored.
macro_rules! impl_variant_any {
    // Binary-arity ($ctor) with a runtime commutativity decider.
    (binary, $fn_name:ident, $op_enum:ident, $sample_op:expr, $op_var:ident, $bind:ident, $get:ident,
     $commutative:path, $build_ty:expr, $missing:literal, $doc:literal) => {
        #[doc = $doc]
        pub fn $fn_name(op_var: $op_var, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
            let inputs = InputsSpec::fixed_maybe_commutative(lhs.into(), rhs.into(), |ctx, node| {
                match ctx.graph.graph.node_kind(node) {
                    NodeKind::$op_enum(op) => $commutative(*op),
                    _ => false,
                }
            });
            NodePat::matcher(
                KindSpec::variant(&NodeKind::$op_enum($sample_op)),
                inputs,
            )
            .with_build_fn(
                Arc::new(move |ctx| {
                    let op = ctx
                        .bindings
                        .$get(op_var)
                        .ok_or_else(|| anyhow::Error::new(crate::error::MissingBinding($missing)))?;
                    Ok(NodeKind::$op_enum(op))
                }),
                $build_ty,
            )
            .with_post_match(Arc::new(move |ctx, node, b| {
                match ctx.graph.graph.node_kind(node) {
                    NodeKind::$op_enum(op) => b.$bind(op_var, *op),
                    _ => false,
                }
            }))
            .into_pat()
        }
    };
    // Cmp-arity: two inputs, no commutativity retry.
    (cmp, $fn_name:ident, $op_enum:ident, $sample_op:expr, $op_var:ident, $bind:ident, $get:ident,
     $build_ty:expr, $missing:literal, $doc:literal) => {
        #[doc = $doc]
        pub fn $fn_name(op_var: $op_var, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
            NodePat::matcher(
                KindSpec::variant(&NodeKind::$op_enum($sample_op)),
                InputsSpec::fixed_ordered(vec![lhs.into(), rhs.into()]),
            )
            .with_build_fn(
                Arc::new(move |ctx| {
                    let op = ctx
                        .bindings
                        .$get(op_var)
                        .ok_or_else(|| anyhow::Error::new(crate::error::MissingBinding($missing)))?;
                    Ok(NodeKind::$op_enum(op))
                }),
                $build_ty,
            )
            .with_post_match(Arc::new(move |ctx, node, b| {
                match ctx.graph.graph.node_kind(node) {
                    NodeKind::$op_enum(op) => b.$bind(op_var, *op),
                    _ => false,
                }
            }))
            .into_pat()
        }
    };
    // Unary-arity: one input.
    (unary, $fn_name:ident, $op_enum:ident, $sample_op:expr, $op_var:ident, $bind:ident, $get:ident,
     $build_ty:expr, $missing:literal, $doc:literal) => {
        #[doc = $doc]
        pub fn $fn_name(op_var: $op_var, operand: impl Into<Pat>) -> Pat {
            NodePat::matcher(
                KindSpec::variant(&NodeKind::$op_enum($sample_op)),
                InputsSpec::fixed_ordered(vec![operand.into()]),
            )
            .with_build_fn(
                Arc::new(move |ctx| {
                    let op = ctx
                        .bindings
                        .$get(op_var)
                        .ok_or_else(|| anyhow::Error::new(crate::error::MissingBinding($missing)))?;
                    Ok(NodeKind::$op_enum(op))
                }),
                $build_ty,
            )
            .with_post_match(Arc::new(move |ctx, node, b| {
                match ctx.graph.graph.node_kind(node) {
                    NodeKind::$op_enum(op) => b.$bind(op_var, *op),
                    _ => false,
                }
            }))
            .into_pat()
        }
    };
}

// Shorthands for each family's constant result type.
fn bool_ty() -> BuildTy { BuildTy::Fixed(ir::node::NodeOutputType::Bool) }

impl_variant_any!(
    binary, int_binary_any, IntBinaryOp, ir::IntBinaryOp::Add, IntBinaryOpVar,
    bind_int_binary_op, get_int_binary_op,
    is_commutative_int_op, BuildTy::InheritRoot, "IntBinaryOpVar",
    "Matches **any** integer binary operation and binds the actual operator variant to `op`.\n\nCommutative ops (`Add`, `Mul`, `And`, `Or`, `Xor`) will try both operand orderings automatically."
);

impl_variant_any!(
    unary, int_unary_any, IntUnaryOp, ir::IntUnaryOp::Neg, IntUnaryOpVar,
    bind_int_unary_op, get_int_unary_op,
    BuildTy::InheritRoot, "IntUnaryOpVar",
    "Matches **any** integer unary operation and binds the actual operator variant to `op`."
);

impl_variant_any!(
    binary, int_cmp_any, IntCmpOp, ir::IntCmpOp::Equal, IntCmpOpVar,
    bind_int_cmp_op, get_int_cmp_op,
    is_commutative_int_cmp_op, bool_ty(), "IntCmpOpVar",
    "Matches **any** integer comparison and binds the actual operator variant to `op`.\n\nCommutative comparisons (`Equal`, `Carry`, `Scarry`) try both operand orderings automatically."
);

impl_variant_any!(
    binary, bool_binary_any, BoolBinaryOp, ir::BoolBinaryOp::And, BoolBinaryOpVar,
    bind_bool_binary_op, get_bool_binary_op,
    is_commutative_bool_op, bool_ty(), "BoolBinaryOpVar",
    "Matches **any** boolean binary operation and binds the actual operator variant to `op`.\n\nCommutative ops (`And`, `Or`, `Xor`) try both operand orderings automatically."
);

impl_variant_any!(
    unary, bool_unary_any, BoolUnaryOp, ir::BoolUnaryOp::Neg, BoolUnaryOpVar,
    bind_bool_unary_op, get_bool_unary_op,
    bool_ty(), "BoolUnaryOpVar",
    "Matches **any** boolean unary operation and binds the actual operator variant to `op`."
);

impl_variant_any!(
    binary, float_binary_any, FloatBinaryOp, ir::FloatBinaryOp::Add, FloatBinaryOpVar,
    bind_float_binary_op, get_float_binary_op,
    is_commutative_float_op, BuildTy::InheritRoot, "FloatBinaryOpVar",
    "Matches **any** float binary operation and binds the actual operator variant to `op`.\n\nCommutative ops (`Add`, `Mul`) try both operand orderings automatically."
);

impl_variant_any!(
    unary, float_unary_any, FloatUnaryOp, ir::FloatUnaryOp::Neg, FloatUnaryOpVar,
    bind_float_unary_op, get_float_unary_op,
    BuildTy::InheritRoot, "FloatUnaryOpVar",
    "Matches **any** float unary operation and binds the actual operator variant to `op`."
);

impl_variant_any!(
    cmp, float_cmp_any, FloatCmpOp, ir::FloatCmpOp::Equal, FloatCmpOpVar,
    bind_float_cmp_op, get_float_cmp_op,
    bool_ty(), "FloatCmpOpVar",
    "Matches **any** float comparison and binds the actual operator variant to `op`.\n\nNo float comparison operators are currently treated as commutative, so no automatic operand-swap retry is attempted."
);
