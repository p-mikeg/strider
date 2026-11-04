use strider_ir::Function;
use strider_ir::node::{NodeId, ValueType};

/// What a [`TemplateKind::Fn`](crate::template::TemplateKind::Fn) closure reads
/// at instantiation.
pub struct TemplateCtx<'a> {
    /// The function under rewrite.
    pub function: &'a Function,
    /// The match's LHS bindings.
    pub bindings: &'a crate::bindings::Bindings,
    /// The matched LHS root, e.g. for
    /// [`first_value_input_type`](crate::first_value_input_type) under an `I1` root.
    pub root: NodeId,
    /// The resolved output type of the node being built.
    pub root_ty: ValueType,
}
