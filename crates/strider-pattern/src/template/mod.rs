//! Template instantiation support.
//!
//! Phase 1 carries only the minimal [`TemplateKind`] enum that the
//! bipartite `PatNode` references for its `build` slot; the full
//! template builder + instantiation engine is built in a later phase.

/// How a buildable pattern node materialises into fresh IR during
/// template instantiation.
pub enum TemplateKind {
    /// Emit a node with the given exact [`strider_ir::node::NodeKind`].
    Exact(strider_ir::node::NodeKind),
}
