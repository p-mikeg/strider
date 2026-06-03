//! The bipartite build-side graph: [`Template`].
//!
//! [`Template`] is the build-side counterpart of
//! [`Pattern`](crate::pattern::Pattern), instantiating the generic
//! [`BiGraph`](crate::bigraph::BiGraph) over the build payloads
//! [`TmplNode`] (a node to materialise) and [`TmplOutput`] (the node's
//! build-signature output). Unlike the match side, a template carries no
//! kindspecs / limits / predicates: every node either declares a
//! [`TemplateKind`] (a concrete `NodeKind` or a dynamic `Fn`) or is
//! capture-only (resolved through the LHS [`Bindings`](crate::Bindings)
//! at instantiation). A `Template` is therefore **buildable by
//! construction** — there is no match-only shape it can represent.

use petgraph::stable_graph::NodeIndex;

use crate::bigraph::BiGraph;
use crate::capture::Capture;
use crate::pattern::OutputKindSpec;
use crate::template::{TemplateKind, TemplateTy};

/// A template node vertex — a node to materialise as fresh IR.
///
/// A `TmplNode` carries **node** data only (mirroring the match side's
/// `PatNode`); the value output *type* lives on the produced
/// [`TmplOutput`]. A `TmplNode` is one of two shapes:
///
/// * **buildable** — `capture` is `None` and `kind` names the
///   `NodeKind` (or a dynamic `Fn`) to synthesise;
/// * **capture-only** — `capture` is `Some(_)`; the node resolves to its
///   LHS binding at instantiation and its `kind` is unused.
pub struct TmplNode {
    /// How this node materialises (an exact `NodeKind` or a dynamic
    /// closure). Ignored for capture-only nodes.
    pub kind: TemplateKind,
    /// When set, the node resolves to this capture's LHS binding instead
    /// of being synthesised.
    pub capture: Option<Capture>,
}

impl TmplNode {
    /// A buildable node with the given build kind and no capture.
    pub fn buildable(kind: TemplateKind) -> Self {
        Self {
            kind,
            capture: None,
        }
    }
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
