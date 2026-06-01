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
    pub root: NodeId,
    pub root_ty: NodeOutputType,
}
