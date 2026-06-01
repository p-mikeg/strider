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

use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir::IntBinaryOp;

use crate::pat_graph::{
    TemplateKind, TemplateSpec, TemplateTy, Combine, Concrete, EdgeData, KindSpec, NodeData, PatGraph, Role,
    Wildcard, merge_subgraph,
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
        template_spec: Some(TemplateSpec {
            kind: TemplateKind::Exact(kind),
            ty: TemplateTy::InheritRoot,
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

/// Match **any** `IntBinaryOp` variant.  Wildcard role; the kind
/// dispatch is by [`KindSpec::Variant`] on the `IntBinaryOp`
/// discriminant — payload is ignored.  Useful in rules that need to
/// quantify over every binary op (the constant-folder, for one).
/// Recover the matched variant after the fact via
/// `Bindings::get_int_binary_op(c, &graph)`.
///
/// The parent has no [`TemplateSpec`], so this builder is match-only:
/// using it on the RHS of a rewrite rule will silently never
/// materialise.
#[must_use]
pub fn int_binary_any<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<Wildcard>
where
    R1: Role,
    R2: Role,
{
    let exemplar = NodeKind::IntBinaryOp(IntBinaryOp::Add);
    let mut parent: PatGraph<Wildcard> = PatGraph::new();
    let lhs_root = merge_subgraph(&mut parent, lhs.0);
    let rhs_root = merge_subgraph(&mut parent, rhs.0);
    let root = parent.add_node(NodeData {
        kind: KindSpec::Variant(std::mem::discriminant(&exemplar)),
        output_ty: None,
        capture: None,
        post_match: None,
        template_spec: None,
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

/// Match an `IntConst` (or `IntConstWide` for I256 / I512) whose
/// stored value, masked to the node's output width, equals the
/// all-ones bit pattern `(2^bit_width) - 1`.  Used by [`bit_not`]
/// (and the canonical `Xor(x, all_ones)` shape) to recognise
/// bitwise complement at any width.
///
/// In build position (RHS of a rewrite rule), materialises an
/// `IntConst(mask)` whose value equals the rewrite root's all-ones
/// mask.  Wide widths (I256 / I512) opt out via
/// [`crate::skip`] because emitting an `IntConstWide` RHS would
/// require interning into the graph at build time and the
/// [`crate::matcher::TemplateCtx`] only carries a shared
/// [`Function`](strider_ir::Function) reference.
#[must_use]
pub fn int_const_all_ones() -> Pat<Concrete> {
    use strider_ir::wide_const::WideConstStorage;

    let mut g: PatGraph<Concrete> = PatGraph::new();
    let post_match: crate::pat_graph::PostMatchFn = Box::new(|m, node, _ty, _b| {
        let f = m.function();
        // Find the node's first value-output and recover its type;
        // bail if the node has no value output (control-only kinds).
        let Some(out_ty) = f
            .node_outputs(node)
            .iter()
            .find_map(|&out| f.output_kind(out).as_value())
        else {
            return false;
        };
        if !out_ty.is_integer() {
            return false;
        }
        match *f.node_kind(node) {
            NodeKind::IntConst(stored) => {
                // IntConst(u128) rejects I256 / I512 at build time, so
                // a stored u128::MAX value at one of those widths is
                // structurally impossible; the wide branch (below)
                // handles that case via IntConstWide.
                if matches!(out_ty, NodeOutputType::I256 | NodeOutputType::I512) {
                    return false;
                }
                let mask = out_ty.bit_mask_u128();
                (stored & mask) == mask
            }
            NodeKind::IntConstWide(id) => {
                let stored = f.wide_const(id);
                let Some(all_ones) = WideConstStorage::all_ones(out_ty.byte_size()) else {
                    return false;
                };
                *stored == all_ones
            }
            _ => false,
        }
    });
    let template_kind = TemplateKind::Fn(Box::new(|ctx| {
        let ty = ctx.root_ty;
        if matches!(ty, NodeOutputType::I256 | NodeOutputType::I512) {
            // Build-side has no &mut Function in TemplateCtx, so we
            // can't intern a fresh WideConstStorage here.  Bail out
            // with the rewrite-skip sentinel — the caller's rule
            // will return Ok(false) instead of a hard error.
            return Err(crate::skip());
        }
        Ok(NodeKind::IntConst(ty.bit_mask_u128()))
    }));
    let n = g.add_node(NodeData {
        kind: KindSpec::Any,
        output_ty: None,
        capture: None,
        post_match: Some(post_match),
        template_spec: Some(TemplateSpec {
            kind: template_kind,
            ty: TemplateTy::InheritRoot,
        }),
        force_ordered: false,
    });
    g.set_root(n);
    Pat::from_graph(g)
}

/// Match a bitwise complement node — `Xor(operand, IntConst(all_ones))`.
///
/// The former `BitNot` unary-op variant was removed in favour of the
/// canonical `Xor(x, all_ones)` shape (`~x ≡ x ^ all_ones`).  Role
/// propagates from `operand`: since [`int_const_all_ones`] is
/// `Pat<Concrete>`, the `Combine` rule only widens when `operand` is
/// `Wildcard` — so `bit_not(var(x))` (Concrete) is usable on the
/// RHS of a rewrite rule.
#[must_use]
pub fn bit_not<R>(operand: Pat<R>) -> Pat<R>
where
    R: Combine<Concrete, Output = R>,
{
    binary_op_pat(IntBinaryOp::Xor, operand, int_const_all_ones())
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
