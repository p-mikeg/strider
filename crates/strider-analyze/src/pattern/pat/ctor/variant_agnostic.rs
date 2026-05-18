//! Variant-agnostic ("`*_any`") op constructors.
//!
//! These patterns match **any** variant of an op family (int binary, bool
//! unary, …) and bind the matched node to a [`Capture`].  After the match,
//! callers recover the concrete op variant via the matching
//! [`Match::get_*_op`](crate::pattern::Match) helper.
//!
//! All eight constructors share the same shape — a single
//! `impl_variant_any!` macro produces each, picking between binary/unary/cmp
//! input layouts and InheritRoot/Fixed(Bool) result type.

use std::sync::Arc;

use strider_ir::node::NodeKind;

use crate::pattern::matcher::bindings::Binding;
use crate::pattern::matcher::commutativity::{
    is_commutative_bool_op, is_commutative_float_cmp_op, is_commutative_float_op,
    is_commutative_int_cmp_op, is_commutative_int_op,
};
use crate::pattern::pat::Pat;
use crate::pattern::pat::node_pat::{BuildTy, InputsSpec, KindSpec, NodePat};
use crate::pattern::var::Capture;

// `binary` / `cmp` / `unary` tags select the ctor's input layout + arity.
// Commutativity deciders are wired per-family.  `$sample_op` is an
// arbitrary variant of the op enum used only to build the
// `KindSpec::variant(...)` discriminant — payload is ignored.
//
// The post_match closure binds the matched `NodeId` to the supplied
// `Capture` so callers can recover the op variant via
// `Match::get_*_op(capture, &graph)`.
//
// The RHS-build closure inspects the bound node and rebuilds the same
// `NodeKind::*Op(op)` variant, so build-time RHS templates that use a
// captured op (e.g. `int_binary_any(c, lhs, rhs)` on the RHS of a
// rewrite rule) materialize the right discriminant.
macro_rules! impl_variant_any {
    // Binary-arity with a runtime commutativity decider.
    (binary, $fn_name:ident, $op_enum:ident, $sample_op:expr,
     $commutative:path, $build_ty:expr, $missing:literal, $doc:literal) => {
        #[doc = $doc]
        pub fn $fn_name(c: Capture, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
            let inputs = InputsSpec::fixed_maybe_commutative(lhs.into(), rhs.into(), |ctx, node| {
                match ctx.graph.node_kind(node) {
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
                    let node = ctx
                        .bindings
                        .get_node(c)
                        .ok_or_else(|| crate::pattern::error::missing_binding($missing))?;
                    match ctx.graph.node_kind(node) {
                        NodeKind::$op_enum(op) => Ok(NodeKind::$op_enum(*op)),
                        _ => Err(crate::pattern::error::missing_binding($missing)),
                    }
                }),
                $build_ty,
            )
            .with_post_match(Arc::new(move |ctx, node, b| {
                if matches!(ctx.graph.node_kind(node), NodeKind::$op_enum(_)) {
                    // Populate the value output too so callers can use
                    // both `Match::get_*_op(c, &graph)` (op-variant) AND
                    // typed extractors / `Match::output(c)` (value).
                    let value_out = ctx.graph.node_outputs(node).into_iter().find(|&o| {
                        ctx.graph.output_kind(o).is_value()
                    });
                    b.bind_capture(c, Binding::new(node, value_out))
                } else {
                    false
                }
            }))
            .into_pat()
        }
    };
    // Cmp-arity with a runtime commutativity decider (shape mirrors `binary`).
    (cmp, $fn_name:ident, $op_enum:ident, $sample_op:expr,
     $commutative:path, $build_ty:expr, $missing:literal, $doc:literal) => {
        #[doc = $doc]
        pub fn $fn_name(c: Capture, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
            let inputs = InputsSpec::fixed_maybe_commutative(lhs.into(), rhs.into(), |ctx, node| {
                match ctx.graph.node_kind(node) {
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
                    let node = ctx
                        .bindings
                        .get_node(c)
                        .ok_or_else(|| crate::pattern::error::missing_binding($missing))?;
                    match ctx.graph.node_kind(node) {
                        NodeKind::$op_enum(op) => Ok(NodeKind::$op_enum(*op)),
                        _ => Err(crate::pattern::error::missing_binding($missing)),
                    }
                }),
                $build_ty,
            )
            .with_post_match(Arc::new(move |ctx, node, b| {
                if matches!(ctx.graph.node_kind(node), NodeKind::$op_enum(_)) {
                    // Populate the value output too so callers can use
                    // both `Match::get_*_op(c, &graph)` (op-variant) AND
                    // typed extractors / `Match::output(c)` (value).
                    let value_out = ctx.graph.node_outputs(node).into_iter().find(|&o| {
                        ctx.graph.output_kind(o).is_value()
                    });
                    b.bind_capture(c, Binding::new(node, value_out))
                } else {
                    false
                }
            }))
            .into_pat()
        }
    };
    // Unary-arity: one input.
    (unary, $fn_name:ident, $op_enum:ident, $sample_op:expr,
     $build_ty:expr, $missing:literal, $doc:literal) => {
        #[doc = $doc]
        pub fn $fn_name(c: Capture, operand: impl Into<Pat>) -> Pat {
            NodePat::matcher(
                KindSpec::variant(&NodeKind::$op_enum($sample_op)),
                InputsSpec::fixed_ordered(vec![operand.into()]),
            )
            .with_build_fn(
                Arc::new(move |ctx| {
                    let node = ctx
                        .bindings
                        .get_node(c)
                        .ok_or_else(|| crate::pattern::error::missing_binding($missing))?;
                    match ctx.graph.node_kind(node) {
                        NodeKind::$op_enum(op) => Ok(NodeKind::$op_enum(*op)),
                        _ => Err(crate::pattern::error::missing_binding($missing)),
                    }
                }),
                $build_ty,
            )
            .with_post_match(Arc::new(move |ctx, node, b| {
                if matches!(ctx.graph.node_kind(node), NodeKind::$op_enum(_)) {
                    // Populate the value output too so callers can use
                    // both `Match::get_*_op(c, &graph)` (op-variant) AND
                    // typed extractors / `Match::output(c)` (value).
                    let value_out = ctx.graph.node_outputs(node).into_iter().find(|&o| {
                        ctx.graph.output_kind(o).is_value()
                    });
                    b.bind_capture(c, Binding::new(node, value_out))
                } else {
                    false
                }
            }))
            .into_pat()
        }
    };
}

// Shorthands for each family's constant result type.
fn bool_ty() -> BuildTy { BuildTy::Fixed(strider_ir::node::NodeOutputType::Bool) }

impl_variant_any!(
    binary, int_binary_any, IntBinaryOp, strider_ir::IntBinaryOp::Add,
    is_commutative_int_op, BuildTy::InheritRoot, "int_binary_any",
    "Matches **any** integer binary operation and binds the matched node to `c`.\n\nCommutative ops (`Add`, `Mul`, `And`, `Or`, `Xor`) will try both operand orderings automatically.  Recover the op via `Match::get_int_binary_op(c, &graph)`."
);

impl_variant_any!(
    unary, int_unary_any, IntUnaryOp, strider_ir::IntUnaryOp::BitNot,
    BuildTy::InheritRoot, "int_unary_any",
    "Matches **any** integer unary operation and binds the matched node to `c`.\n\nRecover the op via `Match::get_int_unary_op(c, &graph)`."
);

impl_variant_any!(
    cmp, int_cmp_any, IntCmpOp, strider_ir::IntCmpOp::Equal,
    is_commutative_int_cmp_op, bool_ty(), "int_cmp_any",
    "Matches **any** integer comparison and binds the matched node to `c`.\n\nCommutative comparisons (`Equal`, `Carry`, `Scarry`) try both operand orderings automatically.  Recover the op via `Match::get_int_cmp_op(c, &graph)`."
);

impl_variant_any!(
    binary, bool_binary_any, BoolBinaryOp, strider_ir::BoolBinaryOp::And,
    is_commutative_bool_op, bool_ty(), "bool_binary_any",
    "Matches **any** boolean binary operation and binds the matched node to `c`.\n\nCommutative ops (`And`, `Or`, `Xor`) try both operand orderings automatically.  Recover the op via `Match::get_bool_binary_op(c, &graph)`."
);

impl_variant_any!(
    unary, bool_unary_any, BoolUnaryOp, strider_ir::BoolUnaryOp::Neg,
    bool_ty(), "bool_unary_any",
    "Matches **any** boolean unary operation and binds the matched node to `c`.\n\nRecover the op via `Match::get_bool_unary_op(c, &graph)`."
);

impl_variant_any!(
    binary, float_binary_any, FloatBinaryOp, strider_ir::FloatBinaryOp::Add,
    is_commutative_float_op, BuildTy::InheritRoot, "float_binary_any",
    "Matches **any** float binary operation and binds the matched node to `c`.\n\nCommutative ops (`Add`, `Mul`) try both operand orderings automatically.  Recover the op via `Match::get_float_binary_op(c, &graph)`."
);

impl_variant_any!(
    unary, float_unary_any, FloatUnaryOp, strider_ir::FloatUnaryOp::Neg,
    BuildTy::InheritRoot, "float_unary_any",
    "Matches **any** float unary operation and binds the matched node to `c`.\n\nRecover the op via `Match::get_float_unary_op(c, &graph)`."
);

impl_variant_any!(
    cmp, float_cmp_any, FloatCmpOp, strider_ir::FloatCmpOp::Equal,
    is_commutative_float_cmp_op, bool_ty(), "float_cmp_any",
    "Matches **any** float comparison and binds the matched node to `c`.\n\n`Equal` is commutative and tries both operand orderings automatically.  `NotEqual` and `LessEqual` are not IR primitives — they are lowered at lift time; use the `float_ne` / `float_le` aliases to match those shapes.  Recover the op via `Match::get_float_cmp_op(c, &graph)`."
);
