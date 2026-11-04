use strider_ir::Function;
use strider_ir::node::{NodeId, ValueType};

/// The input of a [`TemplateKind::Fn`](crate::template::TemplateKind::Fn) closure.
pub struct TemplateCtx<'a> {
    /// The function under rewrite.
    pub function: &'a Function,
    /// The match's LHS bindings.
    pub bindings: &'a crate::bindings::Bindings,
    /// The matched LHS root.
    pub root: NodeId,
    /// The resolved output type of the node being built.
    pub root_ty: ValueType,
}
