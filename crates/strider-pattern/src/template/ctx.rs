//! Context struct threaded through template instantiation.

use strider_ir::Function;
use strider_ir::node::{NodeId, ValueType};

/// Per-rewrite context for template instantiation.
///
/// Threaded through [`instantiate`](crate::template::instantiate) on
/// every [`TemplateKind::Fn`](crate::template::TemplateKind::Fn)
/// evaluation. Exposes the captured LHS
/// [`Bindings`](crate::Bindings), the matched-root `NodeId` plus its
/// resolved output type, and a borrow on the [`Function`] under rewrite
/// so closures may read side-table state.
pub struct TemplateCtx<'a> {
    /// The function under rewrite.
    pub function: &'a Function,
    /// The LHS captures accumulated by the match.
    pub bindings: &'a crate::bindings::Bindings,
    /// The LHS-root `NodeId` of the rewrite being instantiated.
    ///
    /// Load-bearing for closures that need to inspect the matched
    /// root's *input* shape — e.g. the
    /// [`first_value_input_type`](crate::first_value_input_type) helper
    /// used by `IntCmp` constant-folding rules pulls the comparison's
    /// operand type when the root itself is `I1`-typed. Closures that
    /// only compute a constant from [`bindings`](Self::bindings) can
    /// ignore this field.
    pub root: NodeId,
    /// The resolved output type for the node being materialised.
    pub root_ty: ValueType,
}
