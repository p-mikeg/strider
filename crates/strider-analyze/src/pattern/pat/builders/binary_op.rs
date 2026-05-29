//! Generic typed binary-op builder shared by [`IntBinaryOp`] /
//! [`FloatBinaryOp`].
//!
//! The two families differ only in two small details:
//! * which `NodeKind` variant the op enum produces;
//! * which subset of variants is commutative.
//!
//! Both build with [`BuildTy::InheritRoot`].
//!
//! [`BinaryOpKind`] threads those differences through one generic
//! [`BinaryOpPat<Op>`] type so the matcher / build-side code stays
//! factored.  Free constructors (`add`, `float_add`, …) re-export the
//! two concrete instantiations as [`IntBinaryOpPat`] / [`FloatBinaryOpPat`].
//!
//! Booleans are 1-bit integers (`I1`) in this IR, so a boolean binary op is
//! an [`IntBinaryOp`] (`And` / `Or` / `Xor`) whose output is `I1`.  It gets
//! its own chainable [`BoolBinaryOpPat`] (constructed by `bool_binary` /
//! `bool_and` / …) rather than reusing [`IntBinaryOpPat`] — the bool builder
//! pins the output type to `I1` and attaches an `I1`-output post-match guard
//! so it never matches a same-shaped wide integer op.

use std::sync::Arc;

use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir::{FloatBinaryOp, IntBinaryOp};

use crate::pattern::pat::Pat;
use crate::pattern::pat::node_pat::{BuildTy, InputsSpec, KindSpec, NodePat, PostMatchFn};

/// Per-family parameters threaded through the generic [`BinaryOpPat`].
///
/// Implemented for every binary-op enum the IR exposes
/// ([`IntBinaryOp`] / [`FloatBinaryOp`]).  Crate-private because
/// `build_ty` returns the crate-private [`BuildTy`] and the public
/// surface only needs the two concrete alias types.
///
/// Commutativity is read off the resulting `NodeKind` via
/// [`NodeKind::is_commutative`] — no per-family helper is needed.
pub(crate) trait BinaryOpKind: Copy + 'static {
    fn node_kind(self) -> NodeKind;
    fn build_ty(self) -> BuildTy;
}

impl BinaryOpKind for IntBinaryOp {
    fn node_kind(self) -> NodeKind { NodeKind::IntBinaryOp(self) }
    fn build_ty(self) -> BuildTy { BuildTy::InheritRoot }
}

impl BinaryOpKind for FloatBinaryOp {
    fn node_kind(self) -> NodeKind { NodeKind::FloatBinaryOp(self) }
    fn build_ty(self) -> BuildTy { BuildTy::InheritRoot }
}

/// Builder for a typed binary-op pattern over any `Op: BinaryOpKind`.
///
/// Constructed by free functions (`add`, `bool_and`, `float_add`, …)
/// or via the family-level dispatchers (`int_binary`, `bool_binary`,
/// `float_binary`).  See [`IntBinaryOpPat`] / [`FloatBinaryOpPat`] for
/// the two concrete instantiations the public API exposes (boolean ops
/// reuse [`IntBinaryOpPat`] at `I1`).
///
/// `BinaryOpKind` is crate-private — the only way to construct a
/// value of this type from outside the crate is through one of the
/// free constructors, which always returns one of the two aliased
/// instantiations.  `#[allow(private_bounds)]` documents that intent.
#[allow(private_bounds)]
pub struct BinaryOpPat<Op: BinaryOpKind> {
    op: Op,
    lhs: Pat,
    rhs: Pat,
    ordered: bool,
}

#[allow(private_bounds)]
impl<Op: BinaryOpKind> BinaryOpPat<Op> {
    pub(crate) fn new(op: Op, lhs: Pat, rhs: Pat) -> Self {
        Self { op, lhs, rhs, ordered: false }
    }

    /// Force the pattern to match operands in the stated order only.
    /// By default, commutative variants of the op family also try
    /// the reversed operand order.
    #[must_use]
    pub fn ordered(mut self) -> Self {
        self.ordered = true;
        self
    }
}

#[allow(private_bounds)]
impl<Op: BinaryOpKind> From<BinaryOpPat<Op>> for Pat {
    fn from(b: BinaryOpPat<Op>) -> Pat {
        let op = b.op;
        let kind = op.node_kind();
        let inputs = if !b.ordered && kind.is_commutative() {
            InputsSpec::fixed_commutative(b.lhs, b.rhs)
        } else {
            InputsSpec::fixed_ordered(vec![b.lhs, b.rhs])
        };
        NodePat::matcher(KindSpec::Exact(kind), inputs)
            .with_build_exact(kind, op.build_ty())
            .into_pat()
    }
}

/// Back-compat alias: typed builder for integer binary-op patterns.
pub type IntBinaryOpPat = BinaryOpPat<IntBinaryOp>;
/// Back-compat alias: typed builder for float binary-op patterns.
pub type FloatBinaryOpPat = BinaryOpPat<FloatBinaryOp>;

/// Post-match guard restricting a match to a node whose value output is the
/// 1-bit boolean `I1`.  Without it the `bool_*` matchers would also accept a
/// same-shaped wide integer op (e.g. a 64-bit `And`), since after the bool→I1
/// collapse a boolean op and a wide integer op share the same `NodeKind`.
fn require_i1_output() -> PostMatchFn {
    Arc::new(|ctx, node, _bindings| {
        ctx.function
            .node_outputs(node)
            .iter()
            .find_map(|&out| ctx.function.output_kind(out).as_value())
            .is_some_and(|ty| ty.bit_width() == 1)
    })
}

/// Typed builder for a boolean binary-op pattern (`And` / `Or` / `Xor` at
/// `I1`).
///
/// Booleans are 1-bit integers (`I1`) in this IR, so a boolean binary op is
/// an [`IntBinaryOp`] whose output is `I1`.  This builder mirrors the shape of
/// [`IntBinaryOpPat`] / [`FloatBinaryOpPat`] (chainable `.ordered()`) but
/// pins the output type to `I1` and attaches an `I1`-output post-match guard,
/// so it never matches a same-shaped wide integer op.
///
/// Constructed by `bool_binary` / `bool_and` / `bool_or` / `bool_xor`.
pub struct BoolBinaryOpPat {
    op: IntBinaryOp,
    lhs: Pat,
    rhs: Pat,
    ordered: bool,
}

impl BoolBinaryOpPat {
    pub(crate) fn new(op: IntBinaryOp, lhs: Pat, rhs: Pat) -> Self {
        Self { op, lhs, rhs, ordered: false }
    }

    /// Force the pattern to match operands in the stated order only.
    /// By default, commutative variants (`And` / `Or` / `Xor`) also try
    /// the reversed operand order.
    #[must_use]
    pub fn ordered(mut self) -> Self {
        self.ordered = true;
        self
    }
}

impl From<BoolBinaryOpPat> for Pat {
    fn from(b: BoolBinaryOpPat) -> Pat {
        let kind = NodeKind::IntBinaryOp(b.op);
        let inputs = if !b.ordered && kind.is_commutative() {
            InputsSpec::fixed_commutative(b.lhs, b.rhs)
        } else {
            InputsSpec::fixed_ordered(vec![b.lhs, b.rhs])
        };
        NodePat::matcher(KindSpec::Exact(kind), inputs)
            .with_post_match(require_i1_output())
            .with_build_exact(kind, BuildTy::Fixed(NodeOutputType::I1))
            .into_pat()
    }
}
