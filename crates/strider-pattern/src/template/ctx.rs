//! Context struct threaded through template instantiation.

use strider_ir::Function;
use strider_ir::node::{NodeId, NodeOutputType};

/// Per-rewrite context for template instantiation.
///
/// Threaded through [`Template::instantiate`](crate::template::Template::instantiate)
/// on every `TemplateKind::Fn` evaluation.  Exposes the captured LHS
/// [`Bindings`](crate::Bindings), the matched-root `NodeId` plus its
/// resolved output type, and a borrow on the [`Function`] under
/// rewrite so closures may read side-table state.
pub struct TemplateCtx<'a> {
    pub function: &'a Function,
    pub bindings: &'a crate::bindings::Bindings,
    /// The LHS-root `NodeId` of the rewrite that's being instantiated.
    ///
    /// Load-bearing for closures that need to inspect the matched
    /// root's *input* shape — e.g. the
    /// [`first_value_input_type`](crate::first_value_input_type)
    /// helper used by `IntCmp` constant-folding rules pulls the
    /// comparison's operand type when the root itself is `I1`-typed.
    /// Closures that only compute a constant from
    /// [`bindings`](Self::bindings) can ignore this field.
    ///
    /// As of writing this is the sole non-template-type-check
    /// consumer.  Don't extend the consumer set without first asking
    /// whether the closure could get what it needs from `bindings` +
    /// `function` alone — the more closures reach for `root`, the
    /// harder it becomes to refactor the matcher / template split
    /// later.
    pub root: NodeId,
    pub root_ty: NodeOutputType,
}
