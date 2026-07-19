//! Context struct threaded through template instantiation.

use strider_ir::Function;
use strider_ir::node::{NodeId, ValueType};

/// Threaded through [`instantiate`](crate::template::instantiate) on every
/// [`TemplateKind::Fn`](crate::template::TemplateKind::Fn) evaluation. The
/// [`Function`] borrow lets closures read side-table state.
pub struct TemplateCtx<'a> {
    /// The function under rewrite.
    pub function: &'a Function,
    /// The LHS captures accumulated by the match.
    pub bindings: &'a crate::bindings::Bindings,
    /// The LHS root of the rewrite being instantiated.
    ///
    /// For closures needing the matched root's *input* shape:
    /// [`first_value_input_type`](crate::first_value_input_type) reads the
    /// comparison's operand type for `IntCmp` folding rules, whose root is
    /// `I1`-typed. Closures computing a constant purely from
    /// [`bindings`](Self::bindings) can ignore it.
    pub root: NodeId,
    /// The resolved output type for the node being materialised.
    pub root_ty: ValueType,
}
