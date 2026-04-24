//! Cast / coercion / bit-width-change pattern constructors.

use std::sync::Arc;

use ir::ExtendOp;
use ir::node::NodeKind;

use crate::pat::Pat;
use crate::pat::node_pat::{InputsSpec, NodePat};

/// Matches a `CastToBool` node (non-zero integer → `true`).
pub fn cast_to_bool(operand: impl Into<Pat>) -> Pat {
    Pat::from_dyn(Arc::new(NodePat {
        kind_build: Some(Arc::new(|_b| Ok(NodeKind::CastToBool))),
        build_result_ty: crate::pat::node_pat::BuildTy::Fixed(ir::node::NodeOutputType::Bool),
        outputs: crate::pat::node_pat::OutputsSpec::None,
        consumers: crate::pat::node_pat::ConsumersSpec::None,
        candidate_kind: None,
        kind_match: Arc::new(|ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::CastToBool)
        }),
        inputs: InputsSpec::fixed_ordered(vec![operand.into()]),
        post_match: None,
        output_var: None,
        node_var: None,
    }))
}
/// Matches a `CastToInt` node (`bool` → `0` or `1`).
pub fn cast_to_int(operand: impl Into<Pat>) -> Pat {
    Pat::from_dyn(Arc::new(NodePat {
        kind_build: Some(Arc::new(|_b| Ok(NodeKind::CastToInt))),
        build_result_ty: crate::pat::node_pat::BuildTy::InheritRoot,
        outputs: crate::pat::node_pat::OutputsSpec::None,
        consumers: crate::pat::node_pat::ConsumersSpec::None,
        candidate_kind: None,
        kind_match: Arc::new(|ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::CastToInt)
        }),
        inputs: InputsSpec::fixed_ordered(vec![operand.into()]),
        post_match: None,
        output_var: None,
        node_var: None,
    }))
}
/// Matches a `CastToFloat` generic-cast node.
pub fn cast_to_float(operand: impl Into<Pat>) -> Pat {
    Pat::from_dyn(Arc::new(NodePat {
        kind_build: Some(Arc::new(|_b| Ok(NodeKind::CastToFloat))),
        build_result_ty: crate::pat::node_pat::BuildTy::InheritRoot,
        outputs: crate::pat::node_pat::OutputsSpec::None,
        consumers: crate::pat::node_pat::ConsumersSpec::None,
        candidate_kind: None,
        kind_match: Arc::new(|ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::CastToFloat)
        }),
        inputs: InputsSpec::fixed_ordered(vec![operand.into()]),
        post_match: None,
        output_var: None,
        node_var: None,
    }))
}
/// Matches a `Truncate` node (narrows an integer to fewer bits).
pub fn truncate(operand: impl Into<Pat>) -> Pat {
    Pat::from_dyn(Arc::new(NodePat {
        kind_build: Some(Arc::new(|_b| Ok(NodeKind::Truncate))),
        build_result_ty: crate::pat::node_pat::BuildTy::InheritRoot,
        outputs: crate::pat::node_pat::OutputsSpec::None,
        consumers: crate::pat::node_pat::ConsumersSpec::None,
        candidate_kind: None,
        kind_match: Arc::new(|ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::Truncate)
        }),
        inputs: InputsSpec::fixed_ordered(vec![operand.into()]),
        post_match: None,
        output_var: None,
        node_var: None,
    }))
}
/// Matches an `Extend` node with the given extension kind.
pub fn extend(op: ExtendOp, operand: impl Into<Pat>) -> Pat {
    Pat::from_dyn(Arc::new(NodePat {
        kind_build: Some(Arc::new(move |_b| Ok(NodeKind::Extend(op)))),
        build_result_ty: crate::pat::node_pat::BuildTy::InheritRoot,
        outputs: crate::pat::node_pat::OutputsSpec::None,
        consumers: crate::pat::node_pat::ConsumersSpec::None,
        candidate_kind: None,
        kind_match: Arc::new(move |ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::Extend(actual) if *actual == op)
        }),
        inputs: InputsSpec::fixed_ordered(vec![operand.into()]),
        post_match: None,
        output_var: None,
        node_var: None,
    }))
}
/// Matches a zero-extension node.
pub fn zero_extend(operand: impl Into<Pat>) -> Pat {
    extend(ExtendOp::ZeroExtend, operand)
}
/// Matches a sign-extension node.
pub fn sign_extend(operand: impl Into<Pat>) -> Pat {
    extend(ExtendOp::SignExtend, operand)
}
/// Matches a popcount (count-set-bits) node.
pub fn popcount(operand: impl Into<Pat>) -> Pat {
    Pat::from_dyn(Arc::new(NodePat {
        kind_build: Some(Arc::new(|_b| Ok(NodeKind::Popcount))),
        build_result_ty: crate::pat::node_pat::BuildTy::InheritRoot,
        outputs: crate::pat::node_pat::OutputsSpec::None,
        consumers: crate::pat::node_pat::ConsumersSpec::None,
        candidate_kind: None,
        kind_match: Arc::new(|ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::Popcount)
        }),
        inputs: InputsSpec::fixed_ordered(vec![operand.into()]),
        post_match: None,
        output_var: None,
        node_var: None,
    }))
}
/// Matches a leading-zero-count node.
pub fn lzcount(operand: impl Into<Pat>) -> Pat {
    Pat::from_dyn(Arc::new(NodePat {
        kind_build: Some(Arc::new(|_b| Ok(NodeKind::Lzcount))),
        build_result_ty: crate::pat::node_pat::BuildTy::InheritRoot,
        outputs: crate::pat::node_pat::OutputsSpec::None,
        consumers: crate::pat::node_pat::ConsumersSpec::None,
        candidate_kind: None,
        kind_match: Arc::new(|ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::Lzcount)
        }),
        inputs: InputsSpec::fixed_ordered(vec![operand.into()]),
        post_match: None,
        output_var: None,
        node_var: None,
    }))
}
