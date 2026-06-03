//! The bipartite build-side graph: [`Template`].
//!
//! [`Template`] is the build-side counterpart of
//! [`Pattern`](crate::pattern::Pattern), instantiating the generic
//! [`BiGraph`](crate::bigraph::BiGraph) over the build payloads
//! [`TmplNode`] (a node to materialise) and [`TmplOutput`] (the node's
//! build-signature output). Unlike the match side, a template carries no
//! kindspecs / limits / predicates: a node is either a
//! [`Build`](TmplNode::Build) (declaring a [`TemplateKind`] — a concrete
//! `NodeKind` or a dynamic `Fn`) or a [`Capture`](TmplNode::Capture) leaf
//! (resolved through the LHS [`Bindings`](crate::Bindings) at
//! instantiation). A `Template` is therefore **buildable by
//! construction** — there is no match-only shape it can represent.

use petgraph::stable_graph::NodeIndex;

use crate::bigraph::BiGraph;
use crate::capture::Capture;
use crate::pattern::OutputKindSpec;
use crate::template::{TemplateKind, TemplateTy};

/// A template node vertex — distinct **node types** in the build graph.
///
/// A `TmplNode` carries **node** data only (mirroring the match side's
/// `PatNode`); the value output *type* lives on the produced
/// [`TmplOutput`]. A capture is its own node type, not a flag on a build
/// node, and is always a leaf:
///
/// * [`Build`](Self::Build) — a node to synthesise as fresh IR from its
///   [`TemplateKind`] (a concrete `NodeKind` or a dynamic `Fn`).
/// * [`Capture`](Self::Capture) — a **leaf** that resolves to the LHS
///   binding for the given [`Capture`], re-using the captured value
///   verbatim (the `add(x, 0) → x` shape). Never synthesised, never has
///   inputs.
pub enum TmplNode {
    /// A node to synthesise as fresh IR.
    Build(TemplateKind),
    /// A capture leaf: resolves to the LHS-bound value for this capture.
    Capture(Capture),
}

/// A template output vertex — one slot of a node's build signature.
///
/// On the build side the output vertices declare the materialised node's
/// output signature (value / memory / control / phi-token), so a
/// multi-output interior node (a `Store` / `Call` producing a memory
/// token a later node consumes) wires the right slot. Value outputs also
/// carry the build [`TemplateTy`] (inherit-root or fixed) resolved at
/// instantiation; memory / control / phi-token outputs ignore it.
pub struct TmplOutput {
    /// The output slot index on the producing node.
    pub slot: usize,
    /// The kind of output this slot declares.
    pub kind: OutputKindSpec,
    /// The value output type this slot declares
    /// ([`TemplateTy::InheritRoot`] by default). Meaningful only for
    /// value outputs.
    pub ty: TemplateTy,
}

impl TmplOutput {
    /// A value output at `slot`, inheriting the rewrite root's type.
    pub fn value(slot: usize) -> Self {
        Self {
            slot,
            kind: OutputKindSpec::AnyValue,
            ty: TemplateTy::InheritRoot,
        }
    }

    /// A memory-token output at `slot`.
    pub fn memory(slot: usize) -> Self {
        Self {
            slot,
            kind: OutputKindSpec::Memory,
            ty: TemplateTy::InheritRoot,
        }
    }

    /// A control output at `slot`.
    pub fn control(slot: usize) -> Self {
        Self {
            slot,
            kind: OutputKindSpec::Control,
            ty: TemplateTy::InheritRoot,
        }
    }
}

/// A build-side template over the IR: a bipartite [`BiGraph`] of
/// [`TmplNode`] / [`TmplOutput`] vertices, materialised by
/// [`instantiate`](crate::template::instantiate).
pub struct Template {
    pub(crate) graph: BiGraph<TmplNode, TmplOutput>,
}

impl Default for Template {
    fn default() -> Self {
        Self::new()
    }
}

impl Template {
    /// An empty template with no root.
    pub fn new() -> Self {
        Self {
            graph: BiGraph::new(),
        }
    }

    /// The template's build root — the unique graph sink, recovered
    /// structurally.
    ///
    /// # Errors
    /// Errors if the template is not a single-rooted graph: zero sinks
    /// (rootless / cyclic) or more than one (multi-rooted).
    pub fn root(&self) -> anyhow::Result<NodeIndex> {
        self.graph.derive_root()
    }

    /// Number of node vertices.
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Number of output vertices.
    pub fn output_count(&self) -> usize {
        self.graph.output_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_vertex_carries_build_type_not_node() {
        // The value-output *type* is data about the value, so it lives on
        // the output vertex, mirroring the match side where `PatValue`
        // carries width/type and `PatNode` carries none.
        let o = TmplOutput::value(0);
        assert!(matches!(o.ty, TemplateTy::InheritRoot));
    }
}
