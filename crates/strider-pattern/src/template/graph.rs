//! Unlike the match side, a template carries no kindspecs or predicates. A
//! node is either a [`Build`](TmplNodeKind::Build) declaring a
//! [`TemplateKind`], or a [`Capture`](TmplNodeKind::Capture) leaf marker
//! whose [`ValueCapture`](TmplValue::ValueCapture) output resolves through
//! the LHS [`Bindings`](crate::Bindings) at instantiation. A `Template` is
//! therefore buildable by construction: it can represent no match-only
//! shape.

use strider_graph::{Graph, NeverCacheable, NodeId};

use crate::capture::Capture;
use crate::graph_ext::{HasInputSlots, PatGraphRead};
use crate::matcher::OutputKindSpec;
use crate::template::{TemplateKind, TemplateTy};

/// A capture is split across both vertex enums. The node side is a
/// payload-less marker saying only "this leaf is a capture, do not synthesise
/// it"; the capture id and its value resolution live on the produced
/// [`TmplValue::ValueCapture`].
pub struct TmplNode {
    pub kind: TmplNodeKind,
    /// Parallel to the generic graph's input order; see `graph_ext` for why
    /// the slot rides on the node payload.
    pub input_slots: Vec<usize>,
}

impl HasInputSlots for TmplNode {
    fn input_slots(&self) -> &[usize] {
        &self.input_slots
    }
}

pub enum TmplNodeKind {
    /// Synthesised as fresh IR from a concrete `NodeKind` or a dynamic `Fn`.
    Build(TemplateKind),
    /// A leaf resolving through its
    /// [`ValueCapture`](TmplValue::ValueCapture) output to the LHS-bound
    /// value, as in `add(x, 0) -> x`. Never synthesised, never has inputs.
    Capture,
}

/// The value side of the build graph.
pub enum TmplValue {
    /// A built node's output: slot, kind and type.
    TmplOutput(TmplOutput),
    /// Resolves to `bindings.get_value(c)` at instantiation.
    ValueCapture(Capture),
}

/// One slot of a materialised node's output signature. Declaring the kind
/// (value / memory / control / phi-token) is what lets a multi-output interior
/// node, such as a `Store` or `Call` whose memory token a later node consumes,
/// wire the right slot.
pub struct TmplOutput {
    pub slot: usize,
    pub kind: OutputKindSpec,
    /// Resolved at instantiation, and meaningful only for value outputs;
    /// memory / control / phi-token slots ignore it.
    pub ty: TemplateTy,
}

impl TmplOutput {
    /// Inherits the rewrite root's type.
    pub fn value(slot: usize) -> Self {
        Self {
            slot,
            kind: OutputKindSpec::AnyValue,
            ty: TemplateTy::InheritRoot,
        }
    }

    pub fn memory(slot: usize) -> Self {
        Self {
            slot,
            kind: OutputKindSpec::Memory,
            ty: TemplateTy::InheritRoot,
        }
    }
}

/// Materialised by [`instantiate`](crate::template::instantiate).
pub struct Template {
    pub(crate) graph: Graph<TmplNode, TmplValue, NeverCacheable>,
}

impl Template {
    /// For `TemplateBuilder::finish`, once the staging core has sealed the
    /// staged DAG.
    pub(crate) fn from_graph(graph: Graph<TmplNode, TmplValue, NeverCacheable>) -> Self {
        Self { graph }
    }

    /// The build root is the unique sink, recovered structurally.
    ///
    /// # Errors
    /// Unless there is exactly one sink: zero means rootless or cyclic, more
    /// than one means multi-rooted.
    pub fn root(&self) -> anyhow::Result<NodeId> {
        self.graph.derive_root()
    }

    /// Drives the rewrite engine's construction-time coverage check, which
    /// confirms the LHS binds every capture the RHS references.
    pub fn referenced_captures(&self) -> impl Iterator<Item = Capture> + '_ {
        self.graph
            .all_value_ids()
            .filter_map(|v| match self.graph.value_kind_ref(v) {
                TmplValue::ValueCapture(cap) => Some(*cap),
                TmplValue::TmplOutput(_) => None,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_vertex_carries_build_type_not_node() {
        // The type is data about the value, so it rides the output vertex,
        // mirroring `PatValue` on the match side.
        let o = TmplOutput::value(0);
        assert!(matches!(o.ty, TemplateTy::InheritRoot));
    }

    /// `bool_not(var(c))` seals into `xor(var(c), IntConst(1)):I1`: the xor,
    /// its const operand and the captured var, so three node vertices.
    #[test]
    fn bool_not_template_builds_three_node_graph() {
        use crate::{Capture, TemplatePat, var};
        let c = Capture::new();
        let tpl = crate::template::bool_not(var(c)).into_template();
        assert!(tpl.root().is_ok(), "sealed template must have a root");
        assert_eq!(tpl.graph.all_node_ids().count(), 3);
    }
}
