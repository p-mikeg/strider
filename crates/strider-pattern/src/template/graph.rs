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
/// A `TmplNode` is one of two shapes:
///
/// * **buildable** — `capture` is `None` and `kind` names the
///   `NodeKind` (or a dynamic `Fn`) to synthesise, declaring its value
///   output type via `ty`;
/// * **capture-only** — `capture` is `Some(_)`; the node resolves to its
///   LHS binding at instantiation and its `kind` / `ty` are unused.
pub struct TmplNode {
    /// How this node materialises (an exact `NodeKind` or a dynamic
    /// closure). Ignored for capture-only nodes.
    pub kind: TemplateKind,
    /// The value output type this node declares
    /// ([`TemplateTy::InheritRoot`] by default). Ignored for
    /// capture-only nodes.
    pub ty: TemplateTy,
    /// When set, the node resolves to this capture's LHS binding instead
    /// of being synthesised.
    pub capture: Option<Capture>,
}

impl TmplNode {
    /// A buildable node with the given build kind, inheriting the rewrite
    /// root's output type, with no capture.
    #[must_use]
    pub fn buildable(kind: TemplateKind) -> Self {
        Self {
            kind,
            ty: TemplateTy::InheritRoot,
            capture: None,
        }
    }
}

/// A template output vertex — one slot of a node's build signature.
///
/// On the build side the output vertices declare the materialised node's
/// output signature (value / memory / control / phi-token), so a
/// multi-output interior node (a `Store` / `Call` producing a memory
/// token a later node consumes) wires the right slot.
pub struct TmplOutput {
    /// The output slot index on the producing node.
    pub slot: usize,
    /// The kind of output this slot declares.
    pub kind: OutputKindSpec,
}

impl TmplOutput {
    /// A value output at `slot`.
    #[must_use]
    pub fn value(slot: usize) -> Self {
        Self {
            slot,
            kind: OutputKindSpec::AnyValue,
        }
    }

    /// A memory-token output at `slot`.
    #[must_use]
    pub fn memory(slot: usize) -> Self {
        Self {
            slot,
            kind: OutputKindSpec::Memory,
        }
    }

    /// A control output at `slot`.
    #[must_use]
    pub fn control(slot: usize) -> Self {
        Self {
            slot,
            kind: OutputKindSpec::Control,
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
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph: BiGraph::new(),
        }
    }

    /// The template's root node, if set.
    #[must_use]
    pub fn root(&self) -> Option<NodeIndex> {
        self.graph.root()
    }

    /// Number of node vertices.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Number of output vertices.
    #[must_use]
    pub fn output_count(&self) -> usize {
        self.graph.output_count()
    }
}
