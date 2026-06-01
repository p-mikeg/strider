//! Template instantiation support.
//!
//! Minimal stub: only the [`TemplateKind`] enum that the bipartite
//! `PatNode` references for its `build` slot. The full template builder
//! + instantiation engine lands in a later change.

/// How a buildable pattern node materialises into fresh IR during
/// template instantiation.
pub enum TemplateKind {
    /// Emit a node with the given exact [`strider_ir::node::NodeKind`].
    Exact(strider_ir::node::NodeKind),
}
