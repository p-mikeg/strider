//! The bipartite build-side graph: [`Template`].
//!
//! [`Template`] is the build-side counterpart of
//! [`Pattern`](crate::pattern::Pattern), instantiating the generic
//! [`BiGraph`](crate::bigraph::BiGraph) over the build payloads
//! [`TmplNode`] (the node vertex) and [`TmplValue`] (the output
//! vertex). Unlike the match side, a template carries no kindspecs /
//! limits / predicates: a node is either a [`Build`](TmplNode::Build)
//! (declaring a [`TemplateKind`] — a concrete `NodeKind` or a dynamic
//! `Fn`) or a [`Capture`](TmplNode::Capture) leaf marker whose
//! [`ValueCapture`](TmplValue::ValueCapture) output resolves through the
//! LHS [`Bindings`](crate::Bindings) at instantiation. A `Template` is
//! therefore **buildable by construction** — there is no match-only shape
//! it can represent.

use petgraph::stable_graph::NodeIndex;

use crate::bigraph::BiGraph;
use crate::capture::Capture;
use crate::pattern::OutputKindSpec;
use crate::template::{TemplateKind, TemplateTy};

/// A template **node** vertex.
///
/// A capture is split across both vertex enums: the node side is a
/// payload-less [`Capture`](Self::Capture) **marker** that only says "this
/// leaf is a capture, don't synthesise it"; the capture id and its value
/// resolution live on the produced [`TmplValue::ValueCapture`]. The
/// node side is deliberately opaque (a future node-level capture would add
/// meaning here) — for now the value side carries everything.
///
/// * [`Build`](Self::Build) — a node to synthesise as fresh IR from its
///   [`TemplateKind`] (a concrete `NodeKind` or a dynamic `Fn`).
/// * [`Capture`](Self::Capture) — a **leaf** marker; resolves through its
///   [`ValueCapture`](TmplValue::ValueCapture) output to the LHS-bound
///   value (the `add(x, 0) → x` shape). Never synthesised, never has
///   inputs.
pub enum TmplNode {
    /// A node to synthesise as fresh IR.
    Build(TemplateKind),
    /// A capture leaf marker; the capture id lives on its `ValueCapture`
    /// output.
    Capture,
}

/// A template **output** vertex — either a built output's signature or a
/// value capture.
///
/// This is the value side of the build graph. A built node produces
/// [`TmplOutput`](Self::TmplOutput) value/memory/control outputs; a
/// capture leaf produces a [`ValueCapture`](Self::ValueCapture) that
/// carries the capture id and resolves to the LHS-bound value at
/// instantiation.
pub enum TmplValue {
    /// A built output's signature (slot + kind + type).
    TmplOutput(TmplOutput),
    /// A value capture: resolves to `bindings.get_value(c)`.
    ValueCapture(Capture),
}

/// A built output's signature — one slot of a built node's output
/// signature.
///
/// On the build side these declare the materialised node's output
/// signature (value / memory / control / phi-token), so a multi-output
/// interior node (a `Store` / `Call` producing a memory token a later node
/// consumes) wires the right slot. Value outputs also carry the build
/// [`TemplateTy`] (inherit-root or fixed) resolved at instantiation;
/// memory / control / phi-token outputs ignore it.
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
/// [`TmplNode`] / [`TmplValue`] vertices, materialised by
/// [`instantiate`](crate::template::instantiate).
pub struct Template {
    pub(crate) graph: BiGraph<TmplNode, TmplValue>,
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

    /// Number of node vertices. Test-only structural accessor.
    #[cfg(test)]
    pub(crate) fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Number of output vertices. Test-only structural accessor.
    #[cfg(test)]
    pub(crate) fn output_count(&self) -> usize {
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

    /// `bool_not(var(c))` seals into `xor(var(c), IntConst(1)):I1` — the
    /// xor, its const operand, and the captured var node: three node
    /// vertices.
    #[test]
    fn bool_not_template_builds_three_node_graph() {
        use crate::{Capture, TemplatePat, var};
        let c = Capture::new();
        let tpl = crate::template::bool_not(var(c)).into_template();
        assert!(tpl.root().is_ok(), "sealed template must have a root");
        assert_eq!(tpl.node_count(), 3);
    }
}
