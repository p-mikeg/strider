//! Shared backing helper for the per-family comparison constructors
//! (`int_cmp`, `float_cmp`).
//!
//! Both produce `Bool`-typed nodes and pick between commutative-retry
//! and stated-order operand binding based on whether the op variant
//! is symmetric.  [`CmpOpKind`] threads the discriminant and the
//! commutativity decider through one [`cmp_pat`] helper.

use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir::{FloatCmpOp, IntCmpOp};

use crate::pattern::pat::Pat;
use crate::pattern::pat::node_pat::{BuildTy, InputsSpec, KindSpec, NodePat};

/// Per-family parameters threaded through [`cmp_pat`].  Crate-private
/// because the helper returns a [`Pat`] regardless of family — no
/// outside-the-crate impls are useful.
///
/// Commutativity is read off the resulting `NodeKind` via
/// [`NodeKind::is_commutative`] — no per-family helper is needed.
pub(crate) trait CmpOpKind: Copy + 'static {
    fn node_kind(self) -> NodeKind;
}

impl CmpOpKind for IntCmpOp {
    fn node_kind(self) -> NodeKind { NodeKind::IntCmpOp(self) }
}

impl CmpOpKind for FloatCmpOp {
    fn node_kind(self) -> NodeKind { NodeKind::FloatCmpOp(self) }
}

/// Build a cmp-op pattern parameterised by the op family.  Used by
/// the per-family `int_cmp` / `float_cmp` ctors; not re-exported
/// because the trait is crate-private.
pub(crate) fn cmp_pat<Op: CmpOpKind>(op: Op, lhs: Pat, rhs: Pat) -> Pat {
    let kind = op.node_kind();
    let inputs = if kind.is_commutative() {
        InputsSpec::fixed_commutative(lhs, rhs)
    } else {
        InputsSpec::fixed_ordered(vec![lhs, rhs])
    };
    NodePat::matcher(KindSpec::Exact(kind), inputs)
        .with_build_exact(kind, BuildTy::Fixed(NodeOutputType::Bool))
        .into_pat()
}
