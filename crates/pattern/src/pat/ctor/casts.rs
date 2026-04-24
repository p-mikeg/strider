//! Cast / coercion / bit-width-change pattern constructors.

use std::sync::Arc;

use ir::ExtendOp;
use ir::node::NodeKind;

use crate::pat::Pat;
use crate::pat::node_pat::{BuildTy, InputsSpec, NodeKindCheck, NodePat};

/// Helper: build a unary-input NodePat whose kind_match is supplied by the
/// caller and whose kind_build emits a fixed `NodeKind`.  Covers the
/// unit-variant casts (`CastToBool`, `Truncate`, `Popcount`, …).
fn unary_node(
    kind_match: NodeKindCheck,
    build_kind: NodeKind,
    build_ty: BuildTy,
    operand: impl Into<Pat>,
) -> Pat {
    NodePat::matcher(kind_match, InputsSpec::fixed_ordered(vec![operand.into()]))
        .with_build(Arc::new(move |_b| Ok(build_kind)))
        .with_build_ty(build_ty)
        .into_pat()
}

macro_rules! simple_unary_cast {
    ($fn_name:ident, $variant:ident, $build_ty:expr, $doc:literal) => {
        #[doc = $doc]
        pub fn $fn_name(operand: impl Into<Pat>) -> Pat {
            unary_node(
                Arc::new(|ctx, node, _b| matches!(ctx.graph.graph.node_kind(node), NodeKind::$variant)),
                NodeKind::$variant,
                $build_ty,
                operand,
            )
        }
    };
}

simple_unary_cast!(
    cast_to_bool,
    CastToBool,
    BuildTy::Fixed(ir::node::NodeOutputType::Bool),
    "Matches a `CastToBool` node (non-zero integer → `true`)."
);
simple_unary_cast!(
    cast_to_int,
    CastToInt,
    BuildTy::InheritRoot,
    "Matches a `CastToInt` node (`bool` → `0` or `1`)."
);
simple_unary_cast!(
    cast_to_float,
    CastToFloat,
    BuildTy::InheritRoot,
    "Matches a `CastToFloat` generic-cast node."
);
simple_unary_cast!(
    truncate,
    Truncate,
    BuildTy::InheritRoot,
    "Matches a `Truncate` node (narrows an integer to fewer bits)."
);
simple_unary_cast!(
    popcount,
    Popcount,
    BuildTy::InheritRoot,
    "Matches a popcount (count-set-bits) node."
);
simple_unary_cast!(
    lzcount,
    Lzcount,
    BuildTy::InheritRoot,
    "Matches a leading-zero-count node."
);

/// Matches an `Extend` node with the given extension kind.
pub fn extend(op: ExtendOp, operand: impl Into<Pat>) -> Pat {
    unary_node(
        Arc::new(move |ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::Extend(actual) if *actual == op)
        }),
        NodeKind::Extend(op),
        BuildTy::InheritRoot,
        operand,
    )
}
/// Matches a zero-extension node.
pub fn zero_extend(operand: impl Into<Pat>) -> Pat {
    extend(ExtendOp::ZeroExtend, operand)
}
/// Matches a sign-extension node.
pub fn sign_extend(operand: impl Into<Pat>) -> Pat {
    extend(ExtendOp::SignExtend, operand)
}
